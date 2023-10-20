#![cfg_attr(not(feature = "full"), allow(dead_code))]

//! An intrusive double linked list of data.
//!
//! The data structure supports tracking pinned nodes. Most of the data
//! structure's APIs are `unsafe` as they require the caller to ensure the
//! specified node is actually contained by the list.

use core::cell::UnsafeCell;
use core::fmt;
use core::marker::{PhantomData, PhantomPinned};
use core::mem::ManuallyDrop;
use core::ptr::{self, NonNull};

use crate::loom::sync::Mutex;

/// An intrusive linked list.
///
/// Currently, the list is not emptied on drop. It is the caller's
/// responsibility to ensure the list is empty before dropping it.
pub(crate) struct LinkedList2<L, T> {
    head_mutex: Mutex<()>,
    rm_mutex: Mutex<()>,

    /// Linked list head
    head: UnsafeCell<Option<NonNull<T>>>,

    /// Node type marker.
    _marker: PhantomData<*const L>,
}

unsafe impl<L: Link> Send for LinkedList2<L, L::Target> where L::Target: Send {}
unsafe impl<L: Link> Sync for LinkedList2<L, L::Target> where L::Target: Sync {}

/// Defines how a type is tracked within a linked list.
///
/// In order to support storing a single type within multiple lists, accessing
/// the list pointers is decoupled from the entry type.
///
/// # Safety
///
/// Implementations must guarantee that `Target` types are pinned in memory. In
/// other words, when a node is inserted, the value will not be moved as long as
/// it is stored in the list.
pub(crate) unsafe trait Link {
    /// Handle to the list entry.
    ///
    /// This is usually a pointer-ish type.
    type Handle;

    /// Node type.
    type Target;

    /// Convert the handle to a raw pointer without consuming the handle.
    #[allow(clippy::wrong_self_convention)]
    fn as_raw(handle: &Self::Handle) -> NonNull<Self::Target>;

    /// Convert the raw pointer to a handle
    unsafe fn from_raw(ptr: NonNull<Self::Target>) -> Self::Handle;

    /// Return the pointers for a node
    ///
    /// # Safety
    ///
    /// The resulting pointer should have the same tag in the stacked-borrows
    /// stack as the argument. In particular, the method may not create an
    /// intermediate reference in the process of creating the resulting raw
    /// pointer.
    unsafe fn pointers(target: NonNull<Self::Target>) -> NonNull<Pointers<Self::Target>>;
}

/// Previous / next pointers.
pub(crate) struct Pointers<T> {
    inner: UnsafeCell<PointersInner<T>>,
}
/// We do not want the compiler to put the `noalias` attribute on mutable
/// references to this type, so the type has been made `!Unpin` with a
/// `PhantomPinned` field.
///
/// Additionally, we never access the `prev` or `next` fields directly, as any
/// such access would implicitly involve the creation of a reference to the
/// field, which we want to avoid since the fields are not `!Unpin`, and would
/// hence be given the `noalias` attribute if we were to do such an access.
/// As an alternative to accessing the fields directly, the `Pointers` type
/// provides getters and setters for the two fields, and those are implemented
/// using raw pointer casts and offsets, which is valid since the struct is
/// #[repr(C)].
///
/// See this link for more information:
/// <https://github.com/rust-lang/rust/pull/82834>
#[repr(C)]
struct PointersInner<T> {
    /// The previous node in the list. null if there is no previous node.
    ///
    /// This field is accessed through pointer manipulation, so it is not dead code.
    #[allow(dead_code)]
    prev: Option<NonNull<T>>,

    /// The next node in the list. null if there is no previous node.
    ///
    /// This field is accessed through pointer manipulation, so it is not dead code.
    #[allow(dead_code)]
    next: Option<NonNull<T>>,

    /// This type is !Unpin due to the heuristic from:
    /// <https://github.com/rust-lang/rust/pull/82834>
    _pin: PhantomPinned,
}

unsafe impl<T: Send> Send for Pointers<T> {}
unsafe impl<T: Sync> Sync for Pointers<T> {}

// ===== impl LinkedList =====

impl<L, T> LinkedList2<L, T> {
    /// Creates an empty linked list.
    pub(crate) const fn new() -> LinkedList2<L, T> {
        LinkedList2 {
            head_mutex: Mutex::new(()),
            rm_mutex: Mutex::new(()),
            head: UnsafeCell::new(None),
            _marker: PhantomData,
        }
    }
}

