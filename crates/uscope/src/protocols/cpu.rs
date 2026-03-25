/// CPU protocol helpers: schema construction and typed writer operations.
/// Implements the CPU protocol conventions from the spec.
use crate::schema::{DutDescBuilder, FieldSpec, SchemaBuilder};
use crate::types::*;
use crate::writer::Writer;
use std::io::{Seek, Write};

/// Metadata for a buffer storage created by the CPU schema builder.
#[derive(Debug, Clone)]
pub struct BufferInfo {
    pub name: String,
    pub storage_id: u16,
    pub entity_id_field: u16,
    pub fields: Vec<(String, u16)>,
}

/// IDs assigned during schema construction.
#[derive(Debug, Clone)]
pub struct CpuIds {
    pub scope_id: u16,
    pub entities_storage_id: u16,
    pub stage_transition_event_id: u16,
    pub annotate_event_id: u16,
    pub dependency_event_id: u16,
    pub flush_event_id: u16,
    pub stall_event_id: u16,
    pub pipeline_stage_enum_id: u8,
    pub dep_type_enum_id: u8,
    pub flush_reason_enum_id: u8,
    pub stall_reason_enum_id: u8,
    // Field indices in entities storage
    pub field_entity_id: u16,
    pub field_pc: u16,
    pub field_inst_bits: u16,
    // Optional buffer/counter storage IDs
    pub buffers: Vec<BufferInfo>,
    pub counters: Vec<(String, u16, u16)>, // (name, storage_id, count_field_index)
}

/// Builder for a CPU protocol schema.
pub struct CpuSchemaBuilder {
    scope_name: String,
    isa: String,
    pipeline_stages: Vec<String>,
    fetch_width: Option<u32>,
    commit_width: Option<u32>,
    elf_path: Option<String>,
    vendor: Option<String>,
    entity_slots: u16,
    buffers: Vec<BufferDef>,
    counters: Vec<String>,
    stall_reasons: Vec<String>,
}

struct BufferDef {
    name: String,
    num_slots: u16,
    extra_fields: Vec<(String, FieldSpec)>,
}

impl CpuSchemaBuilder {
    pub fn new(scope_name: &str) -> Self {
        Self {
            scope_name: scope_name.to_owned(),
            isa: String::new(),
            pipeline_stages: Vec::new(),
            fetch_width: None,
            commit_width: None,
            elf_path: None,
            vendor: None,
            entity_slots: 512,
            buffers: Vec::new(),
            counters: Vec::new(),
            stall_reasons: vec![
                "rob_full".into(),
                "iq_full".into(),
                "lq_full".into(),
                "sq_full".into(),
                "fetch_miss".into(),
                "dcache_miss".into(),
                "frontend_stall".into(),
            ],
        }
    }

    pub fn isa(mut self, isa: &str) -> Self {
        self.isa = isa.to_owned();
        self
    }

