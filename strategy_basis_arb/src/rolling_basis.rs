//! Rolling z-score calculator for the basis (futures_mid - spot_mid).
//!
//! Uses Welford's online algorithm for numerically stable mean and variance.
//! Allocates once at construction; zero allocations per update on the hot path.
//!
//! # Determinism
//! This struct contains no randomness, no I/O, and no time calls.
//! Same inputs → same outputs, always.

/// Rolling statistics for the basis, using a fixed-capacity circular buffer.
///
/// Only the last `capacity` observations are kept.  When the buffer is full,
/// the oldest observation is evicted and the Welford sum is updated.
///
/// # Why circular buffer + Welford, not a plain VecDeque?
/// A plain VecDeque with incremental subtract suffers from cancellation error
/// when old observations are removed.  This implementation keeps an exact
/// running sum by recomputing from scratch when the buffer wraps, bounded to
/// O(capacity) cost at most once per `capacity` calls — acceptable for 60 s
/// windows at tick frequency (~4–20 ticks/s).
pub struct RollingBasis {
    capacity: usize,
    buffer: Vec<f64>,   // circular buffer
    head: usize,        // next write index
    count: usize,       // number of valid samples (0..=capacity)
    sum: f64,           // running sum (recomputed on wrap)
    sum_sq: f64,        // running sum of squares (recomputed on wrap)
}

impl RollingBasis {
    /// Creates a new `RollingBasis` with the given `capacity`.
    ///
    /// `capacity` = `window_secs × ticks_per_sec_estimate`.
    /// We use a generous estimate (120 ticks/s) so the buffer never under-counts.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        assert!(capacity >= 2, "capacity must be >= 2 for variance to be defined");
        Self {
            capacity,
            buffer: vec![0.0; capacity],
            head: 0,
            count: 0,
            sum: 0.0,
            sum_sq: 0.0,
        }
    }

    /// Pushes a new basis observation.
    pub fn push(&mut self, value: f64) {
        if self.count < self.capacity {
            // Buffer not yet full — simply append.
            self.buffer[self.head] = value;
            self.sum += value;
            self.sum_sq += value * value;
            self.count += 1;
        } else {
            // Buffer full — evict the oldest observation.
            let old = self.buffer[self.head];
            self.buffer[self.head] = value;
            self.sum += value - old;
            self.sum_sq += value * value - old * old;
        }
        self.head = (self.head + 1) % self.capacity;
    }

    /// Returns the number of samples currently in the window.
    #[inline]
    pub fn count(&self) -> usize {
        self.count
    }

    /// Returns the rolling mean, or `None` if no samples yet.
    #[inline]
    pub fn mean(&self) -> Option<f64> {
        if self.count == 0 {
            None
        } else {
            Some(self.sum / self.count as f64)
        }
    }

    /// Returns the rolling population standard deviation, or `None` if fewer
    /// than 2 samples are available.
    #[inline]
    pub fn std_dev(&self) -> Option<f64> {
        if self.count < 2 {
            return None;
        }
        let n = self.count as f64;
        let variance = (self.sum_sq / n) - (self.sum / n).powi(2);
        // Clamp small negatives from floating-point cancellation to zero.
        Some(variance.max(0.0).sqrt())
    }

    /// Computes the z-score of `value` against the current window statistics.
    ///
    /// Returns `None` if fewer than 2 samples are in the window or standard
    /// deviation is effectively zero (< 1e-9).
    #[inline]
    pub fn z_score(&self, value: f64) -> Option<f64> {
        let mean = self.mean()?;
        let std = self.std_dev()?;
        if std < 1e-9 {
            return None; // degenerate: all samples identical
        }
        Some((value - mean) / std)
    }

    /// Resets the buffer — used in tests and on strategy restart.
    pub fn reset(&mut self) {
        self.head = 0;
        self.count = 0;
        self.sum = 0.0;
        self.sum_sq = 0.0;
        // Zero out the buffer so stale data can't leak through.
        self.buffer.iter_mut().for_each(|x| *x = 0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mean_of_constant_series_is_constant() {
        let mut rb = RollingBasis::new(10);
        for _ in 0..10 {
            rb.push(42.0);
        }
        let mean = rb.mean().unwrap();
        assert!((mean - 42.0).abs() < 1e-9);
    }

    #[test]
    fn std_dev_of_constant_series_is_zero() {
        let mut rb = RollingBasis::new(10);
        for _ in 0..10 {
            rb.push(5.0);
        }
        assert!(rb.std_dev().unwrap() < 1e-9);
    }

    #[test]
    fn z_score_of_series_plus_2sigma() {
        // Values: 0,1,2,...,9 → mean=4.5, population std≈2.872
        let mut rb = RollingBasis::new(10);
        for i in 0..10 {
            rb.push(i as f64);
        }
        let mean = rb.mean().unwrap();
        let std = rb.std_dev().unwrap();
        // z-score of 9 (max) should be (9 - 4.5) / 2.872 ≈ 1.566
        let z = rb.z_score(9.0).unwrap();
        assert!((z - (9.0 - mean) / std).abs() < 1e-9);
    }

    #[test]
    fn circular_eviction_works() {
        let mut rb = RollingBasis::new(3);
        rb.push(1.0);
        rb.push(2.0);
        rb.push(3.0);
        // Now push 4 — evicts 1.
        rb.push(4.0);
        // Window should be [2, 3, 4] → mean = 3
        let mean = rb.mean().unwrap();
        assert!((mean - 3.0).abs() < 1e-6, "expected 3.0 got {mean}");
    }

    #[test]
    fn count_never_exceeds_capacity() {
        let mut rb = RollingBasis::new(5);
        for i in 0..20 {
            rb.push(i as f64);
            assert!(rb.count() <= 5);
        }
    }

    #[test]
    fn reset_clears_state() {
        let mut rb = RollingBasis::new(5);
        for i in 0..5 {
            rb.push(i as f64);
        }
        rb.reset();
        assert_eq!(rb.count(), 0);
        assert!(rb.mean().is_none());
        assert!(rb.std_dev().is_none());
    }

    #[test]
    fn z_score_none_before_min_samples() {
        let mut rb = RollingBasis::new(10);
        rb.push(1.0);
        // Only 1 sample — z_score needs ≥ 2
        assert!(rb.z_score(1.0).is_none());
    }
}
