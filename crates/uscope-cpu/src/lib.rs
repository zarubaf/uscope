//! CPU protocol library for uScope traces.
//!
//! Provides `CpuTrace` as the main entry point for opening and querying
//! CPU pipeline trace files in the uScope binary format. Sits between the
//! transport layer (`uscope` crate) and consumers (CLI, MCP, Reflex GUI).

pub mod buffers;
pub mod builder;
pub mod counters;
pub mod decode;
pub mod resolve;
pub mod types;

use std::collections::HashMap;
use std::io;
use std::path::Path;

use uscope::reader::Reader;
use uscope::state::TimedItem;
use uscope::summary::TraceSummary;
use uscope::types::*;

use builder::InstrBuilder;
use resolve::CpuProtocolIds;
use types::*;

/// Main entry point: a CPU trace session.
///
/// Wraps a `Reader` and resolved protocol IDs, providing high-level query
/// methods for counters, buffers, instructions, and metadata.
pub struct CpuTrace {
    reader: Reader,
    ids: CpuProtocolIds,
    segment_index: SegmentIndex,
    summary: Option<TraceSummary>,
    /// Mapping from uscope pipeline_stage enum index to interned StageNameIdx.
    stage_name_indices: Vec<u16>,
    /// Counter storages: (storage_id, name).
    counter_storages: Vec<(u16, String)>,
    /// Buffer storages detected from the schema.
    buffer_infos: Vec<BufferInfo>,
    /// Key-value metadata from the trace source.
    metadata: Vec<(String, String)>,
    /// Clock period in picoseconds.
    period_ps: u64,
    /// Maximum cycle in the trace.
    max_cycle: u32,
    /// Sparse per-cycle counter samples (populated for small traces).
    counter_series: Vec<CounterSeries>,
    /// Map from counter storage_id to index into counter_series.
    counter_storage_map: HashMap<u16, usize>,
    /// Instruction decoder (built once, reused for all segment loads).
    #[cfg(feature = "decode")]
    decoder: Option<instruction_decoder::Decoder>,
}

impl CpuTrace {
    /// Open a uscope trace file and resolve the CPU protocol.
    ///
    /// Reads metadata, schema, segment index, and optionally the embedded
    /// TraceSummary. Does NOT replay any segments (lazy loading).
    pub fn open(path: &str) -> io::Result<Self> {
        let p = Path::new(path);
        let mut reader = Reader::open(path)?;
        let ids = resolve::resolve_cpu_protocol(&reader)?;

        // Pre-intern stage names into indices
        let stage_name_indices: Vec<u16> = (0..ids.stage_names.len()).map(|i| i as u16).collect();

        // Detect counter storages
        let counter_storages = resolve::detect_counter_storages(&reader);

        // Detect buffer storages
        let buffer_infos = resolve::detect_buffer_infos(&reader);

        // Build segment index
        let segment_index = resolve::build_segment_index_from_file(path, &reader, ids.period_ps)?;

        // Read embedded TraceSummary
        let summary = reader.trace_summary().cloned();

        // Populate metadata
        let metadata = resolve::populate_metadata(&reader, p, &ids);

        let max_cycle = (reader.header().total_time_ps / ids.period_ps) as u32;
        let period_ps = ids.period_ps;

        // Build counter storage map
        let counter_storage_map: HashMap<u16, usize> = counter_storages
            .iter()
            .enumerate()
            .map(|(i, (sid, _))| (*sid, i))
            .collect();

        // For small traces, replay segments to get per-cycle counter samples.
        let is_small_trace = max_cycle <= 32 * 1024;
        let counter_series = if is_small_trace && !counter_storages.is_empty() {
            let mut series: Vec<CounterSeries> = counter_storages
                .iter()
                .map(|(_, name)| CounterSeries {
                    name: name.clone(),
                    samples: Vec::new(),
                    default_mode: CounterDisplayMode::Total,
                })
                .collect();
            let mut accum: Vec<u64> = vec![0; counter_storages.len()];
            for seg_idx in 0..reader.segment_count() {
                if let Ok((_storages, items)) = reader.segment_replay(seg_idx) {
                    for item in items {
                        if let uscope::state::TimedItem::Op(op) = item {
                            if op.action == DA_SLOT_ADD {
                                if let Some(&ci) = counter_storage_map.get(&op.storage_id) {
                                    let cycle = (op.time_ps / period_ps) as u32;
                                    accum[ci] = accum[ci].wrapping_add(op.value);
                                    series[ci].samples.push((cycle, accum[ci]));
                                }
                            }
                        }
                    }
                }
            }
            // Final sample at trace end.
            for (ci, s) in series.iter_mut().enumerate() {
                let last_c = s.samples.last().map(|(c, _)| *c).unwrap_or(0);
                if last_c < max_cycle {
                    s.samples.push((max_cycle, accum[ci]));
                }
            }
            series
        } else {
            counter_storages
                .iter()
                .map(|(_, name)| CounterSeries {
                    name: name.clone(),
                    samples: Vec::new(),
                    default_mode: CounterDisplayMode::Total,
                })
                .collect()
        };

        #[cfg(feature = "decode")]
        let decoder = decode::build_rv64gc_decoder();

        Ok(Self {
            reader,
            ids,
            segment_index,
            summary,
            stage_name_indices,
            counter_storages,
            buffer_infos,
            metadata,
            period_ps,
            max_cycle,
            counter_series,
            counter_storage_map,
            #[cfg(feature = "decode")]
            decoder,
        })
    }

