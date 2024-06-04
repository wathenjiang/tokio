use crate::runtime::time::{TimerHandle, TimerShared};
use crate::time::error::InsertError;

mod level;
pub(crate) use self::level::Expiration;
use self::level::Level;

use std::{array, ptr::NonNull};

use super::EntryList;

/// Timing wheel implementation.
///
/// This type provides the hashed timing wheel implementation that backs `Timer`
/// and `DelayQueue`.
///
/// The structure is generic over `T: Stack`. This allows handling timeout data
/// being stored on the heap or in a slab. In order to support the latter case,
/// the slab must be passed into each function allowing the implementation to
/// lookup timer entries.
///
/// See `Timer` documentation for some implementation notes.
#[derive(Debug)]
pub(crate) struct Wheel {
    /// The number of milliseconds elapsed since the wheel started.
    elapsed: u64,

    /// Timer wheel.
    ///
    /// Levels:
    ///
    /// * 1 ms slots / 64 ms range
    /// * 64 ms slots / ~ 4 sec range
    /// * ~ 4 sec slots / ~ 4 min range
    /// * ~ 4 min slots / ~ 4 hr range
    /// * ~ 4 hr slots / ~ 12 day range
    /// * ~ 12 day slots / ~ 2 yr range
    levels: Box<[Level; NUM_LEVELS]>,

    /// Overflows: We store the large Entries here.
    overflow: EntryList,
    // The number of Entries in overflow.
    overflow_count: u64,
    overflow_round: u32,
}

/// Number of levels. Each level has 64 slots. By using 6 levels with 64 slots
/// each, the timer is able to track time up to 2 years into the future with a
/// precision of 1 millisecond.
const NUM_LEVELS: usize = 6;

pub(super) const OVERFLOW_DURATION: u64 = 1 << (6 * NUM_LEVELS);

/// The maximum duration of a `Sleep`.
pub(super) const MAX_DURATION: u64 = OVERFLOW_DURATION - 1;

impl Wheel {
    /// Creates a new timing wheel.
    pub(crate) fn new() -> Wheel {
        Wheel {
            elapsed: 0,
            levels: Box::new(array::from_fn(Level::new)),
            overflow: EntryList::new(),
            overflow_count: 0,
            overflow_round: 1,
        }
    }

    /// Returns the number of milliseconds that have elapsed since the timing
    /// wheel's creation.
    pub(crate) fn elapsed(&self) -> u64 {
        self.elapsed
    }

    /// Inserts an entry into the timing wheel.
    ///
    /// # Arguments
    ///
    /// * `item`: The item to insert into the wheel.
    ///
    /// # Return
    ///
    /// Returns `Ok` when the item is successfully inserted, `Err` otherwise.
    ///
    /// `Err(Elapsed)` indicates that `when` represents an instant that has
    /// already passed. In this case, the caller should fire the timeout
    /// immediately.
    ///
    /// `Err(Invalid)` indicates an invalid `when` argument as been supplied.
    ///
    /// # Safety
    ///
    /// This function registers item into an intrusive linked list. The caller
    /// must ensure that `item` is pinned and will not be dropped without first
    /// being deregistered.
    pub(crate) unsafe fn insert(
        &mut self,
        item: TimerHandle,
    ) -> Result<u64, (TimerHandle, InsertError)> {
        let when = item.sync_when();

        if when <= self.elapsed {
            return Err((item, InsertError::Elapsed));
        }
        if overflow(self.elapsed, when) {
            self.add_to_overflow(item);
            return Ok(when);
        }

        // Get the level at which the entry should be stored
        let level = self.level_for(when);

        unsafe {
            self.levels[level].add_entry(item);
        }

        assert!({
            self.levels[level]
                .next_expiration(self.elapsed)
                .map(|e| e.deadline >= self.elapsed)
                .unwrap_or(true)
        });

        Ok(when)
    }

    /// Removes `item` from the timing wheel.
    pub(crate) unsafe fn remove(&mut self, item: NonNull<TimerShared>) {
        unsafe {
            let when = item.as_ref().cached_when();

            if item.as_ref().overflow() {
                // SAFETY: We ensure this entry is in the overflow linked list.
                self.remove_from_overflow(item);
            } else {
                assert!(
                    self.elapsed <= when,
                    "elapsed={}; when={}",
                    self.elapsed,
                    when
                );

                let level = self.level_for(when);
                // SAFETY: We ensure this entry is in the levels.
                self.levels[level].remove_entry(item);
            }
        }
    }

    /// Instant at which to poll.
    pub(crate) fn poll_at(&self) -> Option<u64> {
        let expiration_slot = self.next_expiration().map(|expiration| expiration.deadline);
        if expiration_slot.is_some() {
            return expiration_slot;
        }
        // If overflow is not empty, then the ideal instant is the minimum expiration time of all
        // `TimerHandle`s  in overflow. However, the cost of traversing overflow can be huge.

        //`overflow_round as u64 * OVERFLOW_DURATION` is not optimal, but considering that this is a huge
        // sleep time, it will not be an unnecessary thread wakeup issue.
        if !self.overflow.is_empty() {
            return Some(self.overflow_round as u64 * OVERFLOW_DURATION);
        }
        None
    }

    /// Advances the timer up to the instant represented by `now`.
    pub(crate) fn poll(&mut self, now: u64) -> Option<Expiration> {
        match self.next_expiration() {
            Some(expiration) if expiration.deadline <= now => Some(expiration),
            _ => {
                // in this case the poll did not indicate an expiration
                // _and_ we were not able to find a next expiration in
                // the current list of timers.  advance to the poll's
                // current time and do nothing else.
                self.try_set_elapsed(now);
                None
            }
        }
    }