    pub fn pipeline_stages(mut self, stages: &[&str]) -> Self {
        self.pipeline_stages = stages.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn fetch_width(mut self, w: u32) -> Self {
        self.fetch_width = Some(w);
        self
    }

    pub fn commit_width(mut self, w: u32) -> Self {
        self.commit_width = Some(w);
        self
    }

    pub fn elf_path(mut self, path: &str) -> Self {
        self.elf_path = Some(path.to_owned());
        self
    }

    pub fn vendor(mut self, v: &str) -> Self {
        self.vendor = Some(v.to_owned());
        self
    }

    pub fn entity_slots(mut self, n: u16) -> Self {
        self.entity_slots = n;
        self
    }

    pub fn buffer(
        mut self,
        name: &str,
        num_slots: u16,
        extra_fields: &[(&str, FieldSpec)],
    ) -> Self {
        self.buffers.push(BufferDef {
            name: name.to_owned(),
            num_slots,
            extra_fields: extra_fields
                .iter()
                .map(|(n, f)| (n.to_string(), f.clone()))
                .collect(),
        });
        self
    }

    pub fn counter(mut self, name: &str) -> Self {
        self.counters.push(name.to_owned());
        self
    }

    pub fn stall_reasons(mut self, reasons: &[&str]) -> Self {
        self.stall_reasons = reasons.iter().map(|s| s.to_string()).collect();
        self
    }

    /// Build the schema and DUT descriptor. Returns (DutDescBuilder, SchemaBuilder, CpuIds).
    pub fn build(self) -> (DutDescBuilder, SchemaBuilder, CpuIds) {
        let mut sb = SchemaBuilder::new();
        let mut dut = DutDescBuilder::new();

        // Clock domain
        let clock_id = sb.clock_domain("core_clk", 1000); // default 1 GHz

        // Root scope
        sb.scope("root", None, None, None);

        // CPU scope
        let scope_id = sb.scope(&self.scope_name, Some(0), Some("cpu"), Some(clock_id));

        // DUT properties
        dut.property("dut_name", &self.scope_name);
        dut.property("cpu.protocol_version", "0.1");
        if !self.isa.is_empty() {
            dut.property("cpu.isa", &self.isa);
        }
        let stages_str = self.pipeline_stages.join(",");
        dut.property("cpu.pipeline_stages", &stages_str);
        if let Some(fw) = self.fetch_width {
            dut.property("cpu.fetch_width", &fw.to_string());
        }
        if let Some(cw) = self.commit_width {
            dut.property("cpu.commit_width", &cw.to_string());
        }
        if let Some(ref path) = self.elf_path {
            dut.property("cpu.elf_path", path);
        }
        if let Some(ref v) = self.vendor {
            dut.property("cpu.vendor", v);
        }

        // Enums
        let stage_refs: Vec<&str> = self.pipeline_stages.iter().map(|s| s.as_str()).collect();
        let pipeline_stage_enum_id = sb.enum_type("pipeline_stage", &stage_refs);
        let dep_type_enum_id = sb.enum_type("dep_type", &["raw", "war", "waw", "structural"]);
        let flush_reason_enum_id = sb.enum_type(
            "flush_reason",
            &["mispredict", "exception", "interrupt", "pipeline_clear"],
        );
        let stall_refs: Vec<&str> = self.stall_reasons.iter().map(|s| s.as_str()).collect();
        let stall_reason_enum_id = sb.enum_type("stall_reason", &stall_refs);

        // Entity catalog
        let entities_storage_id = sb.storage(
            "entities",
            scope_id,
            self.entity_slots,
            SF_SPARSE,
            &[
                ("entity_id", FieldSpec::U32),
                ("pc", FieldSpec::U64),
                ("inst_bits", FieldSpec::U32),
            ],
        );

        // Buffers
        let mut buffer_ids = Vec::new();
        for buf in &self.buffers {
            let mut fields: Vec<(&str, FieldSpec)> = vec![("entity_id", FieldSpec::U32)];
            for (n, f) in &buf.extra_fields {
                fields.push((n.as_str(), f.clone()));
            }
            let sid = sb.storage(&buf.name, scope_id, buf.num_slots, SF_BUFFER, &fields);
            let entity_id_field = 0u16;
            let extra: Vec<(String, u16)> = buf
                .extra_fields
                .iter()
                .enumerate()
                .map(|(i, (n, _))| (n.clone(), (i + 1) as u16))
                .collect();
            buffer_ids.push(BufferInfo {
                name: buf.name.clone(),
                storage_id: sid,
                entity_id_field,
                fields: extra,
            });
        }

        // Counters (1-slot dense storages)
        let mut counter_ids = Vec::new();
        for name in &self.counters {
            let sid = sb.storage(name, scope_id, 1, 0, &[("count", FieldSpec::U64)]);
            counter_ids.push((name.clone(), sid, 0u16));
        }

        // Events
        let stage_transition_event_id = sb.event(
            "stage_transition",
            scope_id,
            &[
                ("entity_id", FieldSpec::U32),
                ("stage", FieldSpec::Enum(pipeline_stage_enum_id)),
            ],
        );

        let annotate_event_id = sb.event(
            "annotate",
            scope_id,
            &[
                ("entity_id", FieldSpec::U32),
                ("text", FieldSpec::StringRef),
            ],
        );

        let dependency_event_id = sb.event(
            "dependency",
            scope_id,
            &[
                ("src_id", FieldSpec::U32),
                ("dst_id", FieldSpec::U32),
                ("dep_type", FieldSpec::Enum(dep_type_enum_id)),
            ],
        );

        let flush_event_id = sb.event(
            "flush",
            scope_id,
            &[
                ("entity_id", FieldSpec::U32),
                ("reason", FieldSpec::Enum(flush_reason_enum_id)),
            ],
        );

        let stall_event_id = sb.event(
            "stall",
            scope_id,
            &[("reason", FieldSpec::Enum(stall_reason_enum_id))],
        );

        let ids = CpuIds {
            scope_id,
            entities_storage_id,
            stage_transition_event_id,
            annotate_event_id,
            dependency_event_id,
            flush_event_id,
            stall_event_id,
            pipeline_stage_enum_id,
            dep_type_enum_id,
            flush_reason_enum_id,
            stall_reason_enum_id,
            field_entity_id: 0,
            field_pc: 1,
            field_inst_bits: 2,
            buffers: buffer_ids,
            counters: counter_ids,
        };

        (dut, sb, ids)
    }
}

/// Typed CPU protocol writer helpers.
pub struct CpuWriter {
    pub ids: CpuIds,
}

impl CpuWriter {
    pub fn new(ids: CpuIds) -> Self {
        Self { ids }
    }

