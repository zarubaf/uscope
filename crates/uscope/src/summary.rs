/// Counter mipmap (multi-resolution summary) computation for fast rendering.
///
/// Builds a pyramid of min/max/sum buckets over counter deltas so the viewer
/// can pick the appropriate resolution level for the current zoom.
use crate::reader::Reader;
use crate::state::TimedItem;
use crate::types::*;
use std::collections::HashMap;
use std::io::{self, Read, Write};

// ── Data structures ────────────────────────────────────────────────

/// A single mipmap bucket entry for a counter.
#[derive(Debug, Clone, Copy, Default)]
pub struct MipmapEntry {
    /// Minimum per-cycle delta observed in this bucket.
    pub min_delta: u64,
    /// Maximum per-cycle delta observed in this bucket.
    pub max_delta: u64,
    /// Total delta accumulated in this bucket.
    pub sum: u64,
}

/// Multi-level mipmap for a single counter.
#[derive(Debug, Clone)]
pub struct CounterMipmap {
    /// Human-readable counter name.
    pub name: String,
    /// Storage ID in the schema.
    pub storage_id: u16,
    /// Levels from finest (index 0) to coarsest. Each level halves resolution
    /// by `fan_out`.
    pub levels: Vec<Vec<MipmapEntry>>,
}

/// Counter summary with mipmaps for all counters in the trace.
#[derive(Debug, Clone)]
pub struct CounterSummary {
    /// Number of cycles per level-0 bucket.
    pub base_interval_cycles: u32,
    /// Number of child buckets aggregated into one parent bucket.
    pub fan_out: u32,
    /// Per-counter mipmaps.
    pub counters: Vec<CounterMipmap>,
}

// ── Computation ────────────────────────────────────────────────────

/// Compute counter mipmaps by replaying all segments from a trace file.
///
/// `period_ps` is the clock period in picoseconds (used to convert absolute
/// timestamps to cycle numbers).  Typically obtained from the schema's
/// clock domain definition.
///
/// Returns `Ok(CounterSummary)` with level-0 through level-N mipmap data.
/// If the trace contains no counter storages the result will have an empty
/// `counters` vector.
pub fn compute_counter_summary(reader: &mut Reader, period_ps: u64) -> io::Result<CounterSummary> {
    let base_interval: u32 = 1024; // power-of-two for clean alignment
    let fan_out: u32 = 4;

    if period_ps == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "period_ps must be > 0",
        ));
    }

    let schema = reader.schema().clone();

    // Identify counter storages: 1-slot, not sparse, not buffer, single U64 field.
    let counter_storages: Vec<(u16, String)> = schema
        .storages
        .iter()
        .filter(|s| {
            s.num_slots == 1
                && !s.is_sparse()
                && !s.is_buffer()
                && s.fields.len() == 1
                && s.fields[0].field_type == FieldType::U64 as u8
        })
        .map(|s| {
            let name = schema.get_string(s.name).unwrap_or("?").to_string();
            (s.storage_id, name)
        })
        .collect();

    if counter_storages.is_empty() {
        return Ok(CounterSummary {
            base_interval_cycles: base_interval,
            fan_out,
            counters: vec![],
        });
    }

    // Build storage_id -> counter index map.
    let counter_map: HashMap<u16, usize> = counter_storages
        .iter()
        .enumerate()
        .map(|(i, (sid, _))| (*sid, i))
        .collect();

    let num_counters = counter_storages.len();

    // Per-counter bucket accumulators for level 0.
    let mut bucket_min: Vec<u64> = vec![u64::MAX; num_counters];
    let mut bucket_max: Vec<u64> = vec![0; num_counters];
    let mut bucket_sum: Vec<u64> = vec![0; num_counters];
    let mut level0: Vec<Vec<MipmapEntry>> = (0..num_counters).map(|_| Vec::new()).collect();

    let mut current_bucket: u64 = 0;

    // Flush completed buckets up to (but not including) `target_bucket`.
    let flush_buckets = |current_bucket: &mut u64,
                         target_bucket: u64,
                         bucket_min: &mut [u64],
                         bucket_max: &mut [u64],
                         bucket_sum: &mut [u64],
                         level0: &mut [Vec<MipmapEntry>]| {
        while *current_bucket < target_bucket {
            for c in 0..num_counters {
                let min_val = if bucket_min[c] == u64::MAX {
                    0
                } else {
                    bucket_min[c]
                };
                level0[c].push(MipmapEntry {
                    min_delta: min_val,
                    max_delta: bucket_max[c],
                    sum: bucket_sum[c],
                });
                bucket_min[c] = u64::MAX;
                bucket_max[c] = 0;
                bucket_sum[c] = 0;
            }
            *current_bucket += 1;
        }
    };

    // Replay all segments, collecting DA_SLOT_ADD ops on counter storages.
    let num_segments = reader.segment_count();
    for seg_idx in 0..num_segments {
        let (_storages, items) = reader.segment_replay(seg_idx)?;

        for item in &items {
            if let TimedItem::Op(op) = item {
                if op.action == DA_SLOT_ADD {
                    if let Some(&ci) = counter_map.get(&op.storage_id) {
                        let cycle = op.time_ps / period_ps;
                        let bucket = cycle / base_interval as u64;

                        // Flush any completed buckets.
                        flush_buckets(
                            &mut current_bucket,
                            bucket,
                            &mut bucket_min,
                            &mut bucket_max,
                            &mut bucket_sum,
                            &mut level0,
                        );

                        // Accumulate into current bucket.
                        let delta = op.value;
                        bucket_min[ci] = bucket_min[ci].min(delta);
                        bucket_max[ci] = bucket_max[ci].max(delta);
                        bucket_sum[ci] += delta;
                    }
                }
            }
        }
    }

    // Flush the final (partial) bucket.
    let final_target = current_bucket + 1;
    flush_buckets(
        &mut current_bucket,
        final_target,
        &mut bucket_min,
        &mut bucket_max,
        &mut bucket_sum,
        &mut level0,
    );

    // Build higher mipmap levels by aggregating `fan_out` consecutive entries.
    let mut counters = Vec::with_capacity(num_counters);
    for (ci, (sid, name)) in counter_storages.iter().enumerate() {
        let mut levels = vec![std::mem::take(&mut level0[ci])];

        loop {
            let prev_level = levels.last().unwrap();
            if prev_level.len() <= 1 {
                break;
            }

            let next_level: Vec<MipmapEntry> = prev_level
                .chunks(fan_out as usize)
                .map(|chunk| MipmapEntry {
                    min_delta: chunk.iter().map(|e| e.min_delta).min().unwrap_or(0),
                    max_delta: chunk.iter().map(|e| e.max_delta).max().unwrap_or(0),
                    sum: chunk.iter().map(|e| e.sum).sum(),
                })
                .collect();
            levels.push(next_level);
        }

        counters.push(CounterMipmap {
            name: name.clone(),
            storage_id: *sid,
            levels,
        });
    }

    Ok(CounterSummary {
        base_interval_cycles: base_interval,
        fan_out,
        counters,
    })
}

