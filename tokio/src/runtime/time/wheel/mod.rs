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

    // The next wakeup time of the overflow.
    overflow_when: Option<u64>,

    // The overflow entry list.
    overflow: EntryList,
}

/// Number of levels. Each level has 64 slots. By using 6 levels with 64 slots
/// each, the timer is able to track time up to 2 years into the future with a
/// precision of 1 millisecond.
const NUM_LEVELS: usize = 6;

/// The maximum duration of a `Sleep`.
pub(super) const MAX_DURATION: u64 = (1 << (6 * NUM_LEVELS)) - 1;

impl Wheel {
    /// Creates a new timing wheel.
    pub(crate) fn new() -> Wheel {
        Wheel {
            elapsed: 0,
            overflow_when: None,
            levels: Box::new(array::from_fn(Level::new)),
            overflow: EntryList::new(),
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

        if self.is_overflow(when) {
            self.push_to_overflow(item);
            return Ok(when);
        }

        // Get the level at which the entry should be stored
        let level = self.level_for(when);

        unsafe {
            self.levels[level].add_entry(item);
        }

        debug_assert!({
            self.levels[level]
                .next_expiration(self.elapsed)
                .map(|e| e.deadline >= self.elapsed)
                .unwrap_or(true)
        });

        Ok(when)
    }

    /// Removes `item` from the timing wheel.
    pub(crate) unsafe fn remove(&mut self, item: NonNull<TimerShared>) {
        if item.as_ref().is_overflow() {
            self.overflow.remove(item);
            return;
        }

        unsafe {
            let when = item.as_ref().cached_when();
            if when != u64::MAX {
                debug_assert!(
                    self.elapsed <= when,
                    "elapsed={}; when={}",
                    self.elapsed,
                    when
                );

                let level = self.level_for(when);
                self.levels[level].remove_entry(item);
            }
        }
    }

    /// Instant at which to poll.
    pub(crate) fn poll_at(&self) -> Option<u64> {
        let wake_up = self.next_expiration().map(|expiration| expiration.deadline);
        min_option(wake_up, self.overflow_when)
    }
    /// Pushes the entry into overflow.
    pub(super) fn push_to_overflow(&mut self, entry: TimerHandle) {
        unsafe {
            entry.mark_overflow();
            self.try_update_overflow_when(entry.cached_when());
        }
        self.overflow.push_front(entry);
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
                self.set_elapsed(now);
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
                debug_assert!(self.no_expirations_before(level_num + 1, expiration.deadline));

                return Some(expiration);
            }
        }

        None
    }

    /// Returns the instant at which the next timeout expires.
    fn next_expiration_deadline(&self) -> Option<u64> {
        // Check all levels
        let mut deadline = None;
        for (level_num, level) in self.levels.iter().enumerate() {
            if let Some(expiration) = level.next_expiration(self.elapsed) {
                // There cannot be any expirations at a higher level that happen
                // before this one.
                debug_assert!(self.no_expirations_before(level_num + 1, expiration.deadline));
                deadline = Some(expiration.deadline);
                break;
            }
        }
        min_option(deadline, self.overflow_when)
    }

    /// Returns the tick at which this timer wheel next needs to perform some
    /// processing, or None if there are no timers registered.
    pub(super) fn next_expiration_time(&self) -> Option<u64> {
        self.next_expiration_deadline()
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

    pub(super) fn add_entry(
        &mut self,
        entry: TimerHandle,
        expiration: Expiration,
        expiration_tick: u64,
    ) {
        let level = level_for(expiration.deadline, expiration_tick);
        unsafe { self.levels[level].add_entry(entry) };
    }

    pub(super) fn set_elapsed(&mut self, when: u64) {
        assert!(
            self.elapsed <= when,
            "elapsed={:?}; when={:?}",
            self.elapsed,
            when
        );

        if when > self.elapsed {
            self.elapsed = when;
        }
    }

    pub(super) fn get_mut_entries(&mut self, expiration: &Expiration) -> &mut EntryList {
        self.levels[expiration.level].get_mut_entries(expiration.slot)
    }

    pub(super) fn mark_empty(&mut self, expiration: &Expiration) {
        self.levels[expiration.level].mark_empty(expiration.slot);
    }

    /// Checks whether we should check the overflow entry list.
    pub(super) fn might_check_overflow(&self, now: u64) -> bool {
        if self.overflow_when.map_or(false, |when| when <= now) {
            return true;
        }
        false
    }

    pub(super) fn take_overflow(&mut self) -> EntryList {
        self.overflow_when = None;
        std::mem::take(&mut self.overflow)
    }

    fn level_for(&self, when: u64) -> usize {
        level_for(self.elapsed, when)
    }

    // Returns whether when is overflow.
    pub(super) fn is_overflow(&self, when: u64) -> bool {
        is_overflow(self.elapsed, when)
    }

    // Attemps to update the overflow_when. This updae will only be successful
    // if overflow_when is None, or when is  than overflow_when.
    fn try_update_overflow_when(&mut self, when: u64) {
        if let Some(overflow_when) = self.overflow_when {
            if when < overflow_when {
                self.overflow_when = Some(when)
            }
        } else {
            self.overflow_when = Some(when)
        }
    }
}

fn min_option(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    a.map(|a_val| b.map_or(a_val, |b_val| a_val.min(b_val)))
        .or(b)
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

// Checks if the when is overflow
fn is_overflow(elapsed: u64, when: u64) -> bool {
    const SLOT_MASK: u64 = (1 << 6) - 1;

    // Mask in the trailing bits ignored by the level calculation in order to cap
    // the possible leading zeros
    let masked = elapsed ^ when | SLOT_MASK;

    if masked >= MAX_DURATION {
        return true;
    }
    false
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
