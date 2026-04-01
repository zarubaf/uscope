/// Schema builder and binary serialization for the µScope format.
use crate::string_pool::StringPoolBuilder;
use crate::types::*;
use std::io::{self, Read, Write};

/// High-level field specification used in the builder API.
#[derive(Debug, Clone)]
pub enum FieldSpec {
    U8,
    U16,
    U32,
    U64,
    I8,
    I16,
    I32,
    I64,
    Bool,
    StringRef,
    Enum(u8), // enum_id
}

impl FieldSpec {
    fn field_type(&self) -> u8 {
        match self {
            Self::U8 => FieldType::U8 as u8,
            Self::U16 => FieldType::U16 as u8,
            Self::U32 => FieldType::U32 as u8,
            Self::U64 => FieldType::U64 as u8,
            Self::I8 => FieldType::I8 as u8,
            Self::I16 => FieldType::I16 as u8,
            Self::I32 => FieldType::I32 as u8,
            Self::I64 => FieldType::I64 as u8,
            Self::Bool => FieldType::Bool as u8,
            Self::StringRef => FieldType::StringRef as u8,
            Self::Enum(_) => FieldType::Enum as u8,
        }
    }

    fn enum_id(&self) -> u8 {
        match self {
            Self::Enum(id) => *id,
            _ => 0,
        }
    }
}

/// A fully built schema ready for serialization.
#[derive(Debug, Clone)]
pub struct Schema {
    pub clock_domains: Vec<ClockDomainDef>,
    pub scopes: Vec<ScopeDef>,
    pub enums: Vec<EnumDef>,
    pub storages: Vec<StorageDef>,
    pub events: Vec<EventDef>,
    pub summary_fields: Vec<SummaryFieldDef>,
    pub string_pool: Vec<u8>,
}

impl Schema {
    /// Serialize the schema to binary format.
    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        let header = SchemaHeader {
            num_enums: self.enums.len() as u8,
            num_clock_domains: self.clock_domains.len() as u8,
            num_scopes: self.scopes.len() as u16,
            num_storages: self.storages.len() as u16,
            num_event_types: self.events.len() as u16,
            num_summary_fields: self.summary_fields.len() as u16,
            string_pool_offset: 0, // placeholder, computed below
        };

        // Compute string pool offset:
        // header(14) + clock_domains + scopes + enums + storages + events + summary_fields
        let mut sp_offset = SchemaHeader::SIZE;
        sp_offset += self.clock_domains.len() * ClockDomainDef::SIZE;
        sp_offset += self.scopes.len() * ScopeDef::SIZE;
        for e in &self.enums {
            sp_offset += EnumDef::HEADER_SIZE + e.values.len() * EnumValue::SIZE;
        }
        for s in &self.storages {
            sp_offset += StorageDef::HEADER_SIZE
                + s.fields.len() * FieldDef::SIZE
                + s.properties.len() * FieldDef::SIZE;
        }
        for e in &self.events {
            sp_offset += EventDef::HEADER_SIZE + e.fields.len() * FieldDef::SIZE;
        }
        sp_offset += self.summary_fields.len() * SummaryFieldDef::SIZE;

        let header = SchemaHeader {
            string_pool_offset: sp_offset as u16,
            ..header
        };

        header.write_to(w)?;

