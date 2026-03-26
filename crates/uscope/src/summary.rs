/// Trace summary (multi-resolution summary) computation for fast rendering.
///
/// Builds a pyramid of min/max/sum buckets over counter deltas and an
/// instruction density mipmap so the viewer can pick the appropriate
/// resolution level for the current zoom, and map rows to cycles instantly.
use crate::reader::Reader;
use crate::state::TimedItem;
use crate::types::*;
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::collections::HashMap;
use std::io::{self, Read, Seek, SeekFrom, Write};

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

/// Trace summary with instruction density mipmap and counter mipmaps.
#[derive(Debug, Clone)]
pub struct TraceSummary {
    /// Number of cycles per level-0 bucket.
    pub base_interval_cycles: u32,
    /// Number of child buckets aggregated into one parent bucket.
    pub fan_out: u32,
    /// Total number of instructions in the trace.
    pub total_instructions: u64,
    /// Instruction count per bucket at each mipmap level.
    /// Level 0 = instruction count per base_interval_cycles bucket.
    /// Level N = aggregated by fan_out from level N-1.
    pub instruction_density: Vec<Vec<u32>>,
    /// Per-counter mipmaps.
    pub counters: Vec<CounterMipmap>,
}

/// Backward-compatible type alias.
pub type CounterSummary = TraceSummary;

impl TraceSummary {
    /// Find the approximate cycle for a global instruction row index.
    /// Uses prefix-sum over level-0 density buckets.
    pub fn row_to_cycle(&self, row: usize) -> u32 {
        if self.instruction_density.is_empty() {
            return 0;
        }
        let level0 = &self.instruction_density[0];
        let mut cumulative = 0usize;
        for (bucket, &count) in level0.iter().enumerate() {
            cumulative += count as usize;
            if cumulative > row {
                return (bucket as u32) * self.base_interval_cycles;
            }
        }
        (level0.len() as u32) * self.base_interval_cycles
    }

    /// Find the approximate global instruction index for a given cycle.
    /// Inverse of `row_to_cycle`: sums instruction counts in density buckets
    /// up to the bucket containing the given cycle.
    pub fn cycle_to_row(&self, cycle: u32) -> usize {
        if self.instruction_density.is_empty() {
            return 0;
        }
        let level0 = &self.instruction_density[0];
        let base = self.base_interval_cycles;
        let target_bucket = (cycle / base) as usize;
        let mut cumulative = 0usize;
        for (bucket, &count) in level0.iter().enumerate() {
            if bucket >= target_bucket {
                // Interpolate within this bucket based on cycle position.
                let cycle_in_bucket = cycle.saturating_sub(bucket as u32 * base);
                let fraction = cycle_in_bucket as f64 / base as f64;
                let offset = (fraction * count as f64) as usize;
                return cumulative + offset;
            }
            cumulative += count as usize;
        }
        cumulative
    }
    /// Get the approximate cumulative counter value at a given cycle.
    /// Prefix-sums level-0 `sum` fields up to the target bucket.
    pub fn counter_value_at(&self, counter_idx: usize, cycle: u32) -> u64 {
        if counter_idx >= self.counters.len() {
            return 0;
        }
        let mipmap = &self.counters[counter_idx];
        if mipmap.levels.is_empty() {
            return 0;
        }
        let level0 = &mipmap.levels[0];
        let base = self.base_interval_cycles;
        let target_bucket = (cycle / base) as usize;
        let mut cumulative = 0u64;
        for (bucket, entry) in level0.iter().enumerate() {
            if bucket >= target_bucket {
                // Interpolate within the bucket.
                let cycle_in_bucket = cycle.saturating_sub(bucket as u32 * base);
                let fraction = cycle_in_bucket as f64 / base as f64;
                cumulative += (fraction * entry.sum as f64) as u64;
                break;
            }
            cumulative += entry.sum;
        }
        cumulative
    }
}

// ── Computation ────────────────────────────────────────────────────