    /// Fetch an instruction: allocate entity in catalog.
    pub fn fetch<W: Write + Seek>(
        &self,
        w: &mut Writer<W>,
        entity_id: u32,
        pc: u64,
        inst_bits: u32,
    ) {
        let slot = entity_id as u16;
        let sid = self.ids.entities_storage_id;
        w.slot_set(sid, slot, self.ids.field_entity_id, entity_id as u64);
        w.slot_set(sid, slot, self.ids.field_pc, pc);
        w.slot_set(sid, slot, self.ids.field_inst_bits, inst_bits as u64);
    }

    /// Emit a stage transition event.
    pub fn stage_transition<W: Write + Seek>(&self, w: &mut Writer<W>, entity_id: u32, stage: u8) {
        let mut payload = Vec::with_capacity(5);
        payload.extend_from_slice(&entity_id.to_le_bytes());
        payload.push(stage);
        w.event(self.ids.stage_transition_event_id, &payload);
    }

    /// Retire an instruction: clear entity from catalog.
    pub fn retire<W: Write + Seek>(&self, w: &mut Writer<W>, entity_id: u32) {
        w.slot_clear(self.ids.entities_storage_id, entity_id as u16);
    }

    /// Flush an instruction.
    ///
    /// The flush event implicitly removes the entity from the active set.
    /// No separate `slot_clear` is emitted — this avoids an ordering race
    /// in the binary frame format where ops and events are stored in
    /// separate arrays and the consumer cannot recover call order.
    pub fn flush<W: Write + Seek>(&self, w: &mut Writer<W>, entity_id: u32, reason: u8) {
        let mut payload = Vec::with_capacity(5);
        payload.extend_from_slice(&entity_id.to_le_bytes());
        payload.push(reason);
        w.event(self.ids.flush_event_id, &payload);
    }

    /// Annotate an entity with text.
    pub fn annotate<W: Write + Seek>(&self, w: &mut Writer<W>, entity_id: u32, text: &str) {
        let text_ref = w.string_table.insert(text);
        let mut payload = Vec::with_capacity(8);
        payload.extend_from_slice(&entity_id.to_le_bytes());
        payload.extend_from_slice(&text_ref.to_le_bytes());
        w.event(self.ids.annotate_event_id, &payload);
    }

    /// Record a dependency between two entities.
    pub fn dependency<W: Write + Seek>(
        &self,
        w: &mut Writer<W>,
        src_id: u32,
        dst_id: u32,
        dep_type: u8,
    ) {
        let mut payload = Vec::with_capacity(9);
        payload.extend_from_slice(&src_id.to_le_bytes());
        payload.extend_from_slice(&dst_id.to_le_bytes());
        payload.push(dep_type);
        w.event(self.ids.dependency_event_id, &payload);
    }

    /// Record a stall event.
    pub fn stall<W: Write + Seek>(&self, w: &mut Writer<W>, reason: u8) {
        w.event(self.ids.stall_event_id, &[reason]);
    }

    /// Increment a counter.
    pub fn counter_add<W: Write + Seek>(&self, w: &mut Writer<W>, counter_name: &str, delta: u64) {
        for (name, sid, field) in &self.ids.counters {
            if name == counter_name {
                w.slot_add(*sid, 0, *field, delta);
                return;
            }
        }
    }

    /// Set a buffer slot to an entity.
    pub fn buffer_set<W: Write + Seek>(
        &self,
        w: &mut Writer<W>,
        name: &str,
        slot: u16,
        entity_id: u32,
    ) {
        for buf in &self.ids.buffers {
            if buf.name == name {
                w.slot_set(buf.storage_id, slot, buf.entity_id_field, entity_id as u64);
                return;
            }
        }
    }

    /// Set an additional field on a buffer slot.
    pub fn buffer_set_field<W: Write + Seek>(
        &self,
        w: &mut Writer<W>,
        name: &str,
        slot: u16,
        field_name: &str,
        value: u64,
    ) {
        for buf in &self.ids.buffers {
            if buf.name == name {
                for (fname, fidx) in &buf.fields {
                    if fname == field_name {
                        w.slot_set(buf.storage_id, slot, *fidx, value);
                        return;
                    }
                }
                return;
            }
        }
    }

    /// Clear a buffer slot.
    pub fn buffer_clear<W: Write + Seek>(&self, w: &mut Writer<W>, name: &str, slot: u16) {
        for buf in &self.ids.buffers {
            if buf.name == name {
                w.slot_clear(buf.storage_id, slot);
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn cpu_schema_builder() {
        let (dut_builder, mut sb, ids) = CpuSchemaBuilder::new("core0")
            .isa("RV64GC")
            .pipeline_stages(&["fetch", "decode", "execute", "memory", "writeback"])
            .fetch_width(4)
            .commit_width(4)
            .entity_slots(256)
            .buffer("rob", 128, &[("completed", FieldSpec::Bool)])
            .counter("committed_insns")
            .build();

        let _dut = dut_builder.build(sb.strings_mut());
        let schema = sb.build();

        // Verify schema structure
        assert_eq!(schema.clock_domains.len(), 1);
        assert_eq!(schema.scopes.len(), 2); // root + core0
        assert_eq!(schema.enums.len(), 4); // pipeline_stage, dep_type, flush_reason, stall_reason
        assert_eq!(schema.storages.len(), 3); // entities, rob, committed_insns
        assert_eq!(schema.events.len(), 5); // stage_transition, annotate, dependency, flush, stall

        assert_eq!(ids.entities_storage_id, 0);
        assert_eq!(ids.buffers.len(), 1);
        assert_eq!(ids.counters.len(), 1);
    }

    #[test]
    fn cpu_write_trace() {
        let (dut_builder, mut sb, ids) = CpuSchemaBuilder::new("core0")
            .isa("RV64GC")
            .pipeline_stages(&["fetch", "decode", "execute", "writeback"])
            .entity_slots(16)
            .counter("committed_insns")
            .build();

        let dut = dut_builder.build(sb.strings_mut());
        let schema = sb.build();

        let buf = Cursor::new(Vec::new());
        let mut w = Writer::create(buf, &dut, &schema, 100_000).unwrap();
        let cpu = CpuWriter::new(ids);

        // Cycle 1: fetch
        w.begin_cycle(1000);
        cpu.fetch(&mut w, 0, 0x8000_0000, 0x13);
        cpu.stage_transition(&mut w, 0, 0); // fetch
        w.end_cycle().unwrap();

        // Cycle 2: decode
        w.begin_cycle(2000);
        cpu.stage_transition(&mut w, 0, 1); // decode
        w.end_cycle().unwrap();

        // Cycle 3: execute
        w.begin_cycle(3000);
        cpu.stage_transition(&mut w, 0, 2); // execute
        w.end_cycle().unwrap();

        // Cycle 4: writeback + retire
        w.begin_cycle(4000);
        cpu.stage_transition(&mut w, 0, 3); // writeback
        cpu.retire(&mut w, 0);
        cpu.counter_add(&mut w, "committed_insns", 1);
        w.end_cycle().unwrap();

        let result = w.close().unwrap();
        let data = result.into_inner();
        assert!(data.len() > FileHeader::SIZE);
    }
}
