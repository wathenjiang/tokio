use super::Synced;
use super::Synced2;

use crate::runtime::task;

use std::marker::PhantomData;

pub(crate) struct Pop<'a, T: 'static> {
    len: usize,
    synced: &'a mut Synced,
    _p: PhantomData<T>,
}

impl<'a, T: 'static> Pop<'a, T> {
    pub(super) fn new(len: usize, synced: &'a mut Synced) -> Pop<'a, T> {
        Pop {
            len,
            synced,
            _p: PhantomData,
        }
    }
}

impl<'a, T: 'static> Iterator for Pop<'a, T> {
    type Item = task::Notified<T>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.len == 0 {
            return None;
        }

        let ret = self.synced.pop();

        // Should be `Some` when `len > 0`
        debug_assert!(ret.is_some());

        self.len -= 1;
        ret
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.len, Some(self.len))
    }
}

impl<'a, T: 'static> ExactSizeIterator for Pop<'a, T> {
    fn len(&self) -> usize {
        self.len
    }
}

impl<'a, T: 'static> Drop for Pop<'a, T> {
    fn drop(&mut self) {
        for _ in self.by_ref() {}
    }
}



pub(crate) struct Pop2<'a, T: 'static> {
    len: usize,
    synced2: &'a mut Synced2,
    _p: PhantomData<T>,
}

impl<'a, T: 'static> Pop2<'a, T> {
    pub(super) fn new(len: usize, synced2: &'a mut Synced2) -> Pop2<'a, T> {
        Pop2 {
            len,
            synced2,
            _p: PhantomData,
        }
    }
}

impl<'a, T: 'static> Iterator for Pop2<'a, T> {
    type Item = task::Notified<T>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.len == 0 {
            return None;
        }

        let ret = self.synced2.pop();

        // Should be `Some` when `len > 0`
        debug_assert!(ret.is_some());

        self.len -= 1;
        ret
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.len, Some(self.len))
    }
}

impl<'a, T: 'static> ExactSizeIterator for Pop2<'a, T> {
    fn len(&self) -> usize {
        self.len
    }
}

impl<'a, T: 'static> Drop for Pop2<'a, T> {
    fn drop(&mut self) {
        for _ in self.by_ref() {}
    }
}