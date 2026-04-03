use crate::types::CounterSeries;

/// Get cumulative counter value at a cycle from sparse samples.
///
/// Uses binary search over sparse samples. Returns the value from the
/// sample at or just before the given cycle.
pub fn counter_value_at(series: &CounterSeries, cycle: u32) -> u64 {
    if series.samples.is_empty() {
        return 0;
    }
    match series.samples.binary_search_by_key(&cycle, |(c, _)| *c) {
        Ok(i) => series.samples[i].1,
        Err(0) => 0,
        Err(i) => series.samples[i - 1].1,
    }
}

/// Get counter rate over a window ending at the given cycle.
pub fn counter_rate_at(series: &CounterSeries, cycle: u32, window: u32) -> f64 {
    let end_val = counter_value_at(series, cycle);
    let start_cycle = cycle.saturating_sub(window);
    let start_val = counter_value_at(series, start_cycle);
    let actual_window = cycle.saturating_sub(start_cycle);
    if actual_window == 0 {
        return 0.0;
    }
    (end_val.wrapping_sub(start_val)) as f64 / actual_window as f64
}

/// Get single-cycle delta for a counter.
///
/// With sparse samples, computes the interpolated per-cycle rate between
/// the two nearest samples surrounding the given cycle.
pub fn counter_delta_at(series: &CounterSeries, cycle: u32) -> u64 {
    if series.samples.is_empty() {
        return 0;
    }
    match series.samples.binary_search_by_key(&cycle, |(c, _)| *c) {
        Ok(i) => {
            // Exact match on a sample boundary.
            if i == 0 {
                let (c, v) = series.samples[0];
                if c == 0 {
                    return v;
                }
                return v / c as u64;
            }
            let (prev_c, prev_v) = series.samples[i - 1];
            let (cur_c, cur_v) = series.samples[i];
            let span = cur_c.saturating_sub(prev_c) as u64;
            if span == 0 {
                return 0;
            }
            cur_v.wrapping_sub(prev_v) / span
        }
        Err(0) => {
            if series.samples.is_empty() {
                return 0;
            }
            let (c, v) = series.samples[0];
            if c == 0 {
                return 0;
            }
            v / c as u64
        }
        Err(i) if i >= series.samples.len() => {
            // After the last sample: assume the counter stops changing.
            0
        }
        Err(i) => {
            // Between samples[i-1] and samples[i].
            let (prev_c, prev_v) = series.samples[i - 1];
            let (next_c, next_v) = series.samples[i];
            let span = next_c.saturating_sub(prev_c) as u64;
            if span == 0 {
                return 0;
            }
            next_v.wrapping_sub(prev_v) / span
        }
    }
}