    // ── Accessors ──────────────────────────────────────────────────

    /// File-level information about the trace.
    pub fn file_info(&self) -> FileInfo {
        let total_instructions = self
            .summary
            .as_ref()
            .map(|s| s.total_instructions)
            .unwrap_or(0);
        FileInfo {
            version: format!(
                "{}.{}",
                self.reader.header().version_major,
                self.reader.header().version_minor
            ),
            segment_count: self.reader.segment_count(),
            total_instructions,
            max_cycle: self.max_cycle,
            period_ps: self.period_ps,
            metadata: self.metadata.clone(),
        }
    }

    /// Expose the Reader's schema.
    pub fn schema(&self) -> &uscope::schema::Schema {
        self.reader.schema()
    }

    /// Maximum cycle in the trace.
    pub fn max_cycle(&self) -> u32 {
        self.max_cycle
    }

    /// Clock period in picoseconds.
    pub fn period_ps(&self) -> u64 {
        self.period_ps
    }

    /// Counter names with storage IDs.
    pub fn counter_names(&self) -> &[(u16, String)] {
        &self.counter_storages
    }

    /// Buffer storages detected from the schema.
    pub fn buffer_infos(&self) -> &[BufferInfo] {
        &self.buffer_infos
    }

    /// Key-value metadata from the trace source.
    pub fn metadata(&self) -> &[(String, String)] {
        &self.metadata
    }

    /// Number of segments in the trace.
    pub fn segment_count(&self) -> usize {
        self.reader.segment_count()
    }

    /// The segment index (cycle ranges per segment).
    pub fn segment_index(&self) -> &SegmentIndex {
        &self.segment_index
    }

    /// The embedded TraceSummary (mipmap data), if available.
    pub fn trace_summary(&self) -> Option<&TraceSummary> {
        self.summary.as_ref()
    }

    /// Pipeline stage names.
    pub fn stage_names(&self) -> &[String] {
        &self.ids.stage_names
    }

    /// Stage name indices mapping (uscope enum index -> StageNameIdx).
    pub fn stage_name_indices(&self) -> &[u16] {
        &self.stage_name_indices
    }

    /// Entities storage ID.
    pub fn entities_storage_id(&self) -> u16 {
        self.ids.entities_storage_id
    }

    /// Borrow the underlying reader.
    pub fn reader(&self) -> &Reader {
        &self.reader
    }