        for cd in &self.clock_domains {
            cd.write_to(w)?;
        }
        for s in &self.scopes {
            s.write_to(w)?;
        }
        for e in &self.enums {
            e.write_to(w)?;
        }
        for s in &self.storages {
            s.write_to(w)?;
        }
        for e in &self.events {
            e.write_to(w)?;
        }
        for sf in &self.summary_fields {
            sf.write_to(w)?;
        }
        w.write_all(&self.string_pool)?;
        Ok(())
    }

    /// Deserialize a schema from binary format.
    pub fn read_from<R: Read>(r: &mut R, total_size: usize) -> io::Result<Self> {
        let header = SchemaHeader::read_from(r)?;

        let mut clock_domains = Vec::with_capacity(header.num_clock_domains as usize);
        for _ in 0..header.num_clock_domains {
            clock_domains.push(ClockDomainDef::read_from(r)?);
        }

        let mut scopes = Vec::with_capacity(header.num_scopes as usize);
        for _ in 0..header.num_scopes {
            scopes.push(ScopeDef::read_from(r)?);
        }

        let mut enums = Vec::with_capacity(header.num_enums as usize);
        for _ in 0..header.num_enums {
            enums.push(EnumDef::read_from(r)?);
        }

        let mut storages = Vec::with_capacity(header.num_storages as usize);
        for _ in 0..header.num_storages {
            storages.push(StorageDef::read_from(r)?);
        }

        let mut events = Vec::with_capacity(header.num_event_types as usize);
        for _ in 0..header.num_event_types {
            events.push(EventDef::read_from(r)?);
        }

        let mut summary_fields = Vec::with_capacity(header.num_summary_fields as usize);
        for _ in 0..header.num_summary_fields {
            summary_fields.push(SummaryFieldDef::read_from(r)?);
        }

        let sp_size = total_size - header.string_pool_offset as usize;
        let mut string_pool = vec![0u8; sp_size];
        r.read_exact(&mut string_pool)?;

        Ok(Self {
            clock_domains,
            scopes,
            enums,
            storages,
            events,
            summary_fields,
            string_pool,
        })
    }

    /// Look up a string in the schema's string pool.
    pub fn get_string(&self, offset: u16) -> Option<&str> {
        crate::string_pool::StringPoolReader::new(&self.string_pool).get(offset)
    }
}

/// Fluent builder for constructing a Schema.
#[derive(Debug)]
pub struct SchemaBuilder {
    strings: StringPoolBuilder,
    clock_domains: Vec<ClockDomainDef>,
    scopes: Vec<ScopeDef>,
    enums: Vec<EnumDef>,
    storages: Vec<StorageDef>,
    events: Vec<EventDef>,
    summary_fields: Vec<SummaryFieldDef>,
}

impl SchemaBuilder {
    pub fn new() -> Self {
        Self {
            strings: StringPoolBuilder::new(),
            clock_domains: Vec::new(),
            scopes: Vec::new(),
            enums: Vec::new(),
            storages: Vec::new(),
            events: Vec::new(),
            summary_fields: Vec::new(),
        }
    }

    /// Add a clock domain. Returns clock_id.
    pub fn clock_domain(&mut self, name: &str, period_ps: u32) -> u8 {
        let id = self.clock_domains.len() as u8;
        let name_off = self.strings.insert(name);
        self.clock_domains.push(ClockDomainDef {
            name: name_off,
            clock_id: id as u16,
            period_ps,
        });
        id
    }

    /// Add a scope. Returns scope_id.
    pub fn scope(
        &mut self,
        name: &str,
        parent_id: Option<u16>,
        protocol: Option<&str>,
        clock_id: Option<u8>,
    ) -> u16 {
        let id = self.scopes.len() as u16;
        let name_off = self.strings.insert(name);
        let protocol_off = protocol
            .map(|p| self.strings.insert(p))
            .unwrap_or(ScopeDef::NO_PROTOCOL);
        self.scopes.push(ScopeDef {
            name: name_off,
            scope_id: id,
            parent_id: parent_id.unwrap_or(ScopeDef::NO_PARENT),
            protocol: protocol_off,
            clock_id: clock_id.unwrap_or(ScopeDef::INHERIT_CLOCK),
            reserved: [0; 3],
        });
        id
    }

    /// Add an enum type. Returns enum_id.
    pub fn enum_type(&mut self, name: &str, values: &[&str]) -> u8 {
        let id = self.enums.len() as u8;
        let name_off = self.strings.insert(name);
        let vals: Vec<EnumValue> = values
            .iter()
            .enumerate()
            .map(|(i, v)| EnumValue {
                value: i as u8,
                reserved: 0,
                name: self.strings.insert(v),
            })
            .collect();
        self.enums.push(EnumDef {
            name: name_off,
            num_values: vals.len() as u8,
            reserved: 0,
            values: vals,
        });
        id
    }

    /// Add a storage definition. Returns storage_id.
    pub fn storage(
        &mut self,
        name: &str,
        scope_id: u16,
        num_slots: u16,
        flags: u16,
        fields: &[(&str, FieldSpec)],
    ) -> u16 {
        self.storage_with_properties(name, scope_id, num_slots, flags, fields, &[])
    }