/// Downsample a counter to min-max envelope buckets over a cycle range.
///
/// Returns `bucket_count` pairs of `(min_rate, max_rate)` covering
/// `[start_cycle, end_cycle)`. Each bucket reports the min and max
/// per-cycle rates among the sparse sample intervals that overlap
/// that bucket. Useful for sparkline rendering where many cycles
/// compress into one pixel.
pub fn counter_downsample_minmax(
    series: &CounterSeries,
    start_cycle: u32,
    end_cycle: u32,
    bucket_count: usize,
) -> Vec<(u64, u64)> {
    if bucket_count == 0 || start_cycle >= end_cycle {
        return Vec::new();
    }
    if series.samples.is_empty() {
        return vec![(0, 0); bucket_count];
    }

    // Build intervals with f64 rates to avoid integer division truncation.
    // Each interval: (start_cycle, end_cycle, rate_per_cycle).
    let mut intervals: Vec<(u32, u32, f64)> = Vec::with_capacity(series.samples.len() + 1);
    if let Some(&(first_c, _first_v)) = series.samples.first() {
        if first_c > 0 {
            // No counter events fired before the first sample -- rate is 0.
            intervals.push((0, first_c, 0.0));
        }
    }
    for w in series.samples.windows(2) {
        let (c0, v0) = w[0];
        let (c1, v1) = w[1];
        let span = c1.saturating_sub(c0) as f64;
        let rate = if span > 0.0 {
            v1.wrapping_sub(v0) as f64 / span
        } else {
            0.0
        };
        intervals.push((c0, c1, rate));
    }

    let range = end_cycle.saturating_sub(start_cycle) as f64;
    let cycles_per_bucket = range / bucket_count as f64;

    // Compute f64 rates per bucket, then scale to integer range.
    let mut f64_result: Vec<(f64, f64)> = Vec::with_capacity(bucket_count);
    for b in 0..bucket_count {
        let bucket_start = start_cycle + (b as f64 * cycles_per_bucket) as u32;
        let bucket_end = start_cycle + ((b + 1) as f64 * cycles_per_bucket) as u32;
        let bucket_end = bucket_end.min(end_cycle);

        let mut min_rate = f64::MAX;
        let mut max_rate = 0.0f64;

        for &(iv_start, iv_end, rate) in &intervals {
            if iv_start < bucket_end && iv_end > bucket_start {
                min_rate = min_rate.min(rate);
                max_rate = max_rate.max(rate);
            }
        }
        if min_rate == f64::MAX {
            min_rate = 0.0;
            max_rate = 0.0;
        }
        f64_result.push((min_rate, max_rate));
    }

    // Scale so the global max maps to a large integer (paint_bars normalizes).
    let global_max = f64_result.iter().map(|(_, mx)| *mx).fold(0.0f64, f64::max);
    let scale = if global_max > 0.0 {
        1_000_000.0 / global_max
    } else {
        1.0
    };
    f64_result
        .iter()
        .map(|(mn, mx)| ((mn * scale) as u64, (mx * scale) as u64))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::CounterDisplayMode;

    #[test]
    fn test_counter_value_at() {
        let series = CounterSeries {
            name: "committed_insns".to_string(),
            samples: vec![(0, 0), (2, 5), (4, 8)],
            default_mode: CounterDisplayMode::Total,
        };
        assert_eq!(counter_value_at(&series, 0), 0);
        assert_eq!(counter_value_at(&series, 2), 5);
        assert_eq!(counter_value_at(&series, 4), 8);
        // Between samples: uses previous sample
        assert_eq!(counter_value_at(&series, 1), 0);
        assert_eq!(counter_value_at(&series, 3), 5);
        // Beyond range: uses last sample
        assert_eq!(counter_value_at(&series, 100), 8);
    }

    #[test]
    fn test_counter_rate_at() {
        let series = CounterSeries {
            name: "committed_insns".to_string(),
            samples: vec![(0, 0), (2, 4), (4, 8)],
            default_mode: CounterDisplayMode::Rate,
        };
        // Rate over 2-cycle window at cycle 4: (8-4)/2 = 2.0
        assert!((counter_rate_at(&series, 4, 2) - 2.0).abs() < f64::EPSILON);
        // Rate at cycle 0 with window 2: (0-0)/0 = 0 (edge case)
        assert!((counter_rate_at(&series, 0, 2) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_counter_delta_at() {
        let series = CounterSeries {
            name: "bp_misses".to_string(),
            samples: vec![(0, 0), (2, 4), (4, 10)],
            default_mode: CounterDisplayMode::Total,
        };
        // At cycle 0 (exact match, first sample): delta is 0 (value/cycle=0/0)
        assert_eq!(counter_delta_at(&series, 0), 0);
        // At cycle 1 (between 0 and 2): avg rate = 4/2 = 2
        assert_eq!(counter_delta_at(&series, 1), 2);
        // At cycle 3 (between 2 and 4): avg rate = 6/2 = 3
        assert_eq!(counter_delta_at(&series, 3), 3);
    }

    #[test]
    fn test_counter_downsample_minmax() {
        let series = CounterSeries {
            name: "test".to_string(),
            samples: vec![(0, 0), (5, 5), (10, 20)],
            default_mode: CounterDisplayMode::Total,
        };

        // 2 buckets over 10 cycles: bucket 0 = cycles 0..5, bucket 1 = cycles 5..10
        let result = counter_downsample_minmax(&series, 0, 10, 2);
        assert_eq!(result.len(), 2);
        assert!(result[0].1 > 0, "bucket 0 should be non-zero");
        assert!(
            result[1].1 > result[0].1,
            "bucket 1 (rate=3) > bucket 0 (rate=1)"
        );
        // Check approximate ratio: bucket1 / bucket0 ~ 3
        let ratio = result[1].1 as f64 / result[0].1 as f64;
        assert!(
            (ratio - 3.0).abs() < 0.1,
            "rate ratio should be ~3, got {}",
            ratio
        );

        // Edge case: empty range
        assert_eq!(counter_downsample_minmax(&series, 5, 5, 10).len(), 0);
        // Edge case: zero buckets
        assert_eq!(counter_downsample_minmax(&series, 0, 10, 0).len(), 0);
    }
}