    /// Mutably borrow the underlying reader.
    pub fn reader_mut(&mut self) -> &mut Reader {
        &mut self.reader
    }

    /// Borrow the resolved protocol IDs.
    pub fn protocol_ids(&self) -> &CpuProtocolIds {
        &self.ids
    }

    /// Sparse counter series (populated for small traces).
    pub fn counter_series(&self) -> &[CounterSeries] {
        &self.counter_series
    }

    /// Mutable access to counter series (for merging loaded segment data).
    pub fn counter_series_mut(&mut self) -> &mut Vec<CounterSeries> {
        &mut self.counter_series
    }

    // ── Counter queries ────────────────────────────────────────────

    /// Get cumulative counter value at a cycle.
    /// Uses per-cycle samples when available, otherwise mipmap.
    pub fn counter_value_at(&self, counter_idx: usize, cycle: u32) -> u64 {
        if counter_idx < self.counter_series.len()
            && !self.counter_series[counter_idx].samples.is_empty()
        {
            return counters::counter_value_at(&self.counter_series[counter_idx], cycle);
        }
        if let Some(ref summary) = self.summary {
            summary.counter_value_at(counter_idx, cycle)
        } else {
            0
        }
    }

    /// Get counter rate over a window ending at the given cycle.
    pub fn counter_rate_at(&self, counter_idx: usize, cycle: u32, window: u32) -> f64 {
        let end_val = self.counter_value_at(counter_idx, cycle);
        let start_val = self.counter_value_at(counter_idx, cycle.saturating_sub(window));
        let actual_window = cycle.saturating_sub(cycle.saturating_sub(window));
        if actual_window == 0 {
            return 0.0;
        }
        (end_val.wrapping_sub(start_val)) as f64 / actual_window as f64
    }

    /// Get counter delta at a cycle.
    pub fn counter_delta_at(&self, counter_idx: usize, cycle: u32) -> u64 {
        let curr = self.counter_value_at(counter_idx, cycle);
        let prev = if cycle > 0 {
            self.counter_value_at(counter_idx, cycle - 1)
        } else {
            0
        };
        curr.wrapping_sub(prev)
    }

    /// Fast counter downsampling using sparse samples or mipmap.
    pub fn counter_downsample(
        &self,
        counter_idx: usize,
        start_cycle: u32,
        end_cycle: u32,
        bucket_count: usize,
    ) -> Vec<(u64, u64)> {
        if bucket_count == 0 || start_cycle >= end_cycle {
            return Vec::new();
        }

        // Use per-cycle samples when available (populated for small traces).
        if counter_idx < self.counter_series.len()
            && !self.counter_series[counter_idx].samples.is_empty()
        {
            return counters::counter_downsample_minmax(
                &self.counter_series[counter_idx],
                start_cycle,
                end_cycle,
                bucket_count,
            );
        }

        // Mipmap path (large traces).
        if let Some(ref summary) = self.summary {
            if counter_idx < summary.counters.len() {
                let mipmap = &summary.counters[counter_idx];
                let range_cycles = end_cycle - start_cycle;
                let cycles_per_bucket = range_cycles as f64 / bucket_count as f64;

                // Pick the mipmap level where each mipmap entry covers
                // roughly one output bucket (or slightly finer).
                let base = summary.base_interval_cycles as f64;
                let fan = summary.fan_out as f64;
                let mut level = 0usize;
                let mut level_interval = base;
                while level + 1 < mipmap.levels.len() && level_interval * fan <= cycles_per_bucket {
                    level += 1;
                    level_interval *= fan;
                }

                let entries = &mipmap.levels[level];
                if entries.is_empty() {
                    return vec![(0, 0); bucket_count];
                }

                // Map output buckets to mipmap entries.
                let level_interval = summary.base_interval_cycles as f64
                    * (summary.fan_out as f64).powi(level as i32);

                let mut result = Vec::with_capacity(bucket_count);
                for b in 0..bucket_count {
                    let b_start = start_cycle as f64 + b as f64 * cycles_per_bucket;
                    let b_end = b_start + cycles_per_bucket;

                    // Find mipmap entries overlapping this bucket.
                    let entry_start = (b_start / level_interval) as usize;
                    let entry_end = ((b_end / level_interval).ceil() as usize).min(entries.len());

                    // Use weighted average rate instead of raw min/max to avoid
                    // moire patterns from mipmap bucket boundary misalignment.
                    let mut total_sum = 0.0f64;
                    let mut total_cycles = 0.0f64;
                    for (ei, entry) in entries[entry_start..entry_end].iter().enumerate() {
                        let e_start = (entry_start + ei) as f64 * level_interval;
                        let e_end = e_start + level_interval;
                        let overlap_start = b_start.max(e_start);
                        let overlap_end = b_end.min(e_end);
                        let overlap = (overlap_end - overlap_start).max(0.0);
                        let entry_frac = overlap / level_interval;
                        total_sum += entry.sum as f64 * entry_frac;
                        total_cycles += overlap;
                    }
                    let avg_rate = if total_cycles > 0.0 {
                        (total_sum / total_cycles * level_interval) as u64
                    } else {
                        0
                    };
                    result.push((avg_rate, avg_rate));
                }
                return result;
            }
        }

        // Fallback to sparse sample method.
        if counter_idx < self.counter_series.len() {
            counters::counter_downsample_minmax(
                &self.counter_series[counter_idx],
                start_cycle,
                end_cycle,
                bucket_count,
            )
        } else {
            vec![(0, 0); bucket_count]
        }
    }

