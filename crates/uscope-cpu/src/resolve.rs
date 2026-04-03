use crate::types::{BufferInfo, BufferPropertyDef};
use std::io;
use std::path::Path;
use uscope::reader::Reader;
use uscope::types::*;

/// Resolved CPU protocol IDs from the schema.
pub struct CpuProtocolIds {
    pub period_ps: u64,
    pub entities_storage_id: u16,
    pub field_entity_id: u16,
    pub field_pc: u16,
    pub field_inst_bits: Option<u16>,
    pub field_rbid: Option<u16>,
    pub field_iq_id: Option<u16>,
    pub field_dq_id: Option<u16>,
    pub field_ready_time_ps: Option<u16>,
    pub stage_transition_event_id: u16,
    pub annotate_event_id: u16,
    pub dependency_event_id: u16,
    pub flush_event_id: u16,
    pub stage_names: Vec<String>,
}

/// Resolve CPU protocol field/event IDs from the uscope schema.
pub fn resolve_cpu_protocol(reader: &Reader) -> io::Result<CpuProtocolIds> {
    let schema = reader.schema();

    // Find scope with protocol == "cpu"
    let cpu_scope = schema
        .scopes
        .iter()
        .find(|s| schema.get_string(s.protocol) == Some("cpu"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no CPU protocol scope found"))?;

    // Get clock period
    let clock_id = cpu_scope.clock_id;
    let period_ps = schema
        .clock_domains
        .get(clock_id as usize)
        .map(|cd| cd.period_ps as u64)
        .unwrap_or(1000); // default 1 GHz

    // Find entities storage (name == "entities" in this scope)
    let entities_storage = schema
        .storages
        .iter()
        .find(|s| s.scope_id == cpu_scope.scope_id && schema.get_string(s.name) == Some("entities"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no entities storage found"))?;

    // Find field indices by name
    let field_entity_id = find_field_index(schema, entities_storage, "entity_id")?;
    let field_pc = find_field_index(schema, entities_storage, "pc")?;
    let field_inst_bits = find_field_index(schema, entities_storage, "inst_bits").ok();
    let field_rbid = find_field_index(schema, entities_storage, "rbid").ok();
    let field_iq_id = find_field_index(schema, entities_storage, "iq_id").ok();
    let field_dq_id = find_field_index(schema, entities_storage, "dq_id").ok();
    let field_ready_time_ps = find_field_index(schema, entities_storage, "ready_time_ps").ok();

    // Find events by name in the CPU scope
    let find_event = |name: &str| -> io::Result<u16> {
        schema
            .events
            .iter()
            .find(|e| e.scope_id == cpu_scope.scope_id && schema.get_string(e.name) == Some(name))
            .map(|e| e.event_type_id)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("no '{}' event found", name),
                )
            })
    };

    let stage_transition_event_id = find_event("stage_transition")?;
    let annotate_event_id = find_event("annotate")?;
    let dependency_event_id = find_event("dependency")?;
    let flush_event_id = find_event("flush")?;

    // Read pipeline stage names from the pipeline_stage enum
    let stage_names = read_stage_names(reader)?;

    Ok(CpuProtocolIds {
        period_ps,
        entities_storage_id: entities_storage.storage_id,
        field_entity_id,
        field_pc,
        field_inst_bits,
        field_rbid,
        field_iq_id,
        field_dq_id,
        field_ready_time_ps,
        stage_transition_event_id,
        annotate_event_id,
        dependency_event_id,
        flush_event_id,
        stage_names,
    })
}

/// Find the index of a field by name within a storage definition.
pub fn find_field_index(
    schema: &uscope::schema::Schema,
    storage: &StorageDef,
    name: &str,
) -> io::Result<u16> {
    storage
        .fields
        .iter()
        .position(|f| schema.get_string(f.name) == Some(name))
        .map(|i| i as u16)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("field '{}' not found in entities", name),
            )
        })
}