// ── Legacy format helpers (unchanged) ──────────────────────────────

/// Read summary header and level descriptors (legacy wire format).
pub fn read_summary_header<R: Read>(r: &mut R) -> io::Result<(SummaryHeader, Vec<LevelDesc>)> {
    let header = SummaryHeader::read_from(r)?;
    let mut levels = Vec::with_capacity(header.num_levels as usize);
    for _ in 0..header.num_levels {
        levels.push(LevelDesc::read_from(r)?);
    }
    Ok((header, levels))
}

/// Write summary header and level descriptors (legacy wire format).
pub fn write_summary_header<W: Write>(
    w: &mut W,
    header: &SummaryHeader,
    levels: &[LevelDesc],
) -> io::Result<()> {
    header.write_to(w)?;
    for l in levels {
        l.write_to(w)?;
    }
    Ok(())
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocols::cpu::{CpuSchemaBuilder, CpuWriter};
    use crate::writer;

    /// Create a small trace with known counter deltas and verify mipmap
    /// computation produces correct min/max/sum values.
    #[test]
    fn mipmap_basic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("counters.uscope");

        // Build schema with one counter.
        let (dut_builder, mut sb, ids) = CpuSchemaBuilder::new("core0")
            .pipeline_stages(&["Fe", "De", "Ex", "Wb"])
            .entity_slots(16)
            .counter("insns")
            .build();
        let dut = dut_builder.build(sb.strings_mut());
        let schema = sb.build();

        let clock_period: u64 = 1000; // 1 ns

        // Write trace: 2048 cycles so we get exactly 2 level-0 buckets
        // (base_interval = 1024).
        let file = std::fs::File::create(&path).unwrap();
        let buf = std::io::BufWriter::new(file);
        let mut w = writer::Writer::create(buf, &dut, &schema, clock_period * 100_000).unwrap();
        let cpu = CpuWriter::new(ids);

        // Bucket 0: cycles 0..1023 — emit deltas of 1 on every cycle.
        for c in 0u64..1024 {
            w.begin_cycle(c * clock_period);
            cpu.counter_add(&mut w, "insns", 1);
            w.end_cycle().unwrap();
        }
        // Bucket 1: cycles 1024..2047 — emit deltas of 3 on every other cycle.
        for c in 1024u64..2048 {
            w.begin_cycle(c * clock_period);
            if c % 2 == 0 {
                cpu.counter_add(&mut w, "insns", 3);
            }
            w.end_cycle().unwrap();
        }

        w.close().unwrap();

        // Compute mipmap.
        let mut reader = Reader::open(path.to_str().unwrap()).unwrap();
        let summary = compute_counter_summary(&mut reader, clock_period).unwrap();

        assert_eq!(summary.base_interval_cycles, 1024);
        assert_eq!(summary.fan_out, 4);
        assert_eq!(summary.counters.len(), 1);

        let cm = &summary.counters[0];
        assert_eq!(cm.name, "insns");
        assert!(cm.levels.len() >= 1);

        let l0 = &cm.levels[0];
        assert_eq!(l0.len(), 2, "expected 2 level-0 buckets");

        // Bucket 0: 1024 deltas of 1.
        assert_eq!(l0[0].min_delta, 1);
        assert_eq!(l0[0].max_delta, 1);
        assert_eq!(l0[0].sum, 1024);

        // Bucket 1: 512 deltas of 3 (every other cycle).
        assert_eq!(l0[1].min_delta, 3);
        assert_eq!(l0[1].max_delta, 3);
        assert_eq!(l0[1].sum, 512 * 3);
    }

    #[test]
    fn mipmap_no_counters() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("no_counters.uscope");

        // Schema without counters.
        let (dut_builder, mut sb, _ids) = CpuSchemaBuilder::new("core0")
            .pipeline_stages(&["Fe", "De"])
            .entity_slots(16)
            .build();
        let dut = dut_builder.build(sb.strings_mut());
        let schema = sb.build();

        let file = std::fs::File::create(&path).unwrap();
        let buf = std::io::BufWriter::new(file);
        let mut w = writer::Writer::create(buf, &dut, &schema, 100_000).unwrap();
        w.begin_cycle(0);
        w.end_cycle().unwrap();
        w.close().unwrap();

        let mut reader = Reader::open(path.to_str().unwrap()).unwrap();
        let summary = compute_counter_summary(&mut reader, 1000).unwrap();
        assert!(summary.counters.is_empty());
    }

    #[test]
    fn mipmap_higher_levels() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("levels.uscope");

        let (dut_builder, mut sb, ids) = CpuSchemaBuilder::new("core0")
            .pipeline_stages(&["Fe", "De"])
            .entity_slots(16)
            .counter("ops")
            .build();
        let dut = dut_builder.build(sb.strings_mut());
        let schema = sb.build();

        let clock_period: u64 = 1000;
        // Write enough cycles to get multiple level-0 buckets:
        // 8 * 1024 = 8192 cycles -> 8 buckets -> level 1 has 2 -> level 2 has 1.
        let file = std::fs::File::create(&path).unwrap();
        let buf = std::io::BufWriter::new(file);
        let mut w = writer::Writer::create(buf, &dut, &schema, clock_period * 100_000).unwrap();
        let cpu = CpuWriter::new(ids);

        for c in 0u64..8192 {
            w.begin_cycle(c * clock_period);
            cpu.counter_add(&mut w, "ops", 1);
            w.end_cycle().unwrap();
        }
        w.close().unwrap();

        let mut reader = Reader::open(path.to_str().unwrap()).unwrap();
        let summary = compute_counter_summary(&mut reader, clock_period).unwrap();

        let cm = &summary.counters[0];
        assert_eq!(cm.levels[0].len(), 8, "level 0: 8 buckets");
        assert_eq!(cm.levels[1].len(), 2, "level 1: 2 buckets (fan_out=4)");
        assert_eq!(cm.levels[2].len(), 1, "level 2: 1 bucket");

        // Level 1, bucket 0 aggregates level-0 buckets 0..3.
        assert_eq!(cm.levels[1][0].sum, 4 * 1024);
        assert_eq!(cm.levels[1][0].min_delta, 1);
        assert_eq!(cm.levels[1][0].max_delta, 1);
    }
}
