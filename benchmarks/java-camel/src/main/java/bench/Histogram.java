package bench;

/**
 * Latency histogram with the same log-linear bucket layout as
 * crates/cameleer-bench/src/hist.rs (64 sub-buckets per power of two). Values are nanoseconds.
 */
final class Histogram {
    private static final long SUB = 64;
    private static final long LINEAR = 2 * SUB;
    private static final long MAX_EXP = 44;
    private static final int BUCKETS = (int) (LINEAR + (MAX_EXP - 7 + 1) * SUB);

    private final long[] counts = new long[BUCKETS];
    private long total;
    private double sum;
    private long max;

    static int index(long v) {
        if (v < LINEAR) {
            return (int) Math.max(v, 0);
        }
        long e = Math.min(63 - Long.numberOfLeadingZeros(v), MAX_EXP);
        long shift = e - 6;
        long m = Math.min(v >>> shift, 2 * SUB - 1) - SUB;
        return (int) (LINEAR + (e - 7) * SUB + m);
    }

    static long valueOf(int idx) {
        if (idx < LINEAR) {
            return idx;
        }
        long e = (idx - LINEAR) / SUB + 7;
        long m = (idx - LINEAR) % SUB + SUB;
        long shift = e - 6;
        return (m << shift) + (1L << shift) / 2;
    }

    void record(long v) {
        counts[index(v)]++;
        total++;
        sum += v;
        if (v > max) {
            max = v;
        }
    }

    void merge(Histogram o) {
        for (int i = 0; i < BUCKETS; i++) {
            counts[i] += o.counts[i];
        }
        total += o.total;
        sum += o.sum;
        max = Math.max(max, o.max);
    }

    long count() {
        return total;
    }

    double mean() {
        return total == 0 ? 0 : sum / total;
    }

    long max() {
        return max;
    }

    long percentile(double p) {
        if (total == 0) {
            return 0;
        }
        long rank = Math.min(Math.max((long) Math.ceil(p / 100.0 * total), 1), total);
        long seen = 0;
        for (int i = 0; i < BUCKETS; i++) {
            seen += counts[i];
            if (seen >= rank) {
                return Math.min(valueOf(i), max);
            }
        }
        return max;
    }
}