    /// Returns the instant at which the next timeout expires.
    fn next_expiration(&self) -> Option<Expiration> {
        // Check all levels
        for (level_num, level) in self.levels.iter().enumerate() {
            if let Some(expiration) = level.next_expiration(self.elapsed) {
                // There cannot be any expirations at a higher level that happen
                // before this one.
                assert!(self.no_expirations_before(level_num + 1, expiration.deadline));

                return Some(expiration);
            }
        }
        None
    }

    /// Returns the instant at which the next timeout expires.
    pub(super) fn next_overflow_expiration(&self) -> Option<u64> {
        if self.overflow.is_empty() {
            return None;
        }
        // 36 + 16 = 52, not overflow for u64
        Some(self.overflow_round as u64 * OVERFLOW_DURATION)
    }

    /// Returns the tick at which this timer wheel next needs to perform some
    /// processing, or None if there are no timers registered.
    pub(super) fn next_expiration_time(&self) -> Option<u64> {
        let res = self.next_expiration().map(|ex| ex.deadline);
        if res.is_some() {
            return res;
        }
        if !self.overflow.is_empty() {
            return Some(self.elapsed + OVERFLOW_DURATION);
        }
        None
    }

    /// Used for debug assertions
    fn no_expirations_before(&self, start_level: usize, before: u64) -> bool {
        let mut res = true;

        for level in &self.levels[start_level..] {
            if let Some(e2) = level.next_expiration(self.elapsed) {
                if e2.deadline < before {
                    res = false;
                }
            }
        }

        res
    }

    /// When we pop entries from the overflow linked list or the linked list in levels,
    /// if the entry is not expired, we reinsert the entry to the lists of the time wheel.
    pub(super) fn reinsert(&mut self, entry: TimerHandle, elapsed: u64, when: u64) {
        if overflow(elapsed, when) {
            unsafe { self.add_to_overflow(entry) };
        } else {
            let level = level_for(elapsed, when);
            unsafe { self.levels[level].add_entry(entry) };
        }
    }

    pub(super) fn get_overflow_count(&self) -> u64 {
        self.overflow_count
    }

    /// SAFETY: This entry must *not* be in other linked list.
    pub(super) unsafe fn add_to_overflow(&mut self, entry: TimerHandle) {
        unsafe { entry.mark_overflow() };
        self.overflow.push_front(entry);
        self.overflow_count += 1;
    }

    /// SAFETY: This item must be in the overflow linked list.
    unsafe fn remove_from_overflow(&mut self, item: NonNull<TimerShared>) {
        self.overflow.remove(item);
        self.overflow_count -= 1;
        item.as_ref().unmark_overflow();
    }

    // If `elapsed` is smaller than `when`, we update `elapsed` to `when`.
    // Otherwise, we do not update it.
    pub(super) fn try_set_elapsed(&mut self, when: u64) {
        if when > self.elapsed {
            self.elapsed = when;
        }
    }

    pub(super) fn get_entry_from_overflow(&mut self) -> Option<TimerHandle> {
        if let Some(item) = self.overflow.pop_back() {
            self.overflow_count -= 1;
            return Some(item);
        }
        None
    }

    pub(super) fn get_mut_entries(&mut self, expiration: &Expiration) -> &mut EntryList {
        self.levels[expiration.level].get_mut_entries(expiration.slot)
    }

    pub(super) fn occupied_bit_maintain(&mut self, expiration: &Expiration) {
        self.levels[expiration.level].occupied_bit_maintain(expiration.slot);
    }

    pub(super) fn increate_overflow_rount(&mut self, now: u64) {
        self.overflow_round = (now >> (6 * NUM_LEVELS)) as u32 + 1;
    }

    fn level_for(&self, when: u64) -> usize {
        level_for(self.elapsed, when)
    }
}

fn level_for(elapsed: u64, when: u64) -> usize {
    const SLOT_MASK: u64 = (1 << 6) - 1;

    // Mask in the trailing bits ignored by the level calculation in order to cap
    // the possible leading zeros
    let mut masked = elapsed ^ when | SLOT_MASK;

    if masked >= MAX_DURATION {
        // Fudge the timer into the top level
        masked = MAX_DURATION - 1;
    }

    let leading_zeros = masked.leading_zeros() as usize;
    let significant = 63 - leading_zeros;

    significant / NUM_LEVELS
}

fn overflow(elapsed: u64, when: u64) -> bool {
    if when <= elapsed {
        return false;
    }
    when - elapsed > MAX_DURATION
}

#[cfg(all(test, not(loom)))]
mod test {
    use super::*;

    #[test]
    fn test_level_for() {
        for pos in 0..64 {
            assert_eq!(
                0,
                level_for(0, pos),
                "level_for({}) -- binary = {:b}",
                pos,
                pos
            );
        }

        for level in 1..5 {
            for pos in level..64 {
                let a = pos * 64_usize.pow(level as u32);
                assert_eq!(
                    level,
                    level_for(0, a as u64),
                    "level_for({}) -- binary = {:b}",
                    a,
                    a
                );

                if pos > level {
                    let a = a - 1;
                    assert_eq!(
                        level,
                        level_for(0, a as u64),
                        "level_for({}) -- binary = {:b}",
                        a,
                        a
                    );
                }

                if pos < 64 {
                    let a = a + 1;
                    assert_eq!(
                        level,
                        level_for(0, a as u64),
                        "level_for({}) -- binary = {:b}",
                        a,
                        a
                    );
                }
            }
        }
    }
}