impl<L: Link> LinkedList2<L, L::Target> {
    /// Adds an element first in the list.
    pub(crate) fn push_front(&self, val: L::Handle) {
        let _lock = self.head_mutex.lock();

        // The value should not be dropped, it is being inserted into the list
        let val = ManuallyDrop::new(val);
        let ptr = L::as_raw(&val);
        assert_ne!(unsafe { *self.head.get() }, Some(ptr));
        unsafe {
            L::pointers(ptr).as_mut().set_next(*self.head.get());
            L::pointers(ptr).as_mut().set_prev(None);

            if let Some(head) = *self.head.get() {
                L::pointers(head).as_mut().set_prev(Some(ptr));
            }

            *self.head.get() = Some(ptr);
        }
    }

    /// Removes the last element from a list and returns it, or None if it is
    /// empty.
    pub(crate) fn pop_front(&self) -> Option<L::Handle> {
        let _head_lock = self.head_mutex.lock();
        let _rm_lock = self.rm_mutex.lock();
        unsafe {
            let first = (*self.head.get())?;
            *self.head.get() = L::pointers(first).as_ref().get_next();

            if let Some(next) = L::pointers(first).as_ref().get_next() {
                L::pointers(next).as_mut().set_prev(None);
            } else {
                *self.head.get() = None;
            }

            L::pointers(first).as_mut().set_prev(None);
            L::pointers(first).as_mut().set_next(None);

            Some(L::from_raw(first))
        }
    }

    /// Returns whether the linked list does not contain any node
    pub(crate) fn is_empty(&self) -> bool {
        let _head_lock = self.head_mutex.lock();
        let _remove_lock = self.rm_mutex.lock();
        if (unsafe { *self.head.get() }).is_some() {
            return false;
        }
        true
    }

    /// Removes the specified node from the list
    ///
    /// # Safety
    ///
    /// The caller **must** ensure that exactly one of the following is true:
    /// - `node` is currently contained by `self`,
    /// - `node` is not contained by any list,
    /// - `node` is currently contained by some other `GuardedLinkedList` **and**
    ///   the caller has an exclusive access to that list. This condition is
    ///   used by the linked list in `sync::Notify`.
    pub(crate) unsafe fn remove(&self, node: NonNull<L::Target>) -> Option<L::Handle> {
        let remove_lock = self.rm_mutex.lock();
        // 不是头节点，那么无需锁进行删除
        let head_lock = {
            if *self.head.get() != Some(node) {
                None
            } else {
                Some(self.head_mutex.lock())
            }
        };

        // 检查要删除的节点是否有前驱节点（prev）。如果有，我们将前驱节点的 next 指针设置为要删除节点的 next 指针。
        // 这样，前驱节点将直接指向要删除节点的后继节点。
        if let Some(prev) = L::pointers(node).as_ref().get_prev() {
            debug_assert_eq!(L::pointers(prev).as_ref().get_next(), Some(node));
            L::pointers(prev)
                .as_mut()
                .set_next(L::pointers(node).as_ref().get_next());
        } else {
            // 如果要删除的节点没有前驱节点，那么它应该是链表的头节点。在这种情况下，我们将链表头设置为要删除节点的后继节点。
            // 如果要删除的节点不是链表头节点，那么返回 None，表示节点未被删除。
            if head_lock.is_none() {
                return None;
            }
            *self.head.get() = L::pointers(node).as_ref().get_next();
        }
        // 接下来，我们检查要删除的节点是否有后继节点（next）。如果有，
        // 我们将后继节点的 prev 指针设置为要删除节点的 prev 指针。这样，后继节点将直接指向要删除节点的前驱节点。
        if let Some(next) = L::pointers(node).as_ref().get_next() {
            debug_assert_eq!(L::pointers(next).as_ref().get_prev(), Some(node));
            L::pointers(next)
                .as_mut()
                .set_prev(L::pointers(node).as_ref().get_prev());
        }

        drop(remove_lock);
        drop(head_lock);
        // 我们将要删除节点的 prev 和 next 指针设置为 None，表示节点已从链表中删除。然后，我们返回已删除节点的句柄。
        L::pointers(node).as_mut().set_next(None);
        L::pointers(node).as_mut().set_prev(None);

        Some(L::from_raw(node))
    }
}