    /// Add a storage definition with storage-level properties (v0.3). Returns storage_id.
    ///
    /// Each property is `(name, spec, role, pair_id)` where role is one of
    /// `PROP_ROLE_PLAIN`, `PROP_ROLE_HEAD_PTR`, `PROP_ROLE_TAIL_PTR` and
    /// pair_id groups head/tail pointers into pairs.
    pub fn storage_with_properties(
        &mut self,
        name: &str,
        scope_id: u16,
        num_slots: u16,
        flags: u16,
        fields: &[(&str, FieldSpec)],
        properties: &[(&str, FieldSpec, u8, u8)],
    ) -> u16 {
        let id = self.storages.len() as u16;
        let name_off = self.strings.insert(name);
        let field_defs: Vec<FieldDef> = fields
            .iter()
            .map(|(n, spec)| FieldDef {
                name: self.strings.insert(n),
                field_type: spec.field_type(),
                enum_id: spec.enum_id(),
                role: 0,
                pair_id: 0,
                reserved: [0; 2],
            })
            .collect();
        let prop_defs: Vec<FieldDef> = properties
            .iter()
            .map(|(n, spec, role, pair_id)| FieldDef {
                name: self.strings.insert(n),
                field_type: spec.field_type(),
                enum_id: spec.enum_id(),
                role: *role,
                pair_id: *pair_id,
                reserved: [0; 2],
            })
            .collect();
        self.storages.push(StorageDef {
            name: name_off,
            storage_id: id,
            num_slots,
            num_fields: field_defs.len() as u16,
            flags,
            scope_id,
            num_properties: prop_defs.len() as u16,
            reserved_v3: 0,
            fields: field_defs,
            properties: prop_defs,
        });
        id
    }

    /// Add an event type. Returns event_type_id.
    pub fn event(&mut self, name: &str, scope_id: u16, fields: &[(&str, FieldSpec)]) -> u16 {
        let id = self.events.len() as u16;
        let name_off = self.strings.insert(name);
        let field_defs: Vec<FieldDef> = fields
            .iter()
            .map(|(n, spec)| FieldDef {
                name: self.strings.insert(n),
                field_type: spec.field_type(),
                enum_id: spec.enum_id(),
                role: 0,
                pair_id: 0,
                reserved: [0; 2],
            })
            .collect();
        self.events.push(EventDef {
            name: name_off,
            event_type_id: id,
            num_fields: field_defs.len() as u16,
            scope_id,
            fields: field_defs,
        });
        id
    }

    /// Add a summary field definition.
    pub fn summary_field(&mut self, name: &str, field_type: FieldSpec, scope_id: u16) {
        let name_off = self.strings.insert(name);
        self.summary_fields.push(SummaryFieldDef {
            name: name_off,
            field_type: field_type.field_type(),
            reserved: 0,
            scope_id,
            reserved2: 0,
        });
    }

    /// Build the final Schema.
    pub fn build(self) -> Schema {
        Schema {
            clock_domains: self.clock_domains,
            scopes: self.scopes,
            enums: self.enums,
            storages: self.storages,
            events: self.events,
            summary_fields: self.summary_fields,
            string_pool: self.strings.data().to_vec(),
        }
    }

    /// Access the string pool builder (for DUT property key/value insertion).
    pub fn strings_mut(&mut self) -> &mut StringPoolBuilder {
        &mut self.strings
    }
}

impl Default for SchemaBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper to build DUT descriptor using a shared string pool.
#[derive(Debug)]
pub struct DutDescBuilder {
    properties: Vec<(String, String)>,
}

impl DutDescBuilder {
    pub fn new() -> Self {
        Self {
            properties: Vec::new(),
        }
    }

    pub fn property(&mut self, key: &str, value: &str) -> &mut Self {
        self.properties.push((key.to_owned(), value.to_owned()));
        self
    }

    /// Serialize using the given string pool (shared with SchemaBuilder).
    pub fn build(&self, strings: &mut StringPoolBuilder) -> DutDesc {
        let properties = self
            .properties
            .iter()
            .map(|(k, v)| DutProperty {
                key: strings.insert(k),
                value: strings.insert(v),
            })
            .collect();
        DutDesc { properties }
    }
}

