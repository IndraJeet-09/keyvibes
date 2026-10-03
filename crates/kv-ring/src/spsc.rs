//! Lock-free single-producer single-consumer ring buffer.
//!
//! This is a critical component for passing PlayCommands from the input thread
//! to the audio thread without blocking or allocation.

use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A lock-free SPSC (Single Producer Single Consumer) ring buffer.
///
/// # Correctness invariants
///
/// 1. Only one thread may call `push` (the producer).
/// 2. Only one thread may call `pop` (the consumer).
/// 3. The capacity must be a power of two.
/// 4. Indices use wrapping arithmetic with bitmask for efficiency.
///
/// # Cache-line separation
///
/// The head and tail indices are separated to avoid false sharing between
/// the producer and consumer threads.
#[repr(C)]
pub struct SpscRing<T> {
    /// Storage for ring buffer elements.
    buffer: Box<[UnsafeCell<MaybeUninit<T>>]>,

    /// Capacity (must be power of two).
    capacity: usize,

    /// Bitmask for fast modulo (capacity - 1).
    mask: usize,

    /// Tail index (producer writes here).
    /// Separated to its own cache line.
    _pad0: [u8; 64],
    tail: AtomicUsize,
    _pad1: [u8; 64],

    /// Head index (consumer reads from here).
    /// Separated to its own cache line.
    _pad2: [u8; 64],
    head: AtomicUsize,
    _pad3: [u8; 64],
}

// SAFETY: T is Send, and the SPSC protocol ensures no data races.
// Only the producer writes to tail and only the consumer writes to head.
unsafe impl<T: Send> Send for SpscRing<T> {}
unsafe impl<T: Send> Sync for SpscRing<T> {}

impl<T: Copy> SpscRing<T> {
    /// Creates a new SPSC ring buffer with the given capacity.
    ///
    /// # Panics
    ///
    /// Panics if capacity is not a power of two or is zero.
    pub fn new(capacity: usize) -> Self
    where
        T: Default,
    {
        assert!(capacity > 0, "Capacity must be greater than zero");
        assert!(
            capacity.is_power_of_two(),
            "Capacity must be a power of two"
        );

        let buffer: Vec<UnsafeCell<MaybeUninit<T>>> = (0..capacity)
            .map(|_| UnsafeCell::new(MaybeUninit::new(T::default())))
            .collect();

        Self {
            buffer: buffer.into_boxed_slice(),
            capacity,
            mask: capacity - 1,
            _pad0: [0; 64],
            tail: AtomicUsize::new(0),
            _pad1: [0; 64],
            _pad2: [0; 64],
            head: AtomicUsize::new(0),
            _pad3: [0; 64],
        }
    }

    /// Creates a new SPSC ring buffer with the given capacity.
    ///
    /// Unlike `new`, this does not require `T: Default`.
    /// The buffer entries will be uninitialized until written.
    ///
    /// # Panics
    ///
    /// Panics if capacity is not a power of two or is zero.
    pub fn with_capacity(capacity: usize) -> Self {
        assert!(capacity > 0, "Capacity must be greater than zero");
        assert!(
            capacity.is_power_of_two(),
            "Capacity must be a power of two"
        );

        let buffer: Vec<UnsafeCell<MaybeUninit<T>>> = (0..capacity)
            .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
            .collect();

        Self {
            buffer: buffer.into_boxed_slice(),
            capacity,
            mask: capacity - 1,
            _pad0: [0; 64],
            tail: AtomicUsize::new(0),
            _pad1: [0; 64],
            _pad2: [0; 64],
            head: AtomicUsize::new(0),
            _pad3: [0; 64],
        }
    }

