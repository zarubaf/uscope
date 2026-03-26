/// Reader for µScope trace files.
/// Supports both finalized files and live tailing.
use crate::checkpoint::{FieldOffsets, StorageState};
use crate::schema::Schema;
use crate::segment;
use crate::state::{self, TimedEvent, TimedItem, TraceState};
use crate::summary::TraceSummary;
use crate::types::*;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};

/// Parsed string table.
#[derive(Debug)]
pub struct StringTable {
    entries: Vec<String>,
}

impl StringTable {
    fn read_from<R: Read>(r: &mut R, size: u64) -> io::Result<Self> {
        let header = StringTableHeader::read_from(r)?;
        let mut indices = Vec::with_capacity(header.num_entries as usize);
        for _ in 0..header.num_entries {
            indices.push(StringIndex::read_from(r)?);
        }

        // Read all remaining string data
        let data_size = size as usize
            - StringTableHeader::SIZE
            - header.num_entries as usize * StringIndex::SIZE;
        let mut data = vec![0u8; data_size];
        r.read_exact(&mut data)?;

        let entries = indices
            .iter()
            .map(|idx| {
                let start = idx.offset as usize;
                let end = start + idx.length as usize;
                String::from_utf8_lossy(&data[start..end]).to_string()
            })
            .collect();

        Ok(Self { entries })
    }

    /// Get a string by its index.
    pub fn get(&self, index: u32) -> Option<&str> {
        self.entries.get(index as usize).map(|s| s.as_str())
    }
}

/// µScope trace reader.
pub struct Reader {
    file: BufReader<File>,
    header: FileHeader,
    schema: Schema,
    dut: DutDesc,
    dut_string_pool: Vec<u8>,
    trace_config: TraceConfig,
    field_offsets: Vec<FieldOffsets>,
    segment_table: Vec<SegmentIndexEntry>,
    string_table: Option<StringTable>,
    trace_summary: Option<TraceSummary>,
}

