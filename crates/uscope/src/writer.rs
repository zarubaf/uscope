/// Streaming writer for µScope trace files.
/// Writes file header, preamble, and segments incrementally.
use crate::checkpoint::{FieldOffsets, StorageState};
use crate::leb128;
use crate::schema::Schema;
use crate::types::*;
use byteorder::WriteBytesExt;
use std::io::{self, Seek, SeekFrom, Write};

/// A single item in an interleaved frame.
#[derive(Debug, Clone)]
enum FrameItem {
    Op(DeltaOp),
    Event(EventRecord),
}

/// Accumulated items for the current cycle (v0.2 interleaved).
#[derive(Debug, Default)]
struct CycleFrame {
    items: Vec<FrameItem>,
}

impl CycleFrame {
    fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    fn ops(&self) -> impl Iterator<Item = &DeltaOp> {
        self.items.iter().filter_map(|item| match item {
            FrameItem::Op(op) => Some(op),
            _ => None,
        })
    }
}

/// A buffered segment waiting to be flushed.
#[derive(Debug)]
struct SegmentBuffer {
    frames: Vec<(u64, CycleFrame)>, // (time_delta_ps, frame)
    time_start_ps: u64,
    time_end_ps: u64,
}

impl SegmentBuffer {
    fn new(time_start_ps: u64) -> Self {
        Self {
            frames: Vec::new(),
            time_start_ps,
            time_end_ps: time_start_ps,
        }
    }
}

/// String table builder for STRING_REF fields.
#[derive(Debug)]
pub struct StringTable {
    entries: Vec<String>,
    index: std::collections::HashMap<String, u32>,
}

impl StringTable {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            index: std::collections::HashMap::new(),
        }
    }

    /// Insert a string and return its index.
    pub fn insert(&mut self, s: &str) -> u32 {
        if let Some(&idx) = self.index.get(s) {
            return idx;
        }
        let idx = self.entries.len() as u32;
        self.index.insert(s.to_owned(), idx);
        self.entries.push(s.to_owned());
        idx
    }

    fn write_to<W: Write>(&self, w: &mut W) -> io::Result<usize> {
        let header = StringTableHeader {
            num_entries: self.entries.len() as u32,
            reserved: 0,
        };
        header.write_to(w)?;
        let mut total = StringTableHeader::SIZE;

        // Write index entries first, then string data
        let mut data_offset = 0u32;
        let mut indices = Vec::with_capacity(self.entries.len());
        for s in &self.entries {
            indices.push(StringIndex {
                offset: data_offset,
                length: s.len() as u32,
            });
            data_offset += s.len() as u32 + 1; // +1 for null terminator
        }
        for idx in &indices {
            idx.write_to(w)?;
            total += StringIndex::SIZE;
        }
        for s in &self.entries {
            w.write_all(s.as_bytes())?;
            w.write_u8(0)?; // null terminator
            total += s.len() + 1;
        }
        Ok(total)
    }
}

/// Streaming µScope trace writer.
pub struct Writer<W: Write + Seek> {
    file: W,
    header: FileHeader,
    schema: Schema,
    field_offsets: Vec<FieldOffsets>,
    checkpoint_interval_ps: u64,

    // Per-storage state
    storage_states: Vec<StorageState>,

    // Current segment
    segment_buf: SegmentBuffer,
    current_frame: CycleFrame,
    current_time_ps: u64,
    last_frame_time_ps: u64,
    in_cycle: bool,

    // Segment tracking
    segment_index: Vec<SegmentIndexEntry>,
    prev_segment_offset: u64,
    next_checkpoint_ps: u64,

    // String table
    pub string_table: StringTable,
}

