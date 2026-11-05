//! Latency histogram with log-linear buckets (like HdrHistogram with 64 sub-buckets per
//! power of two: under 1.6% relative error). Values are nanoseconds. The Java harness in
//! `benchmarks/java-camel` uses the same bucket layout, so both sides report percentiles
//! the same way.

const SUB: u64 = 64;
const LINEAR: u64 = 2 * SUB; // values below this get one bucket each
const MAX_EXP: u64 = 44; // ~4.8 hours in ns; larger values are clamped
const BUCKETS: usize = (LINEAR + (MAX_EXP - 7 + 1) * SUB) as usize;

#[derive(Clone)]
pub struct Histogram {
    counts: Vec<u64>,
    total: u64,
    sum: u128,
    max: u64,
}

impl std::fmt::Debug for Histogram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Histogram(n={}, mean={:.0}, max={})", self.total, self.mean(), self.max)
    }
}

impl Default for Histogram {
    fn default() -> Self {
        Histogram { counts: vec![0; BUCKETS], total: 0, sum: 0, max: 0 }
    }
}

/// Bucket index of a value.
pub fn index(v: u64) -> usize {
    if v < LINEAR {
        return v as usize;
    }
    let e = (63 - v.leading_zeros() as u64).min(MAX_EXP);
    let shift = e - 6;
    let m = ((v >> shift).min(2 * SUB - 1)) - SUB;
    (LINEAR + (e - 7) * SUB + m) as usize
}

/// Midpoint of the values that fall into a bucket.
pub fn value_of(idx: usize) -> u64 {
    let idx = idx as u64;
    if idx < LINEAR {
        return idx;
    }
    let e = (idx - LINEAR) / SUB + 7;
    let m = (idx - LINEAR) % SUB + SUB;
    let shift = e - 6;
    (m << shift) + (1 << shift) / 2
}

impl Histogram {
    pub fn record(&mut self, v: u64) {
        self.counts[index(v)] += 1;
        self.total += 1;
        self.sum += v as u128;
        if v > self.max {
            self.max = v;
        }
    }

    pub fn merge(&mut self, other: &Histogram) {
        for (a, b) in self.counts.iter_mut().zip(&other.counts) {
            *a += b;
        }
        self.total += other.total;
        self.sum += other.sum;
        self.max = self.max.max(other.max);
    }

    pub fn count(&self) -> u64 {
        self.total
    }

    pub fn mean(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.sum as f64 / self.total as f64
        }
    }

    pub fn max(&self) -> u64 {
        self.max
    }

    /// The value at percentile `p` (0..=100): the smallest bucket whose cumulative count
    /// reaches `ceil(p/100 * total)`.
    pub fn percentile(&self, p: f64) -> u64 {
        if self.total == 0 {
            return 0;
        }
        let rank = ((p / 100.0 * self.total as f64).ceil() as u64).clamp(1, self.total);
        let mut seen = 0;
        for (i, c) in self.counts.iter().enumerate() {
            seen += c;
            if seen >= rank {
                return value_of(i).min(self.max);
            }
        }
        self.max
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_round_trip_within_precision() {
        for v in [0u64, 1, 127, 128, 129, 1000, 4095, 65_536, 1_234_567, 98_765_432_100] {
            let back = value_of(index(v));
            let err = (back as f64 - v as f64).abs() / (v.max(1) as f64);
            assert!(err < 0.016, "{v} -> {back} ({err})");
        }
        assert!(index(u64::MAX) < BUCKETS);
    }

    #[test]
    fn indices_are_monotonic() {
        let mut last = 0;
        for v in (0..2_000_000u64).step_by(37) {
            let i = index(v);
            assert!(i >= last);
            last = i;
        }
    }

    #[test]
    fn percentiles_and_merge() {
        let mut a = Histogram::default();
        for v in 1..=1000u64 {
            a.record(v * 1000);
        }
        assert_eq!(a.count(), 1000);
        assert!((a.mean() - 500_500.0).abs() < 1.0);
        let p50 = a.percentile(50.0) as f64;
        assert!((p50 - 500_000.0).abs() / 500_000.0 < 0.016, "{p50}");
        let p99 = a.percentile(99.0) as f64;
        assert!((p99 - 990_000.0).abs() / 990_000.0 < 0.016, "{p99}");
        assert_eq!(a.percentile(100.0), 1_000_000);
        let mut b = Histogram::default();
        b.record(5_000_000);
        a.merge(&b);
        assert_eq!(a.count(), 1001);
        assert_eq!(a.max(), 5_000_000);
        assert_eq!(a.percentile(100.0), 5_000_000);
    }
}