impl Reader {
    /// Open a trace file for reading.
    pub fn open(path: &str) -> io::Result<Self> {
        let file = File::open(path)?;
        let mut file = BufReader::new(file);

        // Read file header
        let header = FileHeader::read_from(&mut file)?;

        // Read preamble chunks
        let mut schema = None;
        let mut dut = None;
        let mut dut_string_pool = Vec::new();
        let mut trace_config = None;

        loop {
            let chunk = PreambleChunk::read_from(&mut file)?;
            match chunk.chunk_type {
                CHUNK_END => break,
                CHUNK_DUT_DESC => {
                    dut = Some(DutDesc::read_from(&mut io::Cursor::new(&chunk.payload))?);
                }
                CHUNK_SCHEMA => {
                    let s = Schema::read_from(
                        &mut io::Cursor::new(&chunk.payload),
                        chunk.payload.len(),
                    )?;
                    dut_string_pool = s.string_pool.clone();
                    schema = Some(s);
                }
                CHUNK_TRACE_CONFIG => {
                    trace_config = Some(TraceConfig::read_from(&mut io::Cursor::new(
                        &chunk.payload,
                    ))?);
                }
                _ => {} // skip unknown chunks
            }
        }

        let schema = schema
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing schema chunk"))?;
        let dut = dut.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "missing DUT descriptor chunk")
        })?;
        let trace_config = trace_config.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "missing trace config chunk")
        })?;

        let field_offsets: Vec<FieldOffsets> = schema
            .storages
            .iter()
            .map(FieldOffsets::from_storage_def)
            .collect();

        // Read section table if finalized
        let mut segment_table = Vec::new();
        let mut string_table = None;
        let mut trace_summary = None;

        if header.flags & F_COMPLETE != 0 && header.section_table_offset != 0 {
            // First pass: read all section entries
            file.seek(SeekFrom::Start(header.section_table_offset))?;
            let mut sections = Vec::new();
            loop {
                let entry = SectionEntry::read_from(&mut file)?;
                if entry.section_type == SECTION_END {
                    break;
                }
                sections.push(entry);
            }
            // Second pass: read section data (requires seeking)
            for entry in &sections {
                match entry.section_type {
                    SECTION_SEGMENTS => {
                        segment_table =
                            segment::read_segment_table(&mut file, entry.offset, entry.size)?;
                    }
                    SECTION_STRINGS => {
                        file.seek(SeekFrom::Start(entry.offset))?;
                        string_table = Some(StringTable::read_from(&mut file, entry.size)?);
                    }
                    SECTION_COUNTER_SUMMARY => {
                        file.seek(SeekFrom::Start(entry.offset))?;
                        let mut data = vec![0u8; entry.size as usize];
                        file.read_exact(&mut data)?;
                        match crate::summary::deserialize_trace_summary(&data) {
                            Ok(summary) => trace_summary = Some(summary),
                            Err(e) => {
                                eprintln!("Warning: failed to read embedded trace summary: {}", e);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // If no segment table, walk the chain
        if segment_table.is_empty() && header.tail_offset != 0 {
            let chain = segment::walk_chain(&mut file, header.tail_offset)?;
            segment_table = chain
                .into_iter()
                .map(|(offset, seg)| SegmentIndexEntry {
                    offset,
                    time_start_ps: seg.time_start_ps,
                    time_end_ps: seg.time_end_ps,
                })
                .collect();
        }

        Ok(Self {
            file,
            header,
            schema,
            dut,
            dut_string_pool,
            trace_config,
            field_offsets,
            segment_table,
            string_table,
            trace_summary,
        })
    }

    pub fn header(&self) -> &FileHeader {
        &self.header
    }

    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    pub fn dut_desc(&self) -> &DutDesc {
        &self.dut
    }

    pub fn trace_config(&self) -> &TraceConfig {
        &self.trace_config
    }

    pub fn field_offsets(&self) -> &[FieldOffsets] {
        &self.field_offsets
    }

    pub fn string_table(&self) -> Option<&StringTable> {
        self.string_table.as_ref()
    }

    /// Set a pre-computed trace summary (mipmap data).
    pub fn set_trace_summary(&mut self, summary: TraceSummary) {
        self.trace_summary = Some(summary);
    }

    /// Get the trace summary, if one has been computed/loaded.
    pub fn trace_summary(&self) -> Option<&TraceSummary> {
        self.trace_summary.as_ref()
    }

    /// Backward-compatible alias for `set_trace_summary`.
    pub fn set_counter_summary(&mut self, summary: TraceSummary) {
        self.set_trace_summary(summary);
    }

    /// Backward-compatible alias for `trace_summary`.
    pub fn counter_summary(&self) -> Option<&TraceSummary> {
        self.trace_summary()
    }

    /// Look up a DUT property value by key.
    pub fn dut_property(&self, key: &str) -> Option<&str> {
        let reader = crate::string_pool::StringPoolReader::new(&self.dut_string_pool);
        for p in &self.dut.properties {
            if reader.get(p.key) == Some(key) {
                return reader.get(p.value);
            }
        }
        None
    }

    /// Iterate over all DUT properties as (key, value) string pairs.
    pub fn dut_properties(&self) -> Vec<(&str, &str)> {
        let reader = crate::string_pool::StringPoolReader::new(&self.dut_string_pool);
        self.dut
            .properties
            .iter()
            .filter_map(|p| {
                let key = reader.get(p.key)?;
                let value = reader.get(p.value)?;
                Some((key, value))
            })
            .collect()
    }

    /// Reconstruct state at a given time.
    pub fn state_at(&mut self, time_ps: u64) -> io::Result<TraceState> {
        let seg_idx = segment::find_segment_for_time(&self.segment_table, time_ps)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no segment for time"))?;

        let seg_entry = &self.segment_table[seg_idx];
        self.file.seek(SeekFrom::Start(seg_entry.offset))?;
        let seg_header = SegmentHeader::read_from(&mut self.file)?;

        // Read checkpoint
        let mut storages: Vec<StorageState> =
            self.schema.storages.iter().map(StorageState::new).collect();

        for storage_state in &mut storages {
            let block = CheckpointBlock::read_from(&mut self.file)?;
            storage_state.read_checkpoint(&mut self.file, block.size)?;
        }

        // Read and decompress deltas
        let mut compressed = vec![0u8; seg_header.deltas_compressed_size as usize];
        self.file.read_exact(&mut compressed)?;

        let delta_data = if self.header.flags & F_COMPRESSED != 0 {
            lz4_flex::decompress_size_prepended(&compressed).map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidData, format!("LZ4 decompress: {}", e))
            })?
        } else {
            compressed
        };

        let interleaved = self.header.flags & F_INTERLEAVED_DELTAS != 0;

        let final_time = if interleaved {
            let (ft, _items) = state::replay_deltas_v2(
                &delta_data,
                &mut storages,
                &self.field_offsets,
                seg_header.time_start_ps,
                Some(time_ps),
            )?;
            ft
        } else {
            let compact = self.header.flags & F_COMPACT_DELTAS != 0;
            let (ft, _events, _ops) = state::replay_deltas(
                &delta_data,
                &mut storages,
                &self.field_offsets,
                seg_header.time_start_ps,
                Some(time_ps),
                compact,
            )?;
            ft
        };

        Ok(TraceState {
            time_ps: final_time,
            storages,
        })
    }

    /// Get all events in a time range.
    pub fn events_in_range(&mut self, t0: u64, t1: u64) -> io::Result<Vec<TimedEvent>> {
        let mut all_events = Vec::new();

        // Find first and last segment
        let first = match segment::find_segment_for_time(&self.segment_table, t0) {
            Some(i) => i,
            None => return Ok(all_events),
        };
        let last = segment::find_segment_for_time(&self.segment_table, t1).unwrap_or(first);

        for idx in first..=last {
            if idx >= self.segment_table.len() {
                break;
            }
            let seg_entry = &self.segment_table[idx];
            self.file.seek(SeekFrom::Start(seg_entry.offset))?;
            let seg_header = SegmentHeader::read_from(&mut self.file)?;

            // Skip checkpoint
            let mut skip = vec![0u8; seg_header.checkpoint_size as usize];
            self.file.read_exact(&mut skip)?;

            // Read and decompress deltas
            let mut compressed = vec![0u8; seg_header.deltas_compressed_size as usize];
            self.file.read_exact(&mut compressed)?;

            let delta_data = if self.header.flags & F_COMPRESSED != 0 {
                lz4_flex::decompress_size_prepended(&compressed).map_err(|e| {
                    io::Error::new(io::ErrorKind::InvalidData, format!("LZ4 decompress: {}", e))
                })?
            } else {
                compressed
            };

            let interleaved = self.header.flags & F_INTERLEAVED_DELTAS != 0;
            let mut storages: Vec<StorageState> =
                self.schema.storages.iter().map(StorageState::new).collect();

            if interleaved {
                let (_final_time, items) = state::replay_deltas_v2(
                    &delta_data,
                    &mut storages,
                    &self.field_offsets,
                    seg_header.time_start_ps,
                    Some(t1),
                )?;
                for item in items {
                    if let state::TimedItem::Event(ev) = item {
                        if ev.time_ps >= t0 && ev.time_ps <= t1 {
                            all_events.push(ev);
                        }
                    }
                }
            } else {
                let compact = self.header.flags & F_COMPACT_DELTAS != 0;
                let (_final_time, events, _ops) = state::replay_deltas(
                    &delta_data,
                    &mut storages,
                    &self.field_offsets,
                    seg_header.time_start_ps,
                    Some(t1),
                    compact,
                )?;
                for ev in events {
                    if ev.time_ps >= t0 && ev.time_ps <= t1 {
                        all_events.push(ev);
                    }
                }
            }
        }

        Ok(all_events)
    }

    /// Number of segments in the trace.
    pub fn segment_count(&self) -> usize {
        self.segment_table.len()
    }

    /// Replay a single segment, returning storage states and ordered items.
    pub fn segment_replay(
        &mut self,
        seg_idx: usize,
    ) -> io::Result<(Vec<StorageState>, Vec<TimedItem>)> {
        if seg_idx >= self.segment_table.len() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "segment index out of range",
            ));
        }

        let seg_entry = &self.segment_table[seg_idx];
        self.file.seek(SeekFrom::Start(seg_entry.offset))?;
        let seg_header = SegmentHeader::read_from(&mut self.file)?;

        // Read checkpoint
        let mut storages: Vec<StorageState> =
            self.schema.storages.iter().map(StorageState::new).collect();

        for storage_state in &mut storages {
            let block = CheckpointBlock::read_from(&mut self.file)?;
            storage_state.read_checkpoint(&mut self.file, block.size)?;
        }

        // Read and decompress deltas
        let mut compressed = vec![0u8; seg_header.deltas_compressed_size as usize];
        self.file.read_exact(&mut compressed)?;

        let delta_data = if self.header.flags & F_COMPRESSED != 0 {
            lz4_flex::decompress_size_prepended(&compressed).map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidData, format!("LZ4 decompress: {}", e))
            })?
        } else {
            compressed
        };

        let interleaved = self.header.flags & F_INTERLEAVED_DELTAS != 0;

        if interleaved {
            let (_final_time, items) = state::replay_deltas_v2(
                &delta_data,
                &mut storages,
                &self.field_offsets,
                seg_header.time_start_ps,
                None,
            )?;
            Ok((storages, items))
        } else {
            // v0.1 fallback: separate ops/events, merge into TimedItem
            let compact = self.header.flags & F_COMPACT_DELTAS != 0;
            let (_final_time, events, ops) = state::replay_deltas(
                &delta_data,
                &mut storages,
                &self.field_offsets,
                seg_header.time_start_ps,
                None,
                compact,
            )?;
            let mut items: Vec<TimedItem> = Vec::with_capacity(ops.len() + events.len());
            for op in ops {
                items.push(TimedItem::Op(op));
            }
            for ev in events {
                items.push(TimedItem::Event(ev));
            }
            // Sort by time, ops before events at same time (v0.1 compat)
            items.sort_by_key(|item| {
                let t = item.time_ps();
                let order = match item {
                    TimedItem::Op(_) => 0u8,
                    TimedItem::Event(_) => 1u8,
                };
                (t, order)
            });
            Ok((storages, items))
        }
    }

    /// Check for new segments (live tailing).
    /// Re-reads the file header to check if tail_offset has changed.
    pub fn poll_new_segments(&mut self) -> io::Result<bool> {
        let old_tail = self.header.tail_offset;
        self.file.seek(SeekFrom::Start(0))?;
        self.header = FileHeader::read_from(&mut self.file)?;

        if self.header.tail_offset != old_tail && self.header.tail_offset != 0 {
            // Walk new segments from old tail
            let mut offset = self.header.tail_offset;
            let mut new_segments = Vec::new();

            while offset != 0 && offset != old_tail {
                let seg = segment::read_segment_at(&mut self.file, offset)?;
                new_segments.push(SegmentIndexEntry {
                    offset,
                    time_start_ps: seg.time_start_ps,
                    time_end_ps: seg.time_end_ps,
                });
                offset = seg.prev_segment_offset;
            }

            new_segments.reverse();
            self.segment_table.extend(new_segments);
            return Ok(true);
        }

        Ok(false)
    }
}