impl<W: Write + Seek> Writer<W> {
    /// Create a new trace writer.
    pub fn create(
        mut file: W,
        dut: &DutDesc,
        schema: &Schema,
        checkpoint_interval_ps: u64,
    ) -> io::Result<Self> {
        // Write file header (placeholder values)
        let mut header = FileHeader::new();
        header.write_to(&mut file)?;

        // Write preamble chunks
        // 1. DUT descriptor
        let mut dut_payload = Vec::new();
        dut.write_to(&mut dut_payload)?;
        PreambleChunk::new(CHUNK_DUT_DESC, dut_payload).write_to(&mut file)?;

        // 2. Schema
        let mut schema_payload = Vec::new();
        schema.write_to(&mut schema_payload)?;
        PreambleChunk::new(CHUNK_SCHEMA, schema_payload).write_to(&mut file)?;

        // 3. Trace config
        let config = TraceConfig {
            checkpoint_interval_ps,
        };
        let mut config_payload = Vec::new();
        config.write_to(&mut config_payload)?;
        PreambleChunk::new(CHUNK_TRACE_CONFIG, config_payload).write_to(&mut file)?;

        // 4. End chunk
        PreambleChunk::new(CHUNK_END, Vec::new()).write_to(&mut file)?;

        let preamble_end = file.stream_position()? as u32;
        header.preamble_end = preamble_end;

        // Update header with preamble_end
        file.seek(SeekFrom::Start(0))?;
        header.write_to(&mut file)?;
        file.seek(SeekFrom::Start(preamble_end as u64))?;

        // Initialize storage states
        let mut storage_states = Vec::with_capacity(schema.storages.len());
        let mut field_offsets = Vec::with_capacity(schema.storages.len());
        for s in &schema.storages {
            storage_states.push(StorageState::new(s));
            field_offsets.push(FieldOffsets::from_storage_def(s));
        }

        Ok(Self {
            file,
            header,
            schema: schema.clone(),
            field_offsets,
            checkpoint_interval_ps,
            storage_states,
            segment_buf: SegmentBuffer::new(0),
            current_frame: CycleFrame::default(),
            current_time_ps: 0,
            last_frame_time_ps: 0,
            in_cycle: false,
            segment_index: Vec::new(),
            prev_segment_offset: 0,
            next_checkpoint_ps: checkpoint_interval_ps,
            string_table: StringTable::new(),
        })
    }

    /// Begin a new cycle at the given time.
    pub fn begin_cycle(&mut self, time_ps: u64) {
        assert!(
            !self.in_cycle,
            "begin_cycle called while already in a cycle"
        );
        assert!(
            time_ps >= self.current_time_ps,
            "time must be monotonically increasing"
        );
        self.current_time_ps = time_ps;
        self.in_cycle = true;
    }

    /// Set a field value in a storage slot.
    pub fn slot_set(&mut self, storage_id: u16, slot: u16, field: u16, value: u64) {
        assert!(self.in_cycle, "slot_set called outside a cycle");
        self.current_frame.items.push(FrameItem::Op(DeltaOp {
            action: DA_SLOT_SET,
            reserved: 0,
            storage_id,
            slot_index: slot,
            field_index: field,
            value,
        }));
        let sid = storage_id as usize;
        if sid < self.storage_states.len() {
            self.storage_states[sid].set_field_at(slot, &self.field_offsets[sid], field, value);
        }
    }

    /// Clear a slot in a storage.
    pub fn slot_clear(&mut self, storage_id: u16, slot: u16) {
        assert!(self.in_cycle, "slot_clear called outside a cycle");
        self.current_frame.items.push(FrameItem::Op(DeltaOp {
            action: DA_SLOT_CLEAR,
            reserved: 0,
            storage_id,
            slot_index: slot,
            field_index: 0,
            value: 0,
        }));
        let sid = storage_id as usize;
        if sid < self.storage_states.len() {
            self.storage_states[sid].clear_slot(slot);
        }
    }

    /// Add a value to a field in a storage slot.
    pub fn slot_add(&mut self, storage_id: u16, slot: u16, field: u16, value: u64) {
        assert!(self.in_cycle, "slot_add called outside a cycle");
        self.current_frame.items.push(FrameItem::Op(DeltaOp {
            action: DA_SLOT_ADD,
            reserved: 0,
            storage_id,
            slot_index: slot,
            field_index: field,
            value,
        }));
        let sid = storage_id as usize;
        if sid < self.storage_states.len() {
            self.storage_states[sid].add_field_at(slot, &self.field_offsets[sid], field, value);
        }
    }

    /// Emit an event with a pre-serialized payload.
    pub fn event(&mut self, event_type_id: u16, payload: &[u8]) {
        assert!(self.in_cycle, "event called outside a cycle");
        self.current_frame.items.push(FrameItem::Event(EventRecord {
            event_type_id,
            reserved: 0,
            payload_size: payload.len() as u32,
            payload: payload.to_vec(),
        }));
    }