impl Default for DutDescBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn schema_roundtrip() {
        let mut builder = SchemaBuilder::new();
        builder.clock_domain("core_clk", 200);
        builder.scope("root", None, None, None);
        builder.scope("core0", Some(0), Some("cpu"), Some(0));
        let stage_enum = builder.enum_type(
            "pipeline_stage",
            &["fetch", "decode", "execute", "memory", "writeback"],
        );
        builder.storage(
            "entities",
            1,
            512,
            SF_SPARSE,
            &[
                ("entity_id", FieldSpec::U32),
                ("pc", FieldSpec::U64),
                ("inst_bits", FieldSpec::U32),
            ],
        );
        builder.event(
            "stage_transition",
            1,
            &[
                ("entity_id", FieldSpec::U32),
                ("stage", FieldSpec::Enum(stage_enum)),
            ],
        );

        let schema = builder.build();

        // Serialize
        let mut buf = Vec::new();
        schema.write_to(&mut buf).unwrap();

        // Deserialize
        let decoded = Schema::read_from(&mut Cursor::new(&buf), buf.len()).unwrap();

        assert_eq!(decoded.clock_domains.len(), 1);
        assert_eq!(decoded.clock_domains[0].period_ps, 200);
        assert_eq!(decoded.scopes.len(), 2);
        assert_eq!(decoded.enums.len(), 1);
        assert_eq!(decoded.enums[0].values.len(), 5);
        assert_eq!(decoded.storages.len(), 1);
        assert_eq!(decoded.storages[0].num_slots, 512);
        assert_eq!(decoded.storages[0].fields.len(), 3);
        assert_eq!(decoded.events.len(), 1);
        assert_eq!(decoded.events[0].fields.len(), 2);

        // Check strings round-trip
        assert_eq!(
            decoded.get_string(decoded.clock_domains[0].name),
            Some("core_clk")
        );
        assert_eq!(decoded.get_string(decoded.scopes[1].name), Some("core0"));
    }

    #[test]
    fn schema_roundtrip_with_properties() {
        let mut builder = SchemaBuilder::new();
        builder.clock_domain("core_clk", 200);
        builder.scope("root", None, None, None);
        builder.scope("core0", Some(0), Some("cpu"), Some(0));
        builder.storage_with_properties(
            "rob",
            1,
            128,
            SF_BUFFER,
            &[("entity_id", FieldSpec::U32)],
            &[
                ("retire_ptr", FieldSpec::U16, PROP_ROLE_HEAD_PTR, 0),
                ("allocate_ptr", FieldSpec::U16, PROP_ROLE_TAIL_PTR, 0),
            ],
        );

        let schema = builder.build();

        // Serialize
        let mut buf = Vec::new();
        schema.write_to(&mut buf).unwrap();

        // Deserialize
        let decoded = Schema::read_from(&mut Cursor::new(&buf), buf.len()).unwrap();

        assert_eq!(decoded.storages.len(), 1);
        assert_eq!(decoded.storages[0].num_slots, 128);
        assert_eq!(decoded.storages[0].fields.len(), 1);
        assert_eq!(decoded.storages[0].num_properties, 2);
        assert_eq!(decoded.storages[0].properties.len(), 2);

        // Check property names round-trip
        assert_eq!(
            decoded.get_string(decoded.storages[0].properties[0].name),
            Some("retire_ptr")
        );
        assert_eq!(
            decoded.get_string(decoded.storages[0].properties[1].name),
            Some("allocate_ptr")
        );
    }

    #[test]
    fn dut_desc_roundtrip() {
        let mut strings = StringPoolBuilder::new();
        let mut dut_builder = DutDescBuilder::new();
        dut_builder
            .property("dut_name", "boom_core_0")
            .property("cpu.isa", "RV64GC");
        let dut = dut_builder.build(&mut strings);

        let mut buf = Vec::new();
        dut.write_to(&mut buf).unwrap();

        let decoded = DutDesc::read_from(&mut Cursor::new(&buf)).unwrap();
        assert_eq!(decoded.properties.len(), 2);

        let reader = crate::string_pool::StringPoolReader::new(strings.data());
        assert_eq!(reader.get(decoded.properties[0].key), Some("dut_name"));
        assert_eq!(reader.get(decoded.properties[0].value), Some("boom_core_0"));
    }
}