    // ── Buffer queries ─────────────────────────────────────────────

    /// Query buffer storage state at a given cycle.
    pub fn buffer_state_at(&mut self, buffer_idx: usize, cycle: u32) -> BufferState {
        let buffer = match self.buffer_infos.get(buffer_idx) {
            Some(b) => b.clone(),
            None => return BufferState::default(),
        };
        let entities_storage_id = self.ids.entities_storage_id;
        let period_ps = self.period_ps;
        buffers::buffer_state_at(
            &mut self.reader,
            &buffer,
            entities_storage_id,
            period_ps,
            cycle,
        )
    }

    // ── Segment loading ────────────────────────────────────────────

    /// Load instructions from specific segments.
    ///
    /// Replays only the given segment indices and returns the resulting
    /// instructions, stages, and dependencies. The caller is responsible for
    /// merging these into their own data structures.
    pub fn load_segments(&mut self, segment_indices: &[usize]) -> io::Result<SegmentLoadResult> {
        let mut slot_to_entity: HashMap<u16, u32> = HashMap::new();
        let mut builders: HashMap<u32, InstrBuilder> = HashMap::new();
        let mut finalized: Vec<InstrBuilder> = Vec::new();
        let mut next_reflex_id: u32 = 0;
        let mut dependencies: Vec<Dependency> = Vec::new();

        // Counter accumulators for per-cycle sample extraction.
        let num_counters = self.counter_storages.len();
        let mut counter_accum: Vec<u64> = vec![0; num_counters];
        let mut counter_samples: Vec<Vec<(u32, u64)>> =
            (0..num_counters).map(|_| Vec::new()).collect();

        for &seg_idx in segment_indices {
            let (_storages, items) = self.reader.segment_replay(seg_idx)?;

            for item in items {
                match item {
                    TimedItem::Op(op) => {
                        let cycle = (op.time_ps / self.period_ps) as u32;

                        // Extract counter deltas.
                        if op.action == DA_SLOT_ADD {
                            if let Some(&ci) = self.counter_storage_map.get(&op.storage_id) {
                                counter_accum[ci] = counter_accum[ci].wrapping_add(op.value);
                                counter_samples[ci].push((cycle, counter_accum[ci]));
                            }
                        }

                        if op.storage_id != self.ids.entities_storage_id {
                            continue;
                        }

                        match op.action {
                            DA_SLOT_SET => {
                                if op.field_index == self.ids.field_entity_id {
                                    let entity_id = op.value as u32;
                                    let reflex_id = next_reflex_id;
                                    next_reflex_id += 1;
                                    slot_to_entity.insert(op.slot, entity_id);
                                    builders.insert(
                                        entity_id,
                                        InstrBuilder::new(entity_id, reflex_id, 0, cycle),
                                    );
                                } else if op.field_index == self.ids.field_pc {
                                    if let Some(&eid) = slot_to_entity.get(&op.slot) {
                                        if let Some(b) = builders.get_mut(&eid) {
                                            b.pc = op.value;
                                            b.disasm = format!("0x{:08x}", op.value);
                                        }
                                    }
                                } else if Some(op.field_index) == self.ids.field_inst_bits {
                                    if let Some(&eid) = slot_to_entity.get(&op.slot) {
                                        if let Some(b) = builders.get_mut(&eid) {
                                            b.inst_bits = Some(op.value as u32);
                                        }
                                    }
                                } else if Some(op.field_index) == self.ids.field_rbid {
                                    if let Some(&eid) = slot_to_entity.get(&op.slot) {
                                        if let Some(b) = builders.get_mut(&eid) {
                                            b.rbid = Some(op.value as u32);
                                        }
                                    }
                                } else if Some(op.field_index) == self.ids.field_iq_id {
                                    if let Some(&eid) = slot_to_entity.get(&op.slot) {
                                        if let Some(b) = builders.get_mut(&eid) {
                                            b.iq_id = Some(op.value as u32);
                                        }
                                    }
                                } else if Some(op.field_index) == self.ids.field_dq_id {
                                    if let Some(&eid) = slot_to_entity.get(&op.slot) {
                                        if let Some(b) = builders.get_mut(&eid) {
                                            b.dq_id = Some(op.value as u32);
                                        }
                                    }
                                } else if Some(op.field_index) == self.ids.field_ready_time_ps {
                                    if let Some(&eid) = slot_to_entity.get(&op.slot) {
                                        if let Some(b) = builders.get_mut(&eid) {
                                            b.ready_time_ps = Some(op.value);
                                        }
                                    }
                                }
                            }
                            DA_SLOT_CLEAR => {
                                if let Some(entity_id) = slot_to_entity.remove(&op.slot) {
                                    if let Some(mut b) = builders.remove(&entity_id) {
                                        b.close_current_stage(cycle);
                                        if b.retire_status == RetireStatus::InFlight {
                                            b.retire_status = RetireStatus::Retired;
                                        }
                                        finalized.push(b);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    TimedItem::Event(ev) => {
                        let cycle = (ev.time_ps / self.period_ps) as u32;

                        if ev.event_type_id == self.ids.stage_transition_event_id {
                            if ev.payload.len() >= 5 {
                                let entity_id =
                                    u32::from_le_bytes(ev.payload[..4].try_into().unwrap());
                                let stage = ev.payload[4] as usize;
                                if let Some(b) = builders.get_mut(&entity_id) {
                                    let stage_idx =
                                        self.stage_name_indices.get(stage).copied().unwrap_or(0);
                                    b.open_stage(stage_idx, cycle);
                                }
                            }
                        } else if ev.event_type_id == self.ids.flush_event_id {
                            if ev.payload.len() >= 4 {
                                let entity_id =
                                    u32::from_le_bytes(ev.payload[..4].try_into().unwrap());
                                if let Some(mut b) = builders.remove(&entity_id) {
                                    b.close_current_stage(cycle);
                                    b.retire_status = RetireStatus::Flushed;
                                    let slot = entity_id as u16;
                                    slot_to_entity.remove(&slot);
                                    finalized.push(b);
                                }
                            }
                        } else if ev.event_type_id == self.ids.annotate_event_id {
                            if ev.payload.len() >= 8 {
                                let entity_id =
                                    u32::from_le_bytes(ev.payload[..4].try_into().unwrap());
                                let text_ref =
                                    u32::from_le_bytes(ev.payload[4..8].try_into().unwrap());
                                if let Some(b) = builders.get_mut(&entity_id) {
                                    if let Some(st) = self.reader.string_table() {
                                        if let Some(text) = st.get(text_ref) {
                                            if builder::is_disasm_line(text, b.pc) {
                                                b.disasm = text.to_string();
                                                b.has_disasm_annotation = true;
                                            } else {
                                                if !b.tooltip.is_empty() {
                                                    b.tooltip.push('\n');
                                                }
                                                b.tooltip.push_str(text);
                                            }
                                        }
                                    }
                                }
                            }
                        } else if ev.event_type_id == self.ids.dependency_event_id
                            && ev.payload.len() >= 9
                        {
                            let src_id = u32::from_le_bytes(ev.payload[..4].try_into().unwrap());
                            let dst_id = u32::from_le_bytes(ev.payload[4..8].try_into().unwrap());
                            let dep_type = ev.payload[8];
                            let kind = match dep_type {
                                0 => DepKind::Data,
                                1 => DepKind::Data,
                                2 => DepKind::Data,
                                3 => DepKind::Memory,
                                _ => DepKind::Data,
                            };
                            dependencies.push(Dependency {
                                producer: src_id,
                                consumer: dst_id,
                                kind,
                            });
                        }
                    }
                }
            }
        }

        // Finalize remaining in-flight instructions
        for (_eid, b) in builders.drain() {
            let mut b = b;
            b.close_current_stage(b.last_cycle.saturating_add(1));
            finalized.push(b);
        }

        // Sort by first_cycle, then by reflex_id for stable ordering
        finalized.sort_by(|a, b| {
            a.first_cycle
                .cmp(&b.first_cycle)
                .then(a.reflex_id.cmp(&b.reflex_id))
        });

        // Build output vectors
        let mut instructions = Vec::with_capacity(finalized.len());
        let mut stages = Vec::new();

        for mut b in finalized {
            // Decode instruction bits into mnemonic if no annotation already provided disasm
            #[cfg(feature = "decode")]
            if !b.has_disasm_annotation {
                if let (Some(bits), Some(dec)) = (b.inst_bits, &self.decoder) {
                    let mnemonic = decode::decode_instruction(dec, bits);
                    if b.pc != 0 {
                        b.disasm = format!("0x{:08x} {}", b.pc, mnemonic);
                    } else {
                        b.disasm = mnemonic;
                    }
                }
            }

            let stage_start = stages.len() as u32;
            stages.extend(b.stages);
            let stage_end = stages.len() as u32;

            instructions.push(InstructionData {
                id: b.entity_id,
                sim_id: b.entity_id as u64,
                thread_id: 0,
                rbid: b.rbid,
                iq_id: b.iq_id,
                dq_id: b.dq_id,
                ready_cycle: b.ready_time_ps.map(|t| (t / self.period_ps) as u32),
                disasm: b.disasm,
                tooltip: b.tooltip,
                stage_range: stage_start..stage_end,
                retire_status: b.retire_status,
                first_cycle: b.first_cycle,
                last_cycle: b.last_cycle,
            });
        }

        Ok(SegmentLoadResult {
            instructions,
            stages,
            dependencies,
            counter_samples,
        })
    }
}

/// Result of loading instruction data from one or more segments.
pub struct SegmentLoadResult {
    pub instructions: Vec<InstructionData>,
    pub stages: Vec<StageSpan>,
    pub dependencies: Vec<Dependency>,
    /// Per-counter (cycle, cumulative_value) samples extracted during replay.
    /// Indexed by counter index (same order as `CpuTrace::counter_names()`).
    pub counter_samples: Vec<Vec<(u32, u64)>>,
}