    /// End the current cycle.
    pub fn end_cycle(&mut self) -> io::Result<()> {
        assert!(self.in_cycle, "end_cycle called outside a cycle");
        self.in_cycle = false;

        let time_delta = self.current_time_ps - self.last_frame_time_ps;
        self.last_frame_time_ps = self.current_time_ps;

        let frame = std::mem::take(&mut self.current_frame);
        self.segment_buf.time_end_ps = self.current_time_ps;
        self.segment_buf.frames.push((time_delta, frame));

        // Check if we need to flush a segment
        if self.current_time_ps >= self.next_checkpoint_ps {
            self.flush_segment()?;
            self.next_checkpoint_ps = self.current_time_ps + self.checkpoint_interval_ps;
        }

        Ok(())
    }

    /// Flush the current segment buffer to disk.
    fn flush_segment(&mut self) -> io::Result<()> {
        if self.segment_buf.frames.is_empty() {
            return Ok(());
        }

        let seg_offset = self.file.stream_position()?;

        // Build checkpoint
        let mut checkpoint_data = Vec::new();
        for (i, state) in self.storage_states.iter().enumerate() {
            let ft: Vec<FieldType> = self.schema.storages[i]
                .fields
                .iter()
                .map(|f| FieldType::from_u8(f.field_type).unwrap_or(FieldType::U8))
                .collect();
            state.write_checkpoint(&mut checkpoint_data, &ft)?;
        }

        // Build delta blob
        let mut delta_raw = Vec::new();
        let mut num_frames = 0u32;
        let mut num_frames_active = 0u32;

        for (time_delta, frame) in &self.segment_buf.frames {
            num_frames += 1;
            if !frame.is_empty() {
                num_frames_active += 1;
            }

            // v0.2 interleaved format
            // Decide compact vs wide: check if all ops fit compact
            let use_compact = frame.ops().all(|op| op.to_compact().is_some());

            // time_delta_ps (LEB128)
            leb128::encode_u64_vec(*time_delta, &mut delta_raw);
            // num_items
            delta_raw.extend_from_slice(&(frame.items.len() as u16).to_le_bytes());

            // Items in insertion order
            for item in &frame.items {
                match item {
                    FrameItem::Op(op) => {
                        if use_compact {
                            let compact = op.to_compact().unwrap();
                            delta_raw.push(TAG_COMPACT_OP);
                            delta_raw.push(compact.action);
                            delta_raw.push(compact.storage_id_lo);
                            delta_raw.extend_from_slice(&compact.slot_index.to_le_bytes());
                            delta_raw.extend_from_slice(&compact.field_index.to_le_bytes());
                            delta_raw.extend_from_slice(&compact.value16.to_le_bytes());
                        } else {
                            delta_raw.push(TAG_WIDE_OP);
                            delta_raw.push(op.action);
                            delta_raw.extend_from_slice(&op.storage_id.to_le_bytes());
                            delta_raw.extend_from_slice(&op.slot_index.to_le_bytes());
                            delta_raw.extend_from_slice(&op.field_index.to_le_bytes());
                            delta_raw.extend_from_slice(&op.value.to_le_bytes());
                        }
                    }
                    FrameItem::Event(ev) => {
                        delta_raw.push(TAG_EVENT);
                        delta_raw.push(0); // reserved
                        delta_raw.extend_from_slice(&ev.event_type_id.to_le_bytes());
                        delta_raw.extend_from_slice(&ev.payload_size.to_le_bytes());
                        delta_raw.extend_from_slice(&ev.payload);
                    }
                }
            }
        }

        // LZ4 compress
        let deltas_compressed = if self.header.flags & F_COMPRESSED != 0 {
            lz4_flex::compress_prepend_size(&delta_raw)
        } else {
            delta_raw.clone()
        };

        // Write segment header
        let seg_header = SegmentHeader {
            segment_magic: SEG_MAGIC,
            flags: 0,
            time_start_ps: self.segment_buf.time_start_ps,
            time_end_ps: self.segment_buf.time_end_ps,
            prev_segment_offset: self.prev_segment_offset,
            checkpoint_size: checkpoint_data.len() as u32,
            deltas_compressed_size: deltas_compressed.len() as u32,
            deltas_raw_size: delta_raw.len() as u32,
            num_frames,
            num_frames_active,
            reserved: 0,
        };
        seg_header.write_to(&mut self.file)?;
        self.file.write_all(&checkpoint_data)?;
        self.file.write_all(&deltas_compressed)?;

        // Track segment
        self.segment_index.push(SegmentIndexEntry {
            offset: seg_offset,
            time_start_ps: self.segment_buf.time_start_ps,
            time_end_ps: self.segment_buf.time_end_ps,
        });

        // Update header
        self.prev_segment_offset = seg_offset;
        self.header.num_segments += 1;
        self.header.tail_offset = seg_offset;

        // Update header on disk (tail_offset is the commit point)
        let current_pos = self.file.stream_position()?;
        self.file.seek(SeekFrom::Start(0))?;
        self.header.write_to(&mut self.file)?;
        self.file.seek(SeekFrom::Start(current_pos))?;

        // Reset segment buffer
        self.segment_buf = SegmentBuffer::new(self.current_time_ps);
        self.last_frame_time_ps = self.current_time_ps;

        Ok(())
    }

