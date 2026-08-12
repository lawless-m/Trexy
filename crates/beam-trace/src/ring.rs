//! The in-memory ring buffer — TRACE-FORMAT.md §5.

use crate::Sample;

/// Default capacity, in samples.
///
/// The buffer is sized in **time**, not samples: it must cover the longest
/// phosphor tail plus margin, which TRACE-FORMAT.md §5 puts at 200 ms. At the
/// worst-case density this slice targets — 500 000 samples/s
/// (FIRST-SLICE.md §1 deliverable 5) — 200 ms is 100 000 samples, rounded up
/// here to the next power of two. At the 32-byte stride that is 4 MiB.
pub const DEFAULT_CAPACITY: usize = 131_072;

/// A fixed-capacity, time-ordered sample window.
///
/// Single producer, single consumer. Overrun **drops the oldest samples
/// silently** — by construction they are fully decayed, so there is nothing to
/// report and nothing to block for. The buffer never owns the time base: the
/// consumer asks for a window and supplies both ends of it.
///
/// This type is not internally synchronised. The producer runs on its own
/// thread and the discipline is exactly one writer and one reader; the caller
/// owns whatever handoff it needs.
#[derive(Debug)]
pub struct RingBuffer {
    samples: Box<[Sample]>,
    /// Physical index of the oldest live sample.
    head: usize,
    len: usize,
    epoch: f64,
    generation: u64,
}

impl RingBuffer {
    /// A buffer holding [`DEFAULT_CAPACITY`] samples, timestamped against
    /// `epoch` (seconds, producer-defined origin).
    pub fn new(epoch: f64) -> Self {
        Self::with_capacity(DEFAULT_CAPACITY, epoch)
    }

    /// A buffer holding `capacity` samples. Panics if `capacity` is zero.
    pub fn with_capacity(capacity: usize, epoch: f64) -> Self {
        assert!(capacity > 0, "ring buffer capacity must be non-zero");
        Self {
            samples: vec![Sample::default(); capacity].into_boxed_slice(),
            head: 0,
            len: 0,
            epoch,
            generation: 0,
        }
    }