/// Read pipeline stage names from the DUT property or enum.
pub fn read_stage_names(reader: &Reader) -> io::Result<Vec<String>> {
    // Try DUT property first (canonical ordering)
    if let Some(stages_str) = reader.dut_property("cpu.pipeline_stages") {
        let names: Vec<String> = stages_str
            .split(',')
            .map(|s| s.trim().to_string())
            .collect();
        if !names.is_empty() {
            return Ok(names);
        }
    }

    // Fallback: read from enum
    let schema = reader.schema();
    let pipeline_enum = schema
        .enums
        .iter()
        .find(|e| schema.get_string(e.name) == Some("pipeline_stage"))
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "no pipeline_stage enum found")
        })?;

    let mut names: Vec<String> = pipeline_enum
        .values
        .iter()
        .map(|v| schema.get_string(v.name).unwrap_or("?").to_string())
        .collect();

    if names.is_empty() {
        names.push("unknown".to_string());
    }

    Ok(names)
}

/// Detect counter storages: 1-slot, dense, not buffer, single U64 field.
pub fn detect_counter_storages(reader: &Reader) -> Vec<(u16, String)> {
    let schema = reader.schema();
    schema
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
        .collect()
}

/// Detect buffer storages: have SF_BUFFER flag.
pub fn detect_buffer_infos(reader: &Reader) -> Vec<BufferInfo> {
    let schema = reader.schema();
    schema
        .storages
        .iter()
        .filter(|s| s.is_buffer())
        .map(|s| {
            let name = schema.get_string(s.name).unwrap_or("?").to_string();
            let fields: Vec<(String, u8)> = s
                .fields
                .iter()
                .map(|f| {
                    (
                        schema.get_string(f.name).unwrap_or("?").to_string(),
                        f.field_type,
                    )
                })
                .collect();
            let properties: Vec<BufferPropertyDef> = s
                .properties
                .iter()
                .filter_map(|f| {
                    schema.get_string(f.name).map(|n| BufferPropertyDef {
                        name: n.to_string(),
                        field_type: f.field_type,
                        role: f.role,
                        pair_id: f.pair_id,
                    })
                })
                .collect();
            BufferInfo {
                name,
                storage_id: s.storage_id,
                capacity: s.num_slots,
                fields,
                properties,
            }
        })
        .collect()
}

/// Populate metadata key-value pairs from the uscope file.
pub fn populate_metadata(
    reader: &Reader,
    path: &Path,
    ids: &CpuProtocolIds,
) -> Vec<(String, String)> {
    let header = reader.header();
    let schema = reader.schema();
    let mut metadata = Vec::new();

    // File info
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    metadata.push(("File".into(), file_name));
    metadata.push((
        "Format".into(),
        format!("uScope v{}.{}", header.version_major, header.version_minor),
    ));

    // Flags
    let mut flags = Vec::new();
    if header.flags & F_COMPRESSED != 0 {
        flags.push("compressed");
    }
    if header.flags & F_INTERLEAVED_DELTAS != 0 {
        flags.push("interleaved");
    }
    if header.flags & F_COMPACT_DELTAS != 0 && header.flags & F_INTERLEAVED_DELTAS == 0 {
        flags.push("compact-deltas");
    }
    if !flags.is_empty() {
        metadata.push(("Flags".into(), flags.join(", ")));
    }

    // DUT properties (all of them)
    for (key, value) in reader.dut_properties() {
        metadata.push((key.to_string(), value.to_string()));
    }

    // Clock
    if !schema.clock_domains.is_empty() {
        let cd = &schema.clock_domains[0];
        let freq_mhz = 1_000_000.0 / cd.period_ps as f64;
        let name = schema.get_string(cd.name).unwrap_or("?");
        metadata.push((
            "Clock".into(),
            format!("{} ({} ps, {:.0} MHz)", name, cd.period_ps, freq_mhz),
        ));
    }

    // Pipeline stages
    metadata.push(("Pipeline".into(), ids.stage_names.join(" -> ")));

    // Trace stats
    let total_us = header.total_time_ps as f64 / 1e6;
    let total_cycles = header.total_time_ps / ids.period_ps;
    metadata.push((
        "Duration".into(),
        format!("{} cycles ({:.1} us)", total_cycles, total_us),
    ));
    metadata.push(("Segments".into(), format!("{}", header.num_segments)));

    // Schema summary
    metadata.push((
        "Schema".into(),
        format!(
            "{} storages, {} events, {} enums",
            schema.storages.len(),
            schema.events.len(),
            schema.enums.len(),
        ),
    ));

    // String table
    if let Some(st) = reader.string_table() {
        let mut count = 0u32;
        while st.get(count).is_some() {
            count += 1;
        }
        metadata.push(("Strings".into(), format!("{} entries", count)));
    }

    metadata
}