    /// Finalize the trace file: flush remaining data, write string table,
    /// segment table, and section table. Sets F_COMPLETE flag.
    pub fn close(mut self) -> io::Result<W> {
        // Flush any remaining segment
        if !self.segment_buf.frames.is_empty() {
            self.flush_segment()?;
        }

        let mut sections = Vec::new();

        // Write string table section
        let string_table_offset = self.file.stream_position()?;
        if !self.string_table.entries.is_empty() {
            let size = self.string_table.write_to(&mut self.file)?;
            self.header.flags |= F_HAS_STRINGS;
            sections.push(SectionEntry {
                section_type: SECTION_STRINGS,
                flags: 0,
                reserved: 0,
                offset: string_table_offset,
                size: size as u64,
            });
        }

        // Write segment table section
        let seg_table_offset = self.file.stream_position()?;
        let mut seg_table_size = 0;
        if !self.segment_index.is_empty() {
            for entry in &self.segment_index {
                entry.write_to(&mut self.file)?;
                seg_table_size += SegmentIndexEntry::SIZE;
            }
            sections.push(SectionEntry {
                section_type: SECTION_SEGMENTS,
                flags: 0,
                reserved: 0,
                offset: seg_table_offset,
                size: seg_table_size as u64,
            });
        }

        // Write section table
        // Pad to 8-byte alignment
        let pos = self.file.stream_position()?;
        let pad = (8 - (pos % 8)) % 8;
        if pad > 0 {
            self.file.write_all(&vec![0u8; pad as usize])?;
        }

        let section_table_offset = self.file.stream_position()?;
        for s in &sections {
            s.write_to(&mut self.file)?;
        }
        // End sentinel
        SectionEntry {
            section_type: SECTION_END,
            flags: 0,
            reserved: 0,
            offset: 0,
            size: 0,
        }
        .write_to(&mut self.file)?;

        // Final header update
        self.header.flags |= F_COMPLETE;
        self.header.total_time_ps = self.current_time_ps;
        self.header.section_table_offset = section_table_offset;
        self.file.seek(SeekFrom::Start(0))?;
        self.header.write_to(&mut self.file)?;

        self.file.flush()?;
        Ok(self.file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{DutDescBuilder, FieldSpec, SchemaBuilder};
    use std::io::Cursor;

    fn make_test_schema_and_dut() -> (DutDesc, Schema) {
        let mut sb = SchemaBuilder::new();
        sb.clock_domain("clk", 1000);
        sb.scope("root", None, None, None);
        sb.scope("core0", Some(0), Some("cpu"), Some(0));

        let stage_enum = sb.enum_type(
            "pipeline_stage",
            &["fetch", "decode", "execute", "writeback"],
        );

        sb.storage(
            "entities",
            1,
            16,
            SF_SPARSE,
            &[
                ("entity_id", FieldSpec::U32),
                ("pc", FieldSpec::U64),
                ("inst_bits", FieldSpec::U32),
            ],
        );

        sb.event(
            "stage_transition",
            1,
            &[
                ("entity_id", FieldSpec::U32),
                ("stage", FieldSpec::Enum(stage_enum)),
            ],
        );

        let mut dut_builder = DutDescBuilder::new();
        dut_builder
            .property("dut_name", "test_core")
            .property("cpu.isa", "RV64GC");
        let dut = dut_builder.build(sb.strings_mut());

        (dut, sb.build())
    }

    #[test]
    fn write_simple_trace() {
        let (dut, schema) = make_test_schema_and_dut();
        let buf = Cursor::new(Vec::new());

        let mut w = Writer::create(buf, &dut, &schema, 10_000).unwrap();

        // Cycle 1: fetch instruction
        w.begin_cycle(1000);
        w.slot_set(0, 0, 0, 0); // entity_id = 0
        w.slot_set(0, 0, 1, 0x8000_0000); // pc
        w.slot_set(0, 0, 2, 0x0000_0013); // nop

        // Emit stage_transition event: entity_id=0, stage=fetch(0)
        let mut payload = Vec::new();
        payload.extend_from_slice(&0u32.to_le_bytes()); // entity_id
        payload.push(0); // stage = fetch
        w.event(0, &payload);
        w.end_cycle().unwrap();

        // Cycle 2: decode
        w.begin_cycle(2000);
        let mut payload = Vec::new();
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.push(1); // stage = decode
        w.event(0, &payload);
        w.end_cycle().unwrap();

        // Cycle 3: execute
        w.begin_cycle(3000);
        let mut payload = Vec::new();
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.push(2); // stage = execute
        w.event(0, &payload);
        w.end_cycle().unwrap();

        // Cycle 4: writeback + retire
        w.begin_cycle(4000);
        let mut payload = Vec::new();
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.push(3); // stage = writeback
        w.event(0, &payload);
        w.slot_clear(0, 0);
        w.end_cycle().unwrap();

        let result = w.close().unwrap();
        let data = result.into_inner();

        // Verify file is non-empty and starts with magic
        assert!(data.len() > FileHeader::SIZE);
        assert_eq!(&data[0..4], &MAGIC);

        // Read back header
        let header = FileHeader::read_from(&mut Cursor::new(&data)).unwrap();
        assert!(header.flags & F_COMPLETE != 0);
        assert_eq!(header.total_time_ps, 4000);
        assert!(header.num_segments >= 1);
    }

    #[test]
    fn interleaved_order_preserved() {
        // Verify that event(flush) then slot_clear() in the same cycle
        // is read back in that exact order (the v0.2 guarantee).
        let (dut, schema) = make_test_schema_and_dut();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("order.uscope");
        let file = std::fs::File::create(&path).unwrap();
        let mut w = Writer::create(file, &dut, &schema, 100_000).unwrap();

        // Cycle 1: set up an entity in slot 0
        w.begin_cycle(1000);
        w.slot_set(0, 0, 0, 42); // entity_id = 42
        w.slot_set(0, 0, 1, 0x8000_0000); // pc
        let mut payload = Vec::new();
        payload.extend_from_slice(&42u32.to_le_bytes());
        payload.push(0); // stage = fetch
        w.event(0, &payload);
        w.end_cycle().unwrap();

        // Cycle 2: event(flush) THEN slot_clear — this is the critical order
        w.begin_cycle(2000);
        let mut flush_payload = Vec::new();
        flush_payload.extend_from_slice(&42u32.to_le_bytes());
        w.event(0, &flush_payload); // flush event first
        w.slot_clear(0, 0); // then clear
        w.end_cycle().unwrap();

        let file = w.close().unwrap();
        drop(file);

        // Read back
        let mut reader = crate::reader::Reader::open(path.to_str().unwrap()).unwrap();
        let (_storages, items) = reader.segment_replay(0).unwrap();

        // Find items at time 2000
        let cycle2_items: Vec<_> = items.iter().filter(|i| i.time_ps() == 2000).collect();

        assert_eq!(cycle2_items.len(), 2, "should have 2 items at t=2000");

        // First item should be the event (flush), second should be the op (clear)
        assert!(
            matches!(cycle2_items[0], crate::state::TimedItem::Event(_)),
            "first item at t=2000 should be event (flush)"
        );
        assert!(
            matches!(cycle2_items[1], crate::state::TimedItem::Op(ref op) if op.action == DA_SLOT_CLEAR),
            "second item at t=2000 should be op (slot_clear)"
        );
    }
}