    /// Returns the capacity of the ring buffer.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Returns the number of elements currently in the buffer.
    ///
    /// Note: This is a snapshot and may be stale by the time it's read.
    #[inline]
    pub fn len(&self) -> usize {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Relaxed);
        tail.wrapping_sub(head)
    }

    /// Returns true if the buffer is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        let tail = self.tail.load(Ordering::Acquire);
        let head = self.head.load(Ordering::Relaxed);
        tail == head
    }

    /// Returns true if the buffer is full.
    #[inline]
    pub fn is_full(&self) -> bool {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        tail.wrapping_sub(head) == self.capacity
    }

    /// Attempts to push an element onto the ring buffer.
    ///
    /// Returns `Ok(())` if successful, or `Err(value)` if the buffer is full.
    ///
    /// # Safety
    ///
    /// Only the producer thread may call this method.
    #[inline]
    pub fn push(&self, value: T) -> Result<(), T> {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);

        // Check if full
        if tail.wrapping_sub(head) == self.capacity {
            return Err(value);
        }

        // Write the value
        let index = tail & self.mask;
        unsafe {
            // Handle both MaybeUninit and T
            let ptr = self.buffer[index].get() as *mut T;
            std::ptr::write(ptr, value);
        }

        // Advance tail with Release ordering to ensure the write is visible
        self.tail.store(tail.wrapping_add(1), Ordering::Release);

        Ok(())
    }

    /// Attempts to pop an element from the ring buffer.
    ///
    /// Returns `Some(value)` if successful, or `None` if the buffer is empty.
    ///
    /// # Safety
    ///
    /// Only the consumer thread may call this method.
    #[inline]
    pub fn pop(&self) -> Option<T> {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);

        // Check if empty
        if head == tail {
            return None;
        }

        // Read the value
        let index = head & self.mask;
        let value = unsafe {
            let ptr = self.buffer[index].get() as *const T;
            std::ptr::read(ptr)
        };

        // Advance head with Release ordering
        self.head.store(head.wrapping_add(1), Ordering::Release);

        Some(value)
    }

    /// Drains all elements from the buffer, calling `f` for each.
    ///
    /// # Safety
    ///
    /// Only the consumer thread may call this method.
    pub fn drain<F>(&self, mut f: F)
    where
        F: FnMut(T),
    {
        while let Some(value) = self.pop() {
            f(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new() {
        let ring: SpscRing<u32> = SpscRing::new(4);
        assert_eq!(ring.capacity(), 4);
        assert!(ring.is_empty());
        assert!(!ring.is_full());
    }

    #[test]
    #[should_panic(expected = "power of two")]
    fn test_new_non_power_of_two() {
        let _ring: SpscRing<u32> = SpscRing::new(3);
    }

    #[test]
    fn test_push_pop() {
        let ring: SpscRing<u32> = SpscRing::new(4);

        assert_eq!(ring.push(1), Ok(()));
        assert_eq!(ring.push(2), Ok(()));
        assert_eq!(ring.push(3), Ok(()));

        assert_eq!(ring.pop(), Some(1));
        assert_eq!(ring.pop(), Some(2));
        assert_eq!(ring.pop(), Some(3));
        assert_eq!(ring.pop(), None);
    }

    #[test]
    fn test_full() {
        let ring: SpscRing<u32> = SpscRing::new(4);

        assert_eq!(ring.push(1), Ok(()));
        assert_eq!(ring.push(2), Ok(()));
        assert_eq!(ring.push(3), Ok(()));
        assert_eq!(ring.push(4), Ok(()));

        // Now full
        assert!(ring.is_full());
        assert_eq!(ring.push(5), Err(5));

        // Pop one, should have space again
        assert_eq!(ring.pop(), Some(1));
        assert!(!ring.is_full());
        assert_eq!(ring.push(5), Ok(()));
    }

    #[test]
    fn test_wraparound() {
        let ring: SpscRing<u32> = SpscRing::new(4);

        // Fill and drain multiple times to test wraparound
        for cycle in 0..10 {
            for i in 0..4 {
                assert_eq!(ring.push(cycle * 10 + i), Ok(()));
            }
            assert!(ring.is_full());

            for i in 0..4 {
                assert_eq!(ring.pop(), Some(cycle * 10 + i));
            }
            assert!(ring.is_empty());
        }
    }

    #[test]
    fn test_drain() {
        let ring: SpscRing<u32> = SpscRing::new(8);

        for i in 0..5 {
            ring.push(i).unwrap();
        }

        let mut drained = Vec::new();
        ring.drain(|v| drained.push(v));

        assert_eq!(drained, vec![0, 1, 2, 3, 4]);
        assert!(ring.is_empty());
    }

    #[test]
    fn test_len() {
        let ring: SpscRing<u32> = SpscRing::new(8);

        assert_eq!(ring.len(), 0);
        ring.push(1).unwrap();
        assert_eq!(ring.len(), 1);
        ring.push(2).unwrap();
        assert_eq!(ring.len(), 2);
        ring.pop();
        assert_eq!(ring.len(), 1);
    }
}