/// Build a SegmentIndex by reading the segment table directly from the file.
///
/// This avoids replaying segments -- we only need per-segment time bounds which
/// are stored in the file's section table (for finalized traces) or segment
/// chain (for live traces).
pub fn build_segment_index_from_file(
    path_str: &str,
    reader: &Reader,
    period_ps: u64,
) -> io::Result<crate::types::SegmentIndex> {
    use std::io::{BufReader, Seek, SeekFrom};

    let file = std::fs::File::open(path_str)?;
    let mut file = BufReader::new(file);

    let header = FileHeader::read_from(&mut file)?;

    let mut seg_entries: Vec<SegmentIndexEntry> = Vec::new();

    if header.flags & F_COMPLETE != 0 && header.section_table_offset != 0 {
        // Finalized file: read section table to find the segments section.
        file.seek(SeekFrom::Start(header.section_table_offset))?;
        // Read section entries. The table is terminated by SECTION_END.
        // Guard against corrupt files with a max iteration count.
        for _ in 0..64 {
            let entry = match SectionEntry::read_from(&mut file) {
                Ok(e) => e,
                Err(_) => break, // EOF or corrupt -- stop gracefully
            };
            if entry.section_type == SECTION_END {
                break;
            }
            if entry.section_type == SECTION_SEGMENTS {
                let next_entry_pos = file.stream_position().unwrap_or(0);
                if let Ok(entries) =
                    uscope::segment::read_segment_table(&mut file, entry.offset, entry.size)
                {
                    seg_entries = entries;
                }
                let _ = file.seek(SeekFrom::Start(next_entry_pos));
            }
        }
    }

    // Fallback: walk the segment chain if no section table was found.
    if seg_entries.is_empty() && header.tail_offset != 0 {
        let chain = uscope::segment::walk_chain(&mut file, header.tail_offset)?;
        seg_entries = chain
            .into_iter()
            .map(|(offset, seg)| SegmentIndexEntry {
                offset,
                time_start_ps: seg.time_start_ps,
                time_end_ps: seg.time_end_ps,
            })
            .collect();
    }

    // If we still have nothing, fall back to segment_count from the Reader
    // with uniform distribution (best effort).
    if seg_entries.is_empty() {
        let n = reader.segment_count();
        if n > 0 && header.total_time_ps > 0 {
            let per_seg = header.total_time_ps / n as u64;
            let mut segs = Vec::with_capacity(n);
            for i in 0..n {
                let start_ps = i as u64 * per_seg;
                let end_ps = start_ps + per_seg;
                let start_cycle = (start_ps / period_ps) as u32;
                let end_cycle = (end_ps / period_ps) as u32;
                segs.push((start_cycle, end_cycle));
            }
            return Ok(crate::types::SegmentIndex { segments: segs });
        }
        return Ok(crate::types::SegmentIndex::default());
    }

    // Convert ps-based time bounds to cycles.
    let segments: Vec<(u32, u32)> = seg_entries
        .iter()
        .map(|e| {
            let start_cycle = (e.time_start_ps / period_ps) as u32;
            let end_cycle = (e.time_end_ps / period_ps) as u32;
            (start_cycle, end_cycle)
        })
        .collect();

    Ok(crate::types::SegmentIndex { segments })
}