impl<L: Link> fmt::Debug for LinkedList2<L, L::Target> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LinkedList2")
            .field("head", &self.head)
            .finish()
    }
}

impl<L: Link> Default for LinkedList2<L, L::Target> {
    fn default() -> Self {
        Self::new()
    }
}

// ===== impl DrainFilter =====

cfg_io_driver_impl! {
    pub(crate) struct DrainFilter<'a, T: Link, F> {
        list: &'a mut LinkedList2<T, T::Target>,
        filter: F,
        curr: Option<NonNull<T::Target>>,
    }

    impl<'a, T, F> Iterator for DrainFilter<'a, T, F>
    where
        T: Link,
        F: FnMut(&T::Target) -> bool,
    {
        type Item = T::Handle;

        fn next(&mut self) -> Option<Self::Item> {
            while let Some(curr) = self.curr {
                // safety: the pointer references data contained by the list
                self.curr = unsafe { T::pointers(curr).as_ref() }.get_next();

                // safety: the value is still owned by the linked list.
                if (self.filter)(unsafe { &mut *curr.as_ptr() }) {
                    return unsafe { self.list.remove(curr) };
                }
            }

            None
        }
    }
}

cfg_taskdump! {
    impl<T: Link> LinkedList2<T, T::Target> {
        pub(crate) fn for_each<F>(&mut self, f: &mut F)
        where
            F: FnMut(&T::Handle),
        {
            let mut next = self.head;

            while let Some(curr) = next {
                unsafe {
                    let handle = ManuallyDrop::new(T::from_raw(curr));
                    f(&handle);
                    next = T::pointers(curr).as_ref().get_next();
                }
            }
        }
    }
}

// ===== impl Pointers =====

impl<T> Pointers<T> {
    /// Create a new set of empty pointers
    pub(crate) fn new() -> Pointers<T> {
        Pointers {
            inner: UnsafeCell::new(PointersInner {
                prev: None,
                next: None,
                _pin: PhantomPinned,
            }),
        }
    }

    pub(crate) fn get_prev(&self) -> Option<NonNull<T>> {
        // SAFETY: prev is the first field in PointersInner, which is #[repr(C)].
        unsafe {
            let inner = self.inner.get();
            let prev = inner as *const Option<NonNull<T>>;
            ptr::read(prev)
        }
    }
    pub(crate) fn get_next(&self) -> Option<NonNull<T>> {
        // SAFETY: next is the second field in PointersInner, which is #[repr(C)].
        unsafe {
            let inner = self.inner.get();
            let prev = inner as *const Option<NonNull<T>>;
            let next = prev.add(1);
            ptr::read(next)
        }
    }

    fn set_prev(&mut self, value: Option<NonNull<T>>) {
        // SAFETY: prev is the first field in PointersInner, which is #[repr(C)].
        unsafe {
            let inner = self.inner.get();
            let prev = inner as *mut Option<NonNull<T>>;
            ptr::write(prev, value);
        }
    }
    fn set_next(&mut self, value: Option<NonNull<T>>) {
        // SAFETY: next is the second field in PointersInner, which is #[repr(C)].
        unsafe {
            let inner = self.inner.get();
            let prev = inner as *mut Option<NonNull<T>>;
            let next = prev.add(1);
            ptr::write(next, value);
        }
    }
}

impl<T> fmt::Debug for Pointers<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let prev = self.get_prev();
        let next = self.get_next();
        f.debug_struct("Pointers")
            .field("prev", &prev)
            .field("next", &next)
            .finish()
    }
}

#[cfg(any(test, fuzzing))]
#[cfg(not(loom))]
pub(crate) mod tests {
    use super::*;

    use std::pin::Pin;

    #[derive(Debug)]
    #[repr(C)]
    struct Entry {
        pointers: Pointers<Entry>,
        val: i32,
    }

    unsafe impl<'a> Link for &'a Entry {
        type Handle = Pin<&'a Entry>;
        type Target = Entry;

        fn as_raw(handle: &Pin<&'_ Entry>) -> NonNull<Entry> {
            NonNull::from(handle.get_ref())
        }

        unsafe fn from_raw(ptr: NonNull<Entry>) -> Pin<&'a Entry> {
            Pin::new_unchecked(&*ptr.as_ptr())
        }