/// Compute trace summary (counter mipmaps + instruction density) by replaying
/// all segments from a trace file.
///
/// `period_ps` is the clock period in picoseconds (used to convert absolute
/// timestamps to cycle numbers).  Typically obtained from the schema's
/// clock domain definition.
///
/// Returns `Ok(TraceSummary)` with level-0 through level-N mipmap data.
/// If the trace contains no counter storages the result will have an empty
/// `counters` vector (but may still have instruction density data).
pub fn compute_trace_summary(reader: &mut Reader, period_ps: u64) -> io::Result<TraceSummary> {
    let base_interval: u32 = 1024; // power-of-two for clean alignment
    let fan_out: u32 = 4;

    if period_ps == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "period_ps must be > 0",
        ));
    }

    let schema = reader.schema().clone();

    // Identify the entities storage: sparse, has an "entity_id" field.
    let entities_info: Option<(u16, u16)> = schema
        .storages
        .iter()
        .filter(|s| s.is_sparse() && !s.is_buffer())
        .find_map(|s| {
            for (fi, f) in s.fields.iter().enumerate() {
                if let Some(name) = schema.get_string(f.name) {
                    if name == "entity_id" {
                        return Some((s.storage_id, fi as u16));
                    }
                }
            }
            None
        });

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

    let has_entities = entities_info.is_some();
    let has_counters = !counter_storages.is_empty();

    if !has_entities && !has_counters {
        return Ok(TraceSummary {
            base_interval_cycles: base_interval,
            fan_out,
            total_instructions: 0,
            instruction_density: vec![],
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
    let mut counter_level0: Vec<Vec<MipmapEntry>> = (0..num_counters).map(|_| Vec::new()).collect();

    // Instruction density level-0 accumulator.
    let mut density_level0: Vec<u32> = Vec::new();
    let mut density_bucket_count: u32 = 0;
    let mut total_instructions: u64 = 0;

    let mut current_bucket: u64 = 0;

    // Flush completed buckets up to (but not including) `target_bucket`.
    let flush_buckets = |current_bucket: &mut u64,
                         target_bucket: u64,
                         bucket_min: &mut [u64],
                         bucket_max: &mut [u64],
                         bucket_sum: &mut [u64],
                         counter_level0: &mut [Vec<MipmapEntry>],
                         density_level0: &mut Vec<u32>,
                         density_bucket_count: &mut u32| {
        while *current_bucket < target_bucket {
            for c in 0..num_counters {
                let min_val = if bucket_min[c] == u64::MAX {
                    0
                } else {
                    bucket_min[c]
                };
                counter_level0[c].push(MipmapEntry {
                    min_delta: min_val,
                    max_delta: bucket_max[c],
                    sum: bucket_sum[c],
                });
                bucket_min[c] = u64::MAX;
                bucket_max[c] = 0;
                bucket_sum[c] = 0;
            }
            density_level0.push(*density_bucket_count);
            *density_bucket_count = 0;
            *current_bucket += 1;
        }
    };

    // Replay all segments.
    let num_segments = reader.segment_count();
    for seg_idx in 0..num_segments {
        let (_storages, items) = reader.segment_replay(seg_idx)?;

        for item in &items {
            if let TimedItem::Op(op) = item {
                let cycle = op.time_ps / period_ps;
                let bucket = cycle / base_interval as u64;

                // Counter: DA_SLOT_ADD on counter storages.
                if op.action == DA_SLOT_ADD {
                    if let Some(&ci) = counter_map.get(&op.storage_id) {
                        // Flush any completed buckets.
                        flush_buckets(
                            &mut current_bucket,
                            bucket,
                            &mut bucket_min,
                            &mut bucket_max,
                            &mut bucket_sum,
                            &mut counter_level0,
                            &mut density_level0,
                            &mut density_bucket_count,
                        );

                        // Accumulate into current bucket.
                        let delta = op.value;
                        bucket_min[ci] = bucket_min[ci].min(delta);
                        bucket_max[ci] = bucket_max[ci].max(delta);
                        bucket_sum[ci] += delta;
                    }
                }

                // Instruction density: DA_SLOT_SET on entities storage, field_entity_id.
                if op.action == DA_SLOT_SET {
                    if let Some((ent_sid, ent_fid)) = entities_info {
                        if op.storage_id == ent_sid && op.field_index == ent_fid {
                            // Flush any completed buckets.
                            flush_buckets(
                                &mut current_bucket,
                                bucket,
                                &mut bucket_min,
                                &mut bucket_max,
                                &mut bucket_sum,
                                &mut counter_level0,
                                &mut density_level0,
                                &mut density_bucket_count,
                            );

                            density_bucket_count += 1;
                            total_instructions += 1;
                        }
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
        &mut counter_level0,
        &mut density_level0,
        &mut density_bucket_count,
    );

    // Build higher mipmap levels for counters.
    let mut counters = Vec::with_capacity(num_counters);
    for (ci, (sid, name)) in counter_storages.iter().enumerate() {
        let mut levels = vec![std::mem::take(&mut counter_level0[ci])];

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

    // Build higher mipmap levels for instruction density.
    let mut instruction_density: Vec<Vec<u32>> = vec![density_level0];
    loop {
        let prev = instruction_density.last().unwrap();
        if prev.len() <= 1 {
            break;
        }
        let next: Vec<u32> = prev
            .chunks(fan_out as usize)
            .map(|chunk| chunk.iter().sum())
            .collect();
        instruction_density.push(next);
    }

    Ok(TraceSummary {
        base_interval_cycles: base_interval,
        fan_out,
        total_instructions,
        instruction_density,
        counters,
    })
}

/// Backward-compatible alias.
pub fn compute_counter_summary(reader: &mut Reader, period_ps: u64) -> io::Result<TraceSummary> {
    compute_trace_summary(reader, period_ps)
}

// ── Serialization ─────────────────────────────────────────────────

const TRACE_SUMMARY_MAGIC: &[u8; 4] = b"TSUM";
const COUNTER_SUMMARY_MAGIC: &[u8; 4] = b"CSUM";

/// Serialize a `TraceSummary` to a self-contained byte vector.
///
/// Format:
/// - 4 bytes: magic `b"TSUM"`
/// - 4 bytes: base_interval_cycles (u32 LE)
/// - 4 bytes: fan_out (u32 LE)
/// - 8 bytes: total_instructions (u64 LE)
/// - 4 bytes: num_density_levels (u32 LE)
/// - For each density level:
///   - 4 bytes: num_entries (u32 LE)
///   - num_entries * 4 bytes: instruction counts (u32 LE each)
/// - 4 bytes: num_counters (u32 LE)
/// - For each counter:
///   - 4 bytes: name_len (u32 LE)
///   - name_len bytes: name (UTF-8)
///   - 2 bytes: storage_id (u16 LE)
///   - 4 bytes: num_levels (u32 LE)
///   - For each level:
///     - 4 bytes: num_entries (u32 LE)
///     - For each entry: min_delta(u64) + max_delta(u64) + sum(u64) = 24 bytes
pub fn serialize_trace_summary(summary: &TraceSummary) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(TRACE_SUMMARY_MAGIC);
    buf.write_u32::<LittleEndian>(summary.base_interval_cycles)
        .unwrap();
    buf.write_u32::<LittleEndian>(summary.fan_out).unwrap();
    buf.write_u64::<LittleEndian>(summary.total_instructions)
        .unwrap();

    // Instruction density levels.
    buf.write_u32::<LittleEndian>(summary.instruction_density.len() as u32)
        .unwrap();
    for level in &summary.instruction_density {
        buf.write_u32::<LittleEndian>(level.len() as u32).unwrap();
        for &count in level {
            buf.write_u32::<LittleEndian>(count).unwrap();
        }
    }

    // Counter mipmaps.
    buf.write_u32::<LittleEndian>(summary.counters.len() as u32)
        .unwrap();

    for counter in &summary.counters {
        let name_bytes = counter.name.as_bytes();
        buf.write_u32::<LittleEndian>(name_bytes.len() as u32)
            .unwrap();
        buf.extend_from_slice(name_bytes);
        buf.write_u16::<LittleEndian>(counter.storage_id).unwrap();
        buf.write_u32::<LittleEndian>(counter.levels.len() as u32)
            .unwrap();

        for level in &counter.levels {
            buf.write_u32::<LittleEndian>(level.len() as u32).unwrap();
            for entry in level {
                buf.write_u64::<LittleEndian>(entry.min_delta).unwrap();
                buf.write_u64::<LittleEndian>(entry.max_delta).unwrap();
                buf.write_u64::<LittleEndian>(entry.sum).unwrap();
            }
        }
    }

    buf
}

/// Backward-compatible alias.
pub fn serialize_counter_summary(summary: &TraceSummary) -> Vec<u8> {
    serialize_trace_summary(summary)
}

/// Deserialize a `TraceSummary` from bytes.
///
/// Handles both TSUM (new, with density) and CSUM (old, counter-only) magic.
pub fn deserialize_trace_summary(data: &[u8]) -> io::Result<TraceSummary> {
    let mut r = io::Cursor::new(data);

    let mut magic = [0u8; 4];
    r.read_exact(&mut magic)?;

    if &magic == TRACE_SUMMARY_MAGIC {
        // New TSUM format with instruction density.
        let base_interval_cycles = r.read_u32::<LittleEndian>()?;
        let fan_out = r.read_u32::<LittleEndian>()?;
        let total_instructions = r.read_u64::<LittleEndian>()?;

        // Density levels.
        let num_density_levels = r.read_u32::<LittleEndian>()? as usize;
        let mut instruction_density = Vec::with_capacity(num_density_levels);
        for _ in 0..num_density_levels {
            let num_entries = r.read_u32::<LittleEndian>()? as usize;
            let mut entries = Vec::with_capacity(num_entries);
            for _ in 0..num_entries {
                entries.push(r.read_u32::<LittleEndian>()?);
            }
            instruction_density.push(entries);
        }

        // Counter mipmaps.
        let num_counters = r.read_u32::<LittleEndian>()? as usize;
        let counters = read_counter_mipmaps(&mut r, num_counters)?;

        Ok(TraceSummary {
            base_interval_cycles,
            fan_out,
            total_instructions,
            instruction_density,
            counters,
        })
    } else if &magic == COUNTER_SUMMARY_MAGIC {
        // Old CSUM format — wrap in TraceSummary with empty density.
        let base_interval_cycles = r.read_u32::<LittleEndian>()?;
        let fan_out = r.read_u32::<LittleEndian>()?;
        let num_counters = r.read_u32::<LittleEndian>()? as usize;
        let counters = read_counter_mipmaps(&mut r, num_counters)?;

        Ok(TraceSummary {
            base_interval_cycles,
            fan_out,
            total_instructions: 0,
            instruction_density: vec![],
            counters,
        })
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid trace summary magic (expected TSUM or CSUM)",
        ))
    }
}

/// Backward-compatible alias.
pub fn deserialize_counter_summary(data: &[u8]) -> io::Result<TraceSummary> {
    deserialize_trace_summary(data)
}

/// Read counter mipmap data from a cursor (shared by TSUM and CSUM paths).
fn read_counter_mipmaps<R: io::Read>(
    r: &mut R,
    num_counters: usize,
) -> io::Result<Vec<CounterMipmap>> {
    let mut counters = Vec::with_capacity(num_counters);
    for _ in 0..num_counters {
        let name_len = r.read_u32::<LittleEndian>()? as usize;
        let mut name_bytes = vec![0u8; name_len];
        r.read_exact(&mut name_bytes)?;
        let name = String::from_utf8(name_bytes).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid UTF-8 counter name: {}", e),
            )
        })?;
        let storage_id = r.read_u16::<LittleEndian>()?;
        let num_levels = r.read_u32::<LittleEndian>()? as usize;

        let mut levels = Vec::with_capacity(num_levels);
        for _ in 0..num_levels {
            let num_entries = r.read_u32::<LittleEndian>()? as usize;
            let mut entries = Vec::with_capacity(num_entries);
            for _ in 0..num_entries {
                entries.push(MipmapEntry {
                    min_delta: r.read_u64::<LittleEndian>()?,
                    max_delta: r.read_u64::<LittleEndian>()?,
                    sum: r.read_u64::<LittleEndian>()?,
                });
            }
            levels.push(entries);
        }

        counters.push(CounterMipmap {
            name,
            storage_id,
            levels,
        });
    }
    Ok(counters)
}

// ── Embedding into .uscope files ──────────────────────────────────

/// Compute trace summary (counter mipmaps + instruction density) and embed
/// inside a finalized `.uscope` file.
///
/// This must be called **after** `Writer::close()`.  It re-opens the file,
/// replays all segments to build the summary, then appends the serialized
/// data and rewrites the section table so the reader can find it.
///
/// `period_ps` is the clock period in picoseconds (used to convert absolute
/// timestamps to cycle numbers).
pub fn embed_trace_summary(path: &str, period_ps: u64) -> io::Result<()> {
    // 1. Open with Reader, compute summary.
    let mut reader = Reader::open(path)?;
    let summary = compute_trace_summary(&mut reader, period_ps)?;
    if summary.counters.is_empty() && summary.total_instructions == 0 {
        return Ok(());
    }
    let data = serialize_trace_summary(&summary);
    // Capture what we need from the reader before dropping it.
    let header = reader.header().clone();
    drop(reader);

    // 2. Re-open the file for read+write.
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;

    // 3. Read existing section table entries.
    if header.section_table_offset == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file has no section table (not finalized?)",
        ));
    }

    file.seek(SeekFrom::Start(header.section_table_offset))?;
    let mut existing_sections = Vec::new();
    let mut old_summary_offset: Option<u64> = None;
    loop {
        let entry = SectionEntry::read_from(&mut file)?;
        if entry.section_type == SECTION_END {
            break;
        }
        if entry.section_type == SECTION_COUNTER_SUMMARY {
            // Remember where the old summary data starts so we can overwrite it.
            old_summary_offset = Some(entry.offset);
        } else {
            existing_sections.push(entry);
        }
    }

    // 4. Determine write position: reuse old summary data offset if present,
    //    otherwise start at the section table offset.
    let write_start = old_summary_offset.unwrap_or(header.section_table_offset);
    file.seek(SeekFrom::Start(write_start))?;

    // Write summary data blob.
    let summary_offset = write_start;
    let summary_size = data.len() as u64;
    file.write_all(&data)?;

    // Pad to 8-byte alignment before section table.
    let pos = file.stream_position()?;
    let pad = (8 - (pos % 8)) % 8;
    if pad > 0 {
        file.write_all(&vec![0u8; pad as usize])?;
    }

    let new_section_table_offset = file.stream_position()?;

    // Write original section entries + new counter summary entry.
    for s in &existing_sections {
        s.write_to(&mut file)?;
    }

    SectionEntry {
        section_type: SECTION_COUNTER_SUMMARY,
        flags: 0,
        reserved: 0,
        offset: summary_offset,
        size: summary_size,
    }
    .write_to(&mut file)?;

    // End sentinel.
    SectionEntry {
        section_type: SECTION_END,
        flags: 0,
        reserved: 0,
        offset: 0,
        size: 0,
    }
    .write_to(&mut file)?;

    // Truncate the file in case the new content is shorter than the old tail.
    let end_pos = file.stream_position()?;
    file.set_len(end_pos)?;

    // 5. Rewrite header with updated section_table_offset.
    let mut new_header = header;
    new_header.section_table_offset = new_section_table_offset;
    file.seek(SeekFrom::Start(0))?;
    new_header.write_to(&mut file)?;

    file.flush()?;

    Ok(())
}