    pub fn capacity(&self) -> usize {
        self.samples.len()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Seconds; sample `t` is relative to this.
    pub fn epoch(&self) -> f64 {
        self.epoch
    }

    /// How many times the buffer has been rebased. `t` values from different
    /// generations are not comparable.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Start a new generation: discard all samples and re-origin `t` against
    /// `epoch`. f32 `t` holds roughly microsecond resolution over a few tens
    /// of seconds, so rebasing per generation is what keeps precision from
    /// degrading over a session (TRACE-FORMAT.md §2).
    pub fn rebase(&mut self, epoch: f64) {
        self.head = 0;
        self.len = 0;
        self.epoch = epoch;
        self.generation += 1;
    }

    /// Append one sample, dropping the oldest if the buffer is full.
    pub fn push(&mut self, sample: Sample) {
        let capacity = self.capacity();
        if self.len == capacity {
            self.head = (self.head + 1) % capacity;
            self.len -= 1;
        }
        let slot = (self.head + self.len) % capacity;
        self.samples[slot] = sample;
        self.len += 1;
    }

    /// Append many samples in order.
    pub fn extend(&mut self, samples: &[Sample]) {
        for &s in samples {
            self.push(s);
        }
    }

    /// The sample at logical index `i`, oldest first.
    pub fn get(&self, i: usize) -> Option<&Sample> {
        (i < self.len).then(|| &self.samples[(self.head + i) % self.capacity()])
    }

    /// All samples in `(after, now]`, as up to two contiguous slices in time
    /// order — two because the window may straddle the wrap point. Both are
    /// raw `Sample` records suitable for direct copy to the GPU.
    pub fn samples_in(&self, after: f32, now: f32) -> (&[Sample], &[Sample]) {
        let lo = self.first_index_after(after);
        let hi = self.first_index_after(now);
        if hi <= lo {
            return (&[], &[]);
        }
        self.slices(lo, hi - lo)
    }

    /// Every sample needed to reconstruct the spans overlapping `[from, to]`.
    ///
    /// This is [`Self::samples_in`] plus the sample at or before `from`. The
    /// renderer asks for a window of *time*, but it draws *spans*, and a span
    /// that straddles the start of the window needs the sample before it too —
    /// without which the first stroke of every frame would be dropped.
    pub fn spans_in(&self, from: f32, to: f32) -> (&[Sample], &[Sample]) {
        let after = self.first_index_after(from);
        let lo = after.saturating_sub(1);
        let hi = self.first_index_after(to);
        if hi <= lo {
            return (&[], &[]);
        }
        self.slices(lo, hi - lo)
    }

    /// Logical index of the first sample with `t > bound`, or `len` if none.
    fn first_index_after(&self, bound: f32) -> usize {
        let (mut lo, mut hi) = (0usize, self.len);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.samples[(self.head + mid) % self.capacity()].t > bound {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        lo
    }

    /// Split `count` samples starting at logical index `start` into the one or
    /// two contiguous physical runs they occupy.
    fn slices(&self, start: usize, count: usize) -> (&[Sample], &[Sample]) {
        let capacity = self.capacity();
        let first = (self.head + start) % capacity;
        let to_end = capacity - first;
        if count <= to_end {
            (&self.samples[first..first + count], &[])
        } else {
            (&self.samples[first..], &self.samples[..count - to_end])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(t: f32) -> Sample {
        Sample::mono(t, t, 1.0, t)
    }

    fn window(ring: &RingBuffer, after: f32, now: f32) -> Vec<f32> {
        let (a, b) = ring.samples_in(after, now);
        a.iter().chain(b.iter()).map(|s| s.t).collect()
    }

    #[test]
    fn push_then_query_returns_the_half_open_window() {
        let mut ring = RingBuffer::with_capacity(8, 0.0);
        ring.extend(&[at(1.0), at(2.0), at(3.0), at(4.0), at(5.0)]);

        // (after, now] — after is exclusive, now inclusive.
        assert_eq!(window(&ring, 2.0, 4.0), vec![3.0, 4.0]);
        assert_eq!(window(&ring, 0.0, 5.0), vec![1.0, 2.0, 3.0, 4.0, 5.0]);
        assert_eq!(window(&ring, 5.0, 9.0), Vec::<f32>::new());
        assert_eq!(window(&ring, 4.0, 4.0), Vec::<f32>::new());
        assert_eq!(window(&ring, 0.5, 1.0), vec![1.0]);
    }

    #[test]
    fn overrun_drops_the_oldest_silently() {
        let mut ring = RingBuffer::with_capacity(4, 0.0);
        ring.extend(&[at(1.0), at(2.0), at(3.0), at(4.0), at(5.0), at(6.0)]);

        assert_eq!(ring.len(), 4);
        assert_eq!(ring.capacity(), 4);
        assert_eq!(window(&ring, 0.0, 10.0), vec![3.0, 4.0, 5.0, 6.0]);
    }

    #[test]
    fn window_straddling_the_wrap_point_returns_two_slices_in_order() {
        let mut ring = RingBuffer::with_capacity(4, 0.0);
        // Six pushes into four slots leaves head at physical index 2.
        ring.extend(&[at(1.0), at(2.0), at(3.0), at(4.0), at(5.0), at(6.0)]);

        let (a, b) = ring.samples_in(0.0, 10.0);
        assert_eq!(a.len(), 2, "first run reaches the end of the backing store");
        assert_eq!(b.len(), 2, "second run wraps to the start");
        assert_eq!(window(&ring, 0.0, 10.0), vec![3.0, 4.0, 5.0, 6.0]);
    }

    #[test]
    fn a_span_query_includes_the_sample_before_the_window() {
        let mut ring = RingBuffer::with_capacity(8, 0.0);
        ring.extend(&[at(1.0), at(2.0), at(3.0), at(4.0), at(5.0)]);

        // The sample window excludes 2.0; the span window keeps it, because
        // the span 2.0 -> 3.0 overlaps the requested time.
        let (a, b) = ring.spans_in(2.0, 4.0);
        let ts: Vec<f32> = a.iter().chain(b.iter()).map(|s| s.t).collect();
        assert_eq!(ts, vec![2.0, 3.0, 4.0]);

        // Nothing before the first sample to include.
        let (a, b) = ring.spans_in(0.0, 2.0);
        let ts: Vec<f32> = a.iter().chain(b.iter()).map(|s| s.t).collect();
        assert_eq!(ts, vec![1.0, 2.0]);
    }

    #[test]
    fn rebase_starts_a_new_generation_and_clears() {
        let mut ring = RingBuffer::with_capacity(4, 100.0);
        ring.extend(&[at(1.0), at(2.0)]);
        assert_eq!(ring.generation(), 0);

        ring.rebase(200.0);
        assert!(ring.is_empty());
        assert_eq!(ring.epoch(), 200.0);
        assert_eq!(ring.generation(), 1);
    }

    #[test]
    fn get_indexes_oldest_first_across_the_wrap() {
        let mut ring = RingBuffer::with_capacity(4, 0.0);
        ring.extend(&[at(1.0), at(2.0), at(3.0), at(4.0), at(5.0)]);
        let ts: Vec<f32> = (0..ring.len()).map(|i| ring.get(i).unwrap().t).collect();
        assert_eq!(ts, vec![2.0, 3.0, 4.0, 5.0]);
        assert!(ring.get(4).is_none());
    }
}