        unsafe fn pointers(target: NonNull<Entry>) -> NonNull<Pointers<Entry>> {
            target.cast()
        }
    }

    fn entry(val: i32) -> Pin<Box<Entry>> {
        Box::pin(Entry {
            pointers: Pointers::new(),
            val,
        })
    }

    fn ptr(r: &Pin<Box<Entry>>) -> NonNull<Entry> {
        r.as_ref().get_ref().into()
    }

    fn collect_list(list: &mut LinkedList2<&'_ Entry, <&'_ Entry as Link>::Target>) -> Vec<i32> {
        let mut ret = vec![];

        while let Some(entry) = list.pop_back() {
            ret.push(entry.val);
        }

        ret
    }

    fn push_all<'a>(
        list: &mut LinkedList2<&'a Entry, <&'_ Entry as Link>::Target>,
        entries: &[Pin<&'a Entry>],
    ) {
        for entry in entries.iter() {
            list.push_front(*entry);
        }
    }

    #[cfg(test)]
    macro_rules! assert_clean {
        ($e:ident) => {{
            assert!($e.pointers.get_next().is_none());
            assert!($e.pointers.get_prev().is_none());
        }};
    }

    #[cfg(test)]
    macro_rules! assert_ptr_eq {
        ($a:expr, $b:expr) => {{
            // Deal with mapping a Pin<&mut T> -> Option<NonNull<T>>
            assert_eq!(Some($a.as_ref().get_ref().into()), $b)
        }};
    }

    #[test]
    fn const_new() {
        const _: LinkedList2<&Entry, <&Entry as Link>::Target> = LinkedList2::new();
    }

    #[test]
    fn push_and_drain() {
        let a = entry(5);
        let b = entry(7);
        let c = entry(31);

        let mut list = LinkedList2::new();
        assert!(list.is_empty());

        list.push_front(a.as_ref());
        assert!(!list.is_empty());
        list.push_front(b.as_ref());
        list.push_front(c.as_ref());

        let items: Vec<i32> = collect_list(&mut list);
        assert_eq!([5, 7, 31].to_vec(), items);

        assert!(list.is_empty());
    }

    #[test]
    fn push_pop_push_pop() {
        let a = entry(5);
        let b = entry(7);

        let mut list = LinkedList2::<&Entry, <&Entry as Link>::Target>::new();

        list.push_front(a.as_ref());

        let entry = list.pop_back().unwrap();
        assert_eq!(5, entry.val);
        assert!(list.is_empty());

        list.push_front(b.as_ref());

        let entry = list.pop_back().unwrap();
        assert_eq!(7, entry.val);

        assert!(list.is_empty());
        assert!(list.pop_back().is_none());
    }

    #[test]
    fn remove_by_address() {
        let a = entry(5);
        let b = entry(7);
        let c = entry(31);

        unsafe {
            // Remove first
            let mut list = LinkedList2::new();

            push_all(&mut list, &[c.as_ref(), b.as_ref(), a.as_ref()]);
            assert!(list.remove(ptr(&a)).is_some());
            assert_clean!(a);
            // `a` should be no longer there and can't be removed twice
            assert!(list.remove(ptr(&a)).is_none());
            assert!(!list.is_empty());

            assert!(list.remove(ptr(&b)).is_some());
            assert_clean!(b);
            // `b` should be no longer there and can't be removed twice
            assert!(list.remove(ptr(&b)).is_none());
            assert!(!list.is_empty());

            assert!(list.remove(ptr(&c)).is_some());
            assert_clean!(c);
            // `b` should be no longer there and can't be removed twice
            assert!(list.remove(ptr(&c)).is_none());
            assert!(list.is_empty());
        }

        unsafe {
            // Remove middle
            let mut list = LinkedList2::new();

            push_all(&mut list, &[c.as_ref(), b.as_ref(), a.as_ref()]);

            assert!(list.remove(ptr(&a)).is_some());
            assert_clean!(a);

            assert_ptr_eq!(b, *list.head.get());
            assert_ptr_eq!(c, b.pointers.get_next());
            assert_ptr_eq!(b, c.pointers.get_prev());

            let items = collect_list(&mut list);
            assert_eq!([31, 7].to_vec(), items);
        }

        unsafe {
            // Remove middle
            let mut list = LinkedList2::new();

            push_all(&mut list, &[c.as_ref(), b.as_ref(), a.as_ref()]);

            assert!(list.remove(ptr(&b)).is_some());
            assert_clean!(b);

            assert_ptr_eq!(c, a.pointers.get_next());
            assert_ptr_eq!(a, c.pointers.get_prev());

            let items = collect_list(&mut list);
            assert_eq!([31, 5].to_vec(), items);
        }

        unsafe {
            // Remove last
            // Remove middle
            let mut list = LinkedList2::new();

            push_all(&mut list, &[c.as_ref(), b.as_ref(), a.as_ref()]);

            assert!(list.remove(ptr(&c)).is_some());
            assert_clean!(c);

            assert!(b.pointers.get_next().is_none());
            assert_ptr_eq!(b, list.tail);

            let items = collect_list(&mut list);
            assert_eq!([7, 5].to_vec(), items);
        }

        unsafe {
            // Remove first of two
            let mut list = LinkedList2::new();

            push_all(&mut list, &[b.as_ref(), a.as_ref()]);

            assert!(list.remove(ptr(&a)).is_some());

            assert_clean!(a);

            // a should be no longer there and can't be removed twice
            assert!(list.remove(ptr(&a)).is_none());

            assert_ptr_eq!(b, *list.head.get());
            assert_ptr_eq!(b, list.tail);

            assert!(b.pointers.get_next().is_none());
            assert!(b.pointers.get_prev().is_none());

            let items = collect_list(&mut list);
            assert_eq!([7].to_vec(), items);
        }

        unsafe {
            // Remove last of two
            let mut list = LinkedList2::new();

            push_all(&mut list, &[b.as_ref(), a.as_ref()]);

            assert!(list.remove(ptr(&b)).is_some());

            assert_clean!(b);

            assert_ptr_eq!(a, *list.head.get());
            assert_ptr_eq!(a, list.tail);

            assert!(a.pointers.get_next().is_none());
            assert!(a.pointers.get_prev().is_none());

            let items = collect_list(&mut list);
            assert_eq!([5].to_vec(), items);
        }

        unsafe {
            // Remove last item
            let mut list = LinkedList2::new();

            push_all(&mut list, &[a.as_ref()]);

            assert!(list.remove(ptr(&a)).is_some());
            assert_clean!(a);

            assert!(list.head.is_none());
            assert!(list.tail.is_none());
            let items = collect_list(&mut list);
            assert!(items.is_empty());
        }

        unsafe {
            // Remove missing
            let mut list = LinkedList2::<&Entry, <&Entry as Link>::Target>::new();

            list.push_front(b.as_ref());
            list.push_front(a.as_ref());

            assert!(list.remove(ptr(&c)).is_none());
        }
    }

    /// This is a fuzz test. You run it by entering `cargo fuzz run fuzz_linked_list` in CLI in `/tokio/` module.
    #[cfg(fuzzing)]
    pub fn fuzz_linked_list(ops: &[u8]) {
        enum Op {
            Push,
            Pop,
            Remove(usize),
        }
        use std::collections::VecDeque;

        let ops = ops
            .iter()
            .map(|i| match i % 3u8 {
                0 => Op::Push,
                1 => Op::Pop,
                2 => Op::Remove((i / 3u8) as usize),
                _ => unreachable!(),
            })
            .collect::<Vec<_>>();

        let mut ll = LinkedList2::<&Entry, <&Entry as Link>::Target>::new();
        let mut reference = VecDeque::new();

        let entries: Vec<_> = (0..ops.len()).map(|i| entry(i as i32)).collect();

        for (i, op) in ops.iter().enumerate() {
            match op {
                Op::Push => {
                    reference.push_front(i as i32);
                    assert_eq!(entries[i].val, i as i32);

                    ll.push_front(entries[i].as_ref());
                }
                Op::Pop => {
                    if reference.is_empty() {
                        assert!(ll.is_empty());
                        continue;
                    }

                    let v = reference.pop_back();
                    assert_eq!(v, ll.pop_back().map(|v| v.val));
                }
                Op::Remove(n) => {
                    if reference.is_empty() {
                        assert!(ll.is_empty());
                        continue;
                    }

                    let idx = n % reference.len();
                    let expect = reference.remove(idx).unwrap();

                    unsafe {
                        let entry = ll.remove(ptr(&entries[expect as usize])).unwrap();
                        assert_eq!(expect, entry.val);
                    }
                }
            }
        }
    }
}