/// Backward-compatible alias.
pub fn embed_counter_summary(path: &str, period_ps: u64) -> io::Result<()> {
    embed_trace_summary(path, period_ps)
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
        let summary = compute_trace_summary(&mut reader, clock_period).unwrap();

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
        let summary = compute_trace_summary(&mut reader, 1000).unwrap();
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
        let summary = compute_trace_summary(&mut reader, clock_period).unwrap();

        let cm = &summary.counters[0];
        assert_eq!(cm.levels[0].len(), 8, "level 0: 8 buckets");
        assert_eq!(cm.levels[1].len(), 2, "level 1: 2 buckets (fan_out=4)");
        assert_eq!(cm.levels[2].len(), 1, "level 2: 1 bucket");

        // Level 1, bucket 0 aggregates level-0 buckets 0..3.
        assert_eq!(cm.levels[1][0].sum, 4 * 1024);
        assert_eq!(cm.levels[1][0].min_delta, 1);
        assert_eq!(cm.levels[1][0].max_delta, 1);
    }

    #[test]
    fn serialize_deserialize_roundtrip() {
        let summary = TraceSummary {
            base_interval_cycles: 1024,
            fan_out: 4,
            total_instructions: 300,
            instruction_density: vec![vec![100, 200], vec![300]],
            counters: vec![
                CounterMipmap {
                    name: "insns".to_string(),
                    storage_id: 3,
                    levels: vec![
                        vec![
                            MipmapEntry {
                                min_delta: 1,
                                max_delta: 5,
                                sum: 100,
                            },
                            MipmapEntry {
                                min_delta: 0,
                                max_delta: 10,
                                sum: 200,
                            },
                        ],
                        vec![MipmapEntry {
                            min_delta: 0,
                            max_delta: 10,
                            sum: 300,
                        }],
                    ],
                },
                CounterMipmap {
                    name: "cycles".to_string(),
                    storage_id: 4,
                    levels: vec![vec![MipmapEntry {
                        min_delta: 1,
                        max_delta: 1,
                        sum: 1024,
                    }]],
                },
            ],
        };

        let data = serialize_trace_summary(&summary);
        let decoded = deserialize_trace_summary(&data).unwrap();

        assert_eq!(decoded.base_interval_cycles, summary.base_interval_cycles);
        assert_eq!(decoded.fan_out, summary.fan_out);
        assert_eq!(decoded.total_instructions, summary.total_instructions);
        assert_eq!(
            decoded.instruction_density.len(),
            summary.instruction_density.len()
        );
        for (orig, dec) in summary
            .instruction_density
            .iter()
            .zip(decoded.instruction_density.iter())
        {
            assert_eq!(dec, orig);
        }
        assert_eq!(decoded.counters.len(), summary.counters.len());

        for (orig, dec) in summary.counters.iter().zip(decoded.counters.iter()) {
            assert_eq!(dec.name, orig.name);
            assert_eq!(dec.storage_id, orig.storage_id);
            assert_eq!(dec.levels.len(), orig.levels.len());

            for (ol, dl) in orig.levels.iter().zip(dec.levels.iter()) {
                assert_eq!(dl.len(), ol.len());
                for (oe, de) in ol.iter().zip(dl.iter()) {
                    assert_eq!(de.min_delta, oe.min_delta);
                    assert_eq!(de.max_delta, oe.max_delta);
                    assert_eq!(de.sum, oe.sum);
                }
            }
        }
    }

    #[test]
    fn serialize_deserialize_empty() {
        let summary = TraceSummary {
            base_interval_cycles: 512,
            fan_out: 2,
            total_instructions: 0,
            instruction_density: vec![],
            counters: vec![],
        };

        let data = serialize_trace_summary(&summary);
        let decoded = deserialize_trace_summary(&data).unwrap();

        assert_eq!(decoded.base_interval_cycles, 512);
        assert_eq!(decoded.fan_out, 2);
        assert_eq!(decoded.total_instructions, 0);
        assert!(decoded.instruction_density.is_empty());
        assert!(decoded.counters.is_empty());
    }

    #[test]
    fn deserialize_bad_magic() {
        let data = b"BADMrest of data...";
        let result = deserialize_trace_summary(data);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("magic"), "error should mention magic: {}", msg);
    }

    #[test]
    fn deserialize_legacy_csum() {
        // Build a CSUM-format blob (old format) and verify it deserializes.
        let mut buf = Vec::new();
        buf.extend_from_slice(b"CSUM");
        buf.write_u32::<LittleEndian>(1024).unwrap(); // base_interval
        buf.write_u32::<LittleEndian>(4).unwrap(); // fan_out
        buf.write_u32::<LittleEndian>(1).unwrap(); // num_counters

        // One counter: "ops", storage_id=2, 1 level, 1 entry
        let name = b"ops";
        buf.write_u32::<LittleEndian>(name.len() as u32).unwrap();
        buf.extend_from_slice(name);
        buf.write_u16::<LittleEndian>(2).unwrap(); // storage_id
        buf.write_u32::<LittleEndian>(1).unwrap(); // num_levels
        buf.write_u32::<LittleEndian>(1).unwrap(); // num_entries in level 0
        buf.write_u64::<LittleEndian>(1).unwrap(); // min_delta
        buf.write_u64::<LittleEndian>(5).unwrap(); // max_delta
        buf.write_u64::<LittleEndian>(100).unwrap(); // sum

        let decoded = deserialize_trace_summary(&buf).unwrap();
        assert_eq!(decoded.base_interval_cycles, 1024);
        assert_eq!(decoded.fan_out, 4);
        assert_eq!(decoded.total_instructions, 0);
        assert!(decoded.instruction_density.is_empty());
        assert_eq!(decoded.counters.len(), 1);
        assert_eq!(decoded.counters[0].name, "ops");
        assert_eq!(decoded.counters[0].storage_id, 2);
        assert_eq!(decoded.counters[0].levels[0][0].sum, 100);
    }

    #[test]
    fn embed_trace_summary_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("embedded.uscope");

        let (dut_builder, mut sb, ids) = CpuSchemaBuilder::new("core0")
            .pipeline_stages(&["Fe", "De", "Ex", "Wb"])
            .entity_slots(16)
            .counter("insns")
            .build();
        let dut = dut_builder.build(sb.strings_mut());
        let schema = sb.build();

        let clock_period: u64 = 1000;

        let file = std::fs::File::create(&path).unwrap();
        let buf = std::io::BufWriter::new(file);
        let mut w = writer::Writer::create(buf, &dut, &schema, clock_period * 100_000).unwrap();
        let cpu = CpuWriter::new(ids);

        for c in 0u64..2048 {
            w.begin_cycle(c * clock_period);
            cpu.counter_add(&mut w, "insns", 1);
            w.end_cycle().unwrap();
        }
        w.close().unwrap();

        // Before embedding: no summary in file.
        let reader = Reader::open(path.to_str().unwrap()).unwrap();
        assert!(reader.trace_summary().is_none());
        drop(reader);

        // Embed trace summary.
        embed_trace_summary(path.to_str().unwrap(), clock_period).unwrap();

        // Re-open: should now have the summary loaded automatically.
        let reader2 = Reader::open(path.to_str().unwrap()).unwrap();
        let loaded_summary = reader2.trace_summary().expect("summary should be embedded");
        assert_eq!(loaded_summary.counters.len(), 1);
        assert_eq!(loaded_summary.counters[0].name, "insns");
        assert!(loaded_summary.counters[0].levels[0].len() >= 2);
        assert_eq!(loaded_summary.counters[0].levels[0][0].sum, 1024);

        // The file should still have valid segments and string table.
        assert!(reader2.segment_count() >= 1);
    }

    #[test]
    fn embed_no_counters_no_entities_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("no_counters_embed.uscope");

        // Schema without counters but with entities (CPU schema always has entities).
        // We need a schema truly without entities. Use SchemaBuilder directly.
        // Actually, CpuSchemaBuilder always creates an entities storage.
        // The embed_trace_summary only skips if both counters and total_instructions are 0.
        // With entities but no fetches, total_instructions will be 0.
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

        let size_before = std::fs::metadata(&path).unwrap().len();

        // No instructions fetched, no counters -> should still embed because
        // entities storage exists (density data will be empty but present).
        // Actually with 0 total_instructions and empty counters, embed is noop.
        embed_trace_summary(path.to_str().unwrap(), 1000).unwrap();

        let size_after = std::fs::metadata(&path).unwrap().len();
        assert_eq!(size_before, size_after, "file should not change");

        let reader = Reader::open(path.to_str().unwrap()).unwrap();
        assert!(reader.trace_summary().is_none());
    }

    #[test]
    fn embed_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("idempotent.uscope");

        let (dut_builder, mut sb, ids) = CpuSchemaBuilder::new("core0")
            .pipeline_stages(&["Fe", "De", "Ex", "Wb"])
            .entity_slots(16)
            .counter("ops")
            .build();
        let dut = dut_builder.build(sb.strings_mut());
        let schema = sb.build();

        let clock_period: u64 = 1000;

        let file = std::fs::File::create(&path).unwrap();
        let buf = std::io::BufWriter::new(file);
        let mut w = writer::Writer::create(buf, &dut, &schema, clock_period * 100_000).unwrap();
        let cpu = CpuWriter::new(ids);

        for c in 0u64..2048 {
            w.begin_cycle(c * clock_period);
            cpu.counter_add(&mut w, "ops", 1);
            w.end_cycle().unwrap();
        }
        w.close().unwrap();

        // Embed twice; second call should produce identical result.
        embed_trace_summary(path.to_str().unwrap(), clock_period).unwrap();
        let size_first = std::fs::metadata(&path).unwrap().len();

        embed_trace_summary(path.to_str().unwrap(), clock_period).unwrap();
        let size_second = std::fs::metadata(&path).unwrap().len();

        assert_eq!(
            size_first, size_second,
            "embedding twice should be idempotent"
        );

        let reader = Reader::open(path.to_str().unwrap()).unwrap();
        let summary = reader.trace_summary().expect("summary should exist");
        assert_eq!(summary.counters.len(), 1);
        assert_eq!(summary.counters[0].levels[0][0].sum, 1024);
    }

    /// Test instruction density: create a trace with known entity creates,
    /// verify density buckets and row_to_cycle.
    #[test]
    fn instruction_density_basic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("density.uscope");

        let (dut_builder, mut sb, ids) = CpuSchemaBuilder::new("core0")
            .pipeline_stages(&["Fe", "De", "Ex", "Wb"])
            .entity_slots(512)
            .build();
        let dut = dut_builder.build(sb.strings_mut());
        let schema = sb.build();

        let clock_period: u64 = 1000;

        let file = std::fs::File::create(&path).unwrap();
        let buf = std::io::BufWriter::new(file);
        let mut w = writer::Writer::create(buf, &dut, &schema, clock_period * 100_000).unwrap();
        let cpu = CpuWriter::new(ids);

        // Bucket 0 (cycles 0..1023): fetch 100 instructions.
        let mut eid: u32 = 0;
        for c in 0u64..100 {
            w.begin_cycle(c * clock_period);
            cpu.fetch(&mut w, eid, 0x80000000 + eid as u64 * 4, 0x13);
            cpu.stage_transition(&mut w, eid, 0);
            eid += 1;
            w.end_cycle().unwrap();
        }
        // Fill remaining cycles in bucket 0 with empty cycles.
        for c in 100u64..1024 {
            w.begin_cycle(c * clock_period);
            w.end_cycle().unwrap();
        }

        // Bucket 1 (cycles 1024..2047): fetch 50 instructions.
        for c in 1024u64..1074 {
            w.begin_cycle(c * clock_period);
            cpu.fetch(&mut w, eid, 0x80000000 + eid as u64 * 4, 0x13);
            cpu.stage_transition(&mut w, eid, 0);
            eid += 1;
            w.end_cycle().unwrap();
        }
        for c in 1074u64..2048 {
            w.begin_cycle(c * clock_period);
            w.end_cycle().unwrap();
        }

        w.close().unwrap();

        let mut reader = Reader::open(path.to_str().unwrap()).unwrap();
        let summary = compute_trace_summary(&mut reader, clock_period).unwrap();

        assert_eq!(summary.total_instructions, 150);
        assert!(!summary.instruction_density.is_empty());

        let d0 = &summary.instruction_density[0];
        assert_eq!(d0.len(), 2, "expected 2 density buckets");
        assert_eq!(d0[0], 100, "bucket 0 should have 100 instructions");
        assert_eq!(d0[1], 50, "bucket 1 should have 50 instructions");

        // Test row_to_cycle.
        assert_eq!(summary.row_to_cycle(0), 0); // row 0 is in bucket 0
        assert_eq!(summary.row_to_cycle(99), 0); // row 99 is still in bucket 0
        assert_eq!(summary.row_to_cycle(100), 1024); // row 100 is in bucket 1
        assert_eq!(summary.row_to_cycle(149), 1024); // row 149 is in bucket 1
        assert_eq!(summary.row_to_cycle(150), 2048); // row 150 is past the end
    }

    #[test]
    fn instruction_density_with_counters() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("density_counters.uscope");

        let (dut_builder, mut sb, ids) = CpuSchemaBuilder::new("core0")
            .pipeline_stages(&["Fe", "De", "Ex", "Wb"])
            .entity_slots(512)
            .counter("insns")
            .build();
        let dut = dut_builder.build(sb.strings_mut());
        let schema = sb.build();

        let clock_period: u64 = 1000;

        let file = std::fs::File::create(&path).unwrap();
        let buf = std::io::BufWriter::new(file);
        let mut w = writer::Writer::create(buf, &dut, &schema, clock_period * 100_000).unwrap();
        let cpu = CpuWriter::new(ids);

        // Emit 20 instructions with counter increments over 2048 cycles.
        let mut eid: u32 = 0;
        for c in 0u64..20 {
            w.begin_cycle(c * clock_period);
            cpu.fetch(&mut w, eid, 0x80000000 + eid as u64 * 4, 0x13);
            cpu.counter_add(&mut w, "insns", 1);
            eid += 1;
            w.end_cycle().unwrap();
        }
        for c in 20u64..2048 {
            w.begin_cycle(c * clock_period);
            w.end_cycle().unwrap();
        }

        w.close().unwrap();

        let mut reader = Reader::open(path.to_str().unwrap()).unwrap();
        let summary = compute_trace_summary(&mut reader, clock_period).unwrap();

        assert_eq!(summary.total_instructions, 20);
        assert_eq!(summary.counters.len(), 1);
        assert_eq!(summary.counters[0].name, "insns");

        // Density: all 20 instructions in bucket 0.
        assert_eq!(summary.instruction_density[0][0], 20);
    }

    #[test]
    fn row_to_cycle_empty_density() {
        let summary = TraceSummary {
            base_interval_cycles: 1024,
            fan_out: 4,
            total_instructions: 0,
            instruction_density: vec![],
            counters: vec![],
        };
        assert_eq!(summary.row_to_cycle(0), 0);
        assert_eq!(summary.row_to_cycle(100), 0);
    }

    #[test]
    fn embed_trace_summary_with_density() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("embed_density.uscope");

        let (dut_builder, mut sb, ids) = CpuSchemaBuilder::new("core0")
            .pipeline_stages(&["Fe", "De", "Ex", "Wb"])
            .entity_slots(512)
            .counter("ops")
            .build();
        let dut = dut_builder.build(sb.strings_mut());
        let schema = sb.build();

        let clock_period: u64 = 1000;

        let file = std::fs::File::create(&path).unwrap();
        let buf = std::io::BufWriter::new(file);
        let mut w = writer::Writer::create(buf, &dut, &schema, clock_period * 100_000).unwrap();
        let cpu = CpuWriter::new(ids);

        // 10 instructions + counter increments.
        let mut eid: u32 = 0;
        for c in 0u64..10 {
            w.begin_cycle(c * clock_period);
            cpu.fetch(&mut w, eid, 0x80000000 + eid as u64 * 4, 0x13);
            cpu.counter_add(&mut w, "ops", 1);
            eid += 1;
            w.end_cycle().unwrap();
        }
        for c in 10u64..2048 {
            w.begin_cycle(c * clock_period);
            w.end_cycle().unwrap();
        }

        w.close().unwrap();

        embed_trace_summary(path.to_str().unwrap(), clock_period).unwrap();

        let reader = Reader::open(path.to_str().unwrap()).unwrap();
        let summary = reader.trace_summary().expect("summary should exist");

        assert_eq!(summary.total_instructions, 10);
        assert_eq!(summary.instruction_density[0][0], 10);
        assert_eq!(summary.counters.len(), 1);
        assert_eq!(summary.counters[0].name, "ops");
        assert_eq!(summary.row_to_cycle(5), 0);
    }
}
