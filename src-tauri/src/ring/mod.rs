//! Bounded ring buffer for captured records.
//!
//! A log stream is unbounded and the UI is not, so capture always writes into a
//! fixed-capacity ring: the newest `capacity` records survive and older ones are
//! evicted. Eviction is counted rather than silent, because "the view is dropping
//! data" is something the user must be able to see.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

/// Default capacity: 100k records, which is roughly a minute of a busy logcat
/// flood and comfortably more than any scroll-back a human reads by hand.
pub const DEFAULT_CAPACITY: usize = 100_000;

/// Counters describing a ring's occupancy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RingStats {
    /// Records currently retained.
    pub len: usize,
    /// Maximum records retained.
    pub capacity: usize,
    /// Records ever accepted.
    pub total_pushed: u64,
    /// Records evicted to make room.
    pub total_dropped: u64,
}

/// A fixed-capacity FIFO.
#[derive(Debug, Clone)]
pub struct RingBuffer<T> {
    items: VecDeque<T>,
    capacity: usize,
    total_pushed: u64,
    total_dropped: u64,
}

impl<T> RingBuffer<T> {
    /// Creates a ring holding at most `capacity` items.
    ///
    /// A capacity of zero would make the buffer useless, so it is clamped to 1.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            items: VecDeque::with_capacity(capacity.max(1)),
            capacity: capacity.max(1),
            total_pushed: 0,
            total_dropped: 0,
        }
    }

    /// Maximum number of retained items.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Number of retained items.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// True when nothing has been retained.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Appends `item`, evicting the oldest entry when full.
    ///
    /// Returns `true` when an eviction happened, so callers can surface drops.
    pub fn push(&mut self, item: T) -> bool {
        let evicted = self.items.len() == self.capacity;
        if evicted {
            self.items.pop_front();
            self.total_dropped = self.total_dropped.saturating_add(1);
        }
        self.items.push_back(item);
        self.total_pushed = self.total_pushed.saturating_add(1);
        evicted
    }

    /// Drops every retained item, preserving lifetime counters.
    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// The most recently pushed item.
    #[must_use]
    pub fn last(&self) -> Option<&T> {
        self.items.back()
    }

    /// Iterates oldest to newest.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.items.iter()
    }

    /// Clones the retained items, oldest first.
    #[must_use]
    pub fn snapshot(&self) -> Vec<T>
    where
        T: Clone,
    {
        self.items.iter().cloned().collect()
    }

    /// Removes and returns every retained item, oldest first.
    pub fn drain(&mut self) -> Vec<T> {
        self.items.drain(..).collect()
    }

    /// Current occupancy and lifetime counters.
    #[must_use]
    pub fn stats(&self) -> RingStats {
        RingStats {
            len: self.items.len(),
            capacity: self.capacity,
            total_pushed: self.total_pushed,
            total_dropped: self.total_dropped,
        }
    }
}

impl<T> Default for RingBuffer<T> {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_capacity_is_clamped_to_one() {
        let ring: RingBuffer<u8> = RingBuffer::new(0);
        assert_eq!(ring.capacity(), 1);
    }

    #[test]
    fn items_are_retained_in_order() {
        let mut ring = RingBuffer::new(4);
        for value in 1..=3 {
            assert!(!ring.push(value), "no eviction expected below capacity");
        }
        assert_eq!(ring.snapshot(), vec![1, 2, 3]);
        assert_eq!(ring.len(), 3);
        assert!(!ring.is_empty());
    }

    #[test]
    fn oldest_items_are_evicted_first() {
        let mut ring = RingBuffer::new(3);
        for value in 1..=5 {
            ring.push(value);
        }
        assert_eq!(ring.snapshot(), vec![3, 4, 5]);
        assert_eq!(ring.last(), Some(&5));
    }

    #[test]
    fn eviction_and_push_are_counted() {
        let mut ring = RingBuffer::new(2);
        ring.push(1);
        ring.push(2);
        assert!(ring.push(3), "third push must report an eviction");

        let stats = ring.stats();
        assert_eq!(stats.total_pushed, 3);
        assert_eq!(stats.total_dropped, 1);
        assert_eq!(stats.len, 2);
        assert_eq!(stats.capacity, 2);
    }

    #[test]
    fn clear_keeps_lifetime_counters() {
        let mut ring = RingBuffer::new(2);
        ring.push(1);
        ring.push(2);
        ring.push(3);
        ring.clear();

        assert!(ring.is_empty());
        let stats = ring.stats();
        assert_eq!(stats.total_pushed, 3);
        assert_eq!(stats.total_dropped, 1);
        assert_eq!(stats.len, 0);
    }

    #[test]
    fn drain_empties_and_returns_in_order() {
        let mut ring = RingBuffer::new(4);
        for value in 1..=3 {
            ring.push(value);
        }
        assert_eq!(ring.drain(), vec![1, 2, 3]);
        assert!(ring.is_empty());
        assert!(ring.drain().is_empty());
    }

    #[test]
    fn iter_walks_oldest_to_newest() {
        let mut ring = RingBuffer::new(3);
        for value in 1..=4 {
            ring.push(value);
        }
        let collected: Vec<i32> = ring.iter().copied().collect();
        assert_eq!(collected, vec![2, 3, 4]);
    }

    #[test]
    fn default_capacity_is_documented_value() {
        let ring: RingBuffer<u8> = RingBuffer::default();
        assert_eq!(ring.capacity(), DEFAULT_CAPACITY);
    }

    #[test]
    fn stats_serialise_camel_case() -> crate::error::Result<()> {
        let json = serde_json::to_value(RingBuffer::<u8>::new(8).stats())?;
        assert!(json.get("totalPushed").is_some());
        assert!(json.get("totalDropped").is_some());
        Ok(())
    }
}
