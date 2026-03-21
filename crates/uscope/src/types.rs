/// Wire-format types for the µScope trace format.
/// All multi-byte integers are little-endian.

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::io::{self, Read, Write};

// ── Magic numbers ──────────────────────────────────────────────────

pub const MAGIC: [u8; 4] = [0x75, 0x53, 0x43, 0x50]; // "uSCP"
pub const SEG_MAGIC: [u8; 4] = [0x75, 0x53, 0x45, 0x47]; // "uSEG"

pub const VERSION_MAJOR: u16 = 0;
pub const VERSION_MINOR: u16 = 1;

// ── File header flags ──────────────────────────────────────────────

pub const F_COMPLETE: u64 = 1 << 0;
pub const F_COMPRESSED: u64 = 1 << 1;
pub const F_HAS_STRINGS: u64 = 1 << 2;
/// Bits 3-5: compression method (0=LZ4, 1=ZSTD)
pub const F_COMP_METHOD_SHIFT: u32 = 3;
pub const F_COMP_METHOD_MASK: u64 = 0b111 << F_COMP_METHOD_SHIFT;
pub const COMP_LZ4: u64 = 0;
pub const COMP_ZSTD: u64 = 1;
pub const F_COMPACT_DELTAS: u64 = 1 << 6;

// ── Storage flags ──────────────────────────────────────────────────

pub const SF_SPARSE: u16 = 1 << 0;

// ── Preamble chunk types ───────────────────────────────────────────

pub const CHUNK_END: u16 = 0x0000;
pub const CHUNK_DUT_DESC: u16 = 0x0001;
pub const CHUNK_SCHEMA: u16 = 0x0002;
pub const CHUNK_TRACE_CONFIG: u16 = 0x0003;

// ── Section types ──────────────────────────────────────────────────

pub const SECTION_END: u16 = 0x0000;
pub const SECTION_SUMMARY: u16 = 0x0001;
pub const SECTION_STRINGS: u16 = 0x0002;
pub const SECTION_SEGMENTS: u16 = 0x0003;

// ── Delta actions ──────────────────────────────────────────────────

pub const DA_SLOT_SET: u8 = 0x01;
pub const DA_SLOT_CLEAR: u8 = 0x02;
pub const DA_SLOT_ADD: u8 = 0x03;

// ── Field types ────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FieldType {
    U8 = 0x01,
    U16 = 0x02,
    U32 = 0x03,
    U64 = 0x04,
    I8 = 0x05,
    I16 = 0x06,
    I32 = 0x07,
    I64 = 0x08,
    Bool = 0x09,
    StringRef = 0x0A,
    Enum = 0x0B,
}

impl FieldType {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0x01 => Some(Self::U8),
            0x02 => Some(Self::U16),
            0x03 => Some(Self::U32),
            0x04 => Some(Self::U64),
            0x05 => Some(Self::I8),
            0x06 => Some(Self::I16),
            0x07 => Some(Self::I32),
            0x08 => Some(Self::I64),
            0x09 => Some(Self::Bool),
            0x0A => Some(Self::StringRef),
            0x0B => Some(Self::Enum),
            _ => None,
        }
    }

    /// Size in bytes of a value of this field type.
    pub fn size(self) -> usize {
        match self {
            Self::U8 | Self::I8 | Self::Bool | Self::Enum => 1,
            Self::U16 | Self::I16 => 2,
            Self::U32 | Self::I32 | Self::StringRef => 4,
            Self::U64 | Self::I64 => 8,
        }
    }
}

// ── File Header (48 bytes) ─────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct FileHeader {
    pub magic: [u8; 4],
    pub version_major: u16,
    pub version_minor: u16,
    pub flags: u64,
    pub total_time_ps: u64,
    pub num_segments: u32,
    pub preamble_end: u32,
    pub section_table_offset: u64,
    pub tail_offset: u64,
}

impl FileHeader {
    pub const SIZE: usize = 48;

    pub fn new() -> Self {
        Self {
            magic: MAGIC,
            version_major: VERSION_MAJOR,
            version_minor: VERSION_MINOR,
            flags: F_COMPRESSED | F_COMPACT_DELTAS, // LZ4 + compact deltas by default
            total_time_ps: 0,
            num_segments: 0,
            preamble_end: 0,
            section_table_offset: 0,
            tail_offset: 0,
        }
    }

    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_all(&self.magic)?;
        w.write_u16::<LittleEndian>(self.version_major)?;
        w.write_u16::<LittleEndian>(self.version_minor)?;
        w.write_u64::<LittleEndian>(self.flags)?;
        w.write_u64::<LittleEndian>(self.total_time_ps)?;
        w.write_u32::<LittleEndian>(self.num_segments)?;
        w.write_u32::<LittleEndian>(self.preamble_end)?;
        w.write_u64::<LittleEndian>(self.section_table_offset)?;
        w.write_u64::<LittleEndian>(self.tail_offset)?;
        Ok(())
    }

    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        let mut magic = [0u8; 4];
        r.read_exact(&mut magic)?;
        if magic != MAGIC {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid magic"));
        }
        Ok(Self {
            magic,
            version_major: r.read_u16::<LittleEndian>()?,
            version_minor: r.read_u16::<LittleEndian>()?,
            flags: r.read_u64::<LittleEndian>()?,
            total_time_ps: r.read_u64::<LittleEndian>()?,
            num_segments: r.read_u32::<LittleEndian>()?,
            preamble_end: r.read_u32::<LittleEndian>()?,
            section_table_offset: r.read_u64::<LittleEndian>()?,
            tail_offset: r.read_u64::<LittleEndian>()?,
        })
    }
}

impl Default for FileHeader {
    fn default() -> Self {
        Self::new()
    }
}

// ── Preamble Chunk ─────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct PreambleChunk {
    pub chunk_type: u16,
    pub flags: u16,
    pub payload: Vec<u8>,
}

impl PreambleChunk {
    pub fn new(chunk_type: u16, payload: Vec<u8>) -> Self {
        Self {
            chunk_type,
            flags: 0,
            payload,
        }
    }

    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_u16::<LittleEndian>(self.chunk_type)?;
        w.write_u16::<LittleEndian>(self.flags)?;
        w.write_u32::<LittleEndian>(self.payload.len() as u32)?;
        w.write_all(&self.payload)?;
        // Pad to 8-byte alignment
        let pad = (8 - (self.payload.len() % 8)) % 8;
        if pad > 0 {
            w.write_all(&vec![0u8; pad])?;
        }
        Ok(())
    }

    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        let chunk_type = r.read_u16::<LittleEndian>()?;
        let flags = r.read_u16::<LittleEndian>()?;
        let size = r.read_u32::<LittleEndian>()? as usize;
        let mut payload = vec![0u8; size];
        r.read_exact(&mut payload)?;
        // Skip padding
        let pad = (8 - (size % 8)) % 8;
        if pad > 0 {
            let mut skip = vec![0u8; pad];
            r.read_exact(&mut skip)?;
        }
        Ok(Self {
            chunk_type,
            flags,
            payload,
        })
    }
}

// ── Trace Config ───────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct TraceConfig {
    pub checkpoint_interval_ps: u64,
}

impl TraceConfig {
    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_u64::<LittleEndian>(self.checkpoint_interval_ps)?;
        Ok(())
    }

    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        Ok(Self {
            checkpoint_interval_ps: r.read_u64::<LittleEndian>()?,
        })
    }
}

// ── DUT Descriptor ─────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct DutProperty {
    pub key: u16,   // string pool offset
    pub value: u16, // string pool offset
}

#[derive(Debug, Clone)]
pub struct DutDesc {
    pub properties: Vec<DutProperty>,
}

impl DutDesc {
    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_u16::<LittleEndian>(self.properties.len() as u16)?;
        w.write_u16::<LittleEndian>(0)?; // reserved
        for p in &self.properties {
            w.write_u16::<LittleEndian>(p.key)?;
            w.write_u16::<LittleEndian>(p.value)?;
        }
        Ok(())
    }

    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        let num = r.read_u16::<LittleEndian>()? as usize;
        let _reserved = r.read_u16::<LittleEndian>()?;
        let mut properties = Vec::with_capacity(num);
        for _ in 0..num {
            properties.push(DutProperty {
                key: r.read_u16::<LittleEndian>()?,
                value: r.read_u16::<LittleEndian>()?,
            });
        }
        Ok(Self { properties })
    }
}

// ── Schema types ───────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SchemaHeader {
    pub num_enums: u8,
    pub num_clock_domains: u8,
    pub num_scopes: u16,
    pub num_storages: u16,
    pub num_event_types: u16,
    pub num_summary_fields: u16,
    pub string_pool_offset: u16,
}

impl SchemaHeader {
    pub const SIZE: usize = 14;

    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_u8(self.num_enums)?;
        w.write_u8(self.num_clock_domains)?;
        w.write_u16::<LittleEndian>(self.num_scopes)?;
        w.write_u16::<LittleEndian>(self.num_storages)?;
        w.write_u16::<LittleEndian>(self.num_event_types)?;
        w.write_u16::<LittleEndian>(self.num_summary_fields)?;
        w.write_u16::<LittleEndian>(self.string_pool_offset)?;
        Ok(())
    }

    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        Ok(Self {
            num_enums: r.read_u8()?,
            num_clock_domains: r.read_u8()?,
            num_scopes: r.read_u16::<LittleEndian>()?,
            num_storages: r.read_u16::<LittleEndian>()?,
            num_event_types: r.read_u16::<LittleEndian>()?,
            num_summary_fields: r.read_u16::<LittleEndian>()?,
            string_pool_offset: r.read_u16::<LittleEndian>()?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ClockDomainDef {
    pub name: u16,     // string pool offset
    pub clock_id: u16, // 0-based
    pub period_ps: u32,
}

impl ClockDomainDef {
    pub const SIZE: usize = 8;

    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_u16::<LittleEndian>(self.name)?;
        w.write_u16::<LittleEndian>(self.clock_id)?;
        w.write_u32::<LittleEndian>(self.period_ps)?;
        Ok(())
    }

    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        Ok(Self {
            name: r.read_u16::<LittleEndian>()?,
            clock_id: r.read_u16::<LittleEndian>()?,
            period_ps: r.read_u32::<LittleEndian>()?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ScopeDef {
    pub name: u16,
    pub scope_id: u16,
    pub parent_id: u16,  // 0xFFFF = root
    pub protocol: u16,   // string pool offset, 0xFFFF = no protocol
    pub clock_id: u8,    // 0xFF = inherit
    pub reserved: [u8; 3],
}

impl ScopeDef {
    pub const SIZE: usize = 12;
    pub const NO_PARENT: u16 = 0xFFFF;
    pub const NO_PROTOCOL: u16 = 0xFFFF;
    pub const INHERIT_CLOCK: u8 = 0xFF;

    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_u16::<LittleEndian>(self.name)?;
        w.write_u16::<LittleEndian>(self.scope_id)?;
        w.write_u16::<LittleEndian>(self.parent_id)?;
        w.write_u16::<LittleEndian>(self.protocol)?;
        w.write_u8(self.clock_id)?;
        w.write_all(&self.reserved)?;
        Ok(())
    }

    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        let name = r.read_u16::<LittleEndian>()?;
        let scope_id = r.read_u16::<LittleEndian>()?;
        let parent_id = r.read_u16::<LittleEndian>()?;
        let protocol = r.read_u16::<LittleEndian>()?;
        let clock_id = r.read_u8()?;
        let mut reserved = [0u8; 3];
        r.read_exact(&mut reserved)?;
        Ok(Self {
            name,
            scope_id,
            parent_id,
            protocol,
            clock_id,
            reserved,
        })
    }
}

#[derive(Debug, Clone)]
pub struct EnumValue {
    pub value: u8,
    pub reserved: u8,
    pub name: u16, // string pool offset
}

impl EnumValue {
    pub const SIZE: usize = 4;

    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_u8(self.value)?;
        w.write_u8(self.reserved)?;
        w.write_u16::<LittleEndian>(self.name)?;
        Ok(())
    }

    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        Ok(Self {
            value: r.read_u8()?,
            reserved: r.read_u8()?,
            name: r.read_u16::<LittleEndian>()?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct EnumDef {
    pub name: u16, // string pool offset
    pub num_values: u8,
    pub reserved: u8,
    pub values: Vec<EnumValue>,
}

impl EnumDef {
    pub const HEADER_SIZE: usize = 4;

    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_u16::<LittleEndian>(self.name)?;
        w.write_u8(self.num_values)?;
        w.write_u8(self.reserved)?;
        for v in &self.values {
            v.write_to(w)?;
        }
        Ok(())
    }

    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        let name = r.read_u16::<LittleEndian>()?;
        let num_values = r.read_u8()?;
        let reserved = r.read_u8()?;
        let mut values = Vec::with_capacity(num_values as usize);
        for _ in 0..num_values {
            values.push(EnumValue::read_from(r)?);
        }
        Ok(Self {
            name,
            num_values,
            reserved,
            values,
        })
    }
}

#[derive(Debug, Clone)]
pub struct FieldDef {
    pub name: u16,    // string pool offset
    pub field_type: u8,
    pub enum_id: u8,  // if type==FT_ENUM, else 0
    pub reserved: [u8; 4],
}

impl FieldDef {
    pub const SIZE: usize = 8;

    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_u16::<LittleEndian>(self.name)?;
        w.write_u8(self.field_type)?;
        w.write_u8(self.enum_id)?;
        w.write_all(&self.reserved)?;
        Ok(())
    }

    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        let name = r.read_u16::<LittleEndian>()?;
        let field_type = r.read_u8()?;
        let enum_id = r.read_u8()?;
        let mut reserved = [0u8; 4];
        r.read_exact(&mut reserved)?;
        Ok(Self {
            name,
            field_type,
            enum_id,
            reserved,
        })
    }
}

#[derive(Debug, Clone)]
pub struct StorageDef {
    pub name: u16,
    pub storage_id: u16,
    pub num_slots: u16,
    pub num_fields: u16,
    pub flags: u16,
    pub scope_id: u16, // 0xFFFF = root
    pub fields: Vec<FieldDef>,
}

impl StorageDef {
    pub const HEADER_SIZE: usize = 12;

    pub fn is_sparse(&self) -> bool {
        self.flags & SF_SPARSE != 0
    }

    /// Size of one slot in bytes (sum of field sizes).
    pub fn slot_size(&self) -> usize {
        self.fields
            .iter()
            .map(|f| FieldType::from_u8(f.field_type).map_or(0, |t| t.size()))
            .sum()
    }

    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_u16::<LittleEndian>(self.name)?;
        w.write_u16::<LittleEndian>(self.storage_id)?;
        w.write_u16::<LittleEndian>(self.num_slots)?;
        w.write_u16::<LittleEndian>(self.num_fields)?;
        w.write_u16::<LittleEndian>(self.flags)?;
        w.write_u16::<LittleEndian>(self.scope_id)?;
        for f in &self.fields {
            f.write_to(w)?;
        }
        Ok(())
    }

    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        let name = r.read_u16::<LittleEndian>()?;
        let storage_id = r.read_u16::<LittleEndian>()?;
        let num_slots = r.read_u16::<LittleEndian>()?;
        let num_fields = r.read_u16::<LittleEndian>()?;
        let flags = r.read_u16::<LittleEndian>()?;
        let scope_id = r.read_u16::<LittleEndian>()?;
        let mut fields = Vec::with_capacity(num_fields as usize);
        for _ in 0..num_fields {
            fields.push(FieldDef::read_from(r)?);
        }
        Ok(Self {
            name,
            storage_id,
            num_slots,
            num_fields,
            flags,
            scope_id,
            fields,
        })
    }
}

#[derive(Debug, Clone)]
pub struct EventDef {
    pub name: u16,
    pub event_type_id: u16,
    pub num_fields: u16,
    pub scope_id: u16,
    pub fields: Vec<FieldDef>,
}

impl EventDef {
    pub const HEADER_SIZE: usize = 8;

    /// Total payload size in bytes (sum of field sizes).
    pub fn payload_size(&self) -> usize {
        self.fields
            .iter()
            .map(|f| FieldType::from_u8(f.field_type).map_or(0, |t| t.size()))
            .sum()
    }

    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_u16::<LittleEndian>(self.name)?;
        w.write_u16::<LittleEndian>(self.event_type_id)?;
        w.write_u16::<LittleEndian>(self.num_fields)?;
        w.write_u16::<LittleEndian>(self.scope_id)?;
        for f in &self.fields {
            f.write_to(w)?;
        }
        Ok(())
    }

    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        let name = r.read_u16::<LittleEndian>()?;
        let event_type_id = r.read_u16::<LittleEndian>()?;
        let num_fields = r.read_u16::<LittleEndian>()?;
        let scope_id = r.read_u16::<LittleEndian>()?;
        let mut fields = Vec::with_capacity(num_fields as usize);
        for _ in 0..num_fields {
            fields.push(FieldDef::read_from(r)?);
        }
        Ok(Self {
            name,
            event_type_id,
            num_fields,
            scope_id,
            fields,
        })
    }
}

#[derive(Debug, Clone)]
pub struct SummaryFieldDef {
    pub name: u16,
    pub field_type: u8,
    pub reserved: u8,
    pub scope_id: u16,
    pub reserved2: u16,
}

impl SummaryFieldDef {
    pub const SIZE: usize = 8;

    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_u16::<LittleEndian>(self.name)?;
        w.write_u8(self.field_type)?;
        w.write_u8(self.reserved)?;
        w.write_u16::<LittleEndian>(self.scope_id)?;
        w.write_u16::<LittleEndian>(self.reserved2)?;
        Ok(())
    }

    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        Ok(Self {
            name: r.read_u16::<LittleEndian>()?,
            field_type: r.read_u8()?,
            reserved: r.read_u8()?,
            scope_id: r.read_u16::<LittleEndian>()?,
            reserved2: r.read_u16::<LittleEndian>()?,
        })
    }
}

// ── Segment types ──────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SegmentHeader {
    pub segment_magic: [u8; 4],
    pub flags: u32,
    pub time_start_ps: u64,
    pub time_end_ps: u64,
    pub prev_segment_offset: u64,
    pub checkpoint_size: u32,
    pub deltas_compressed_size: u32,
    pub deltas_raw_size: u32,
    pub num_frames: u32,
    pub num_frames_active: u32,
    pub reserved: u32,
}

impl SegmentHeader {
    pub const SIZE: usize = 56;

    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_all(&self.segment_magic)?;
        w.write_u32::<LittleEndian>(self.flags)?;
        w.write_u64::<LittleEndian>(self.time_start_ps)?;
        w.write_u64::<LittleEndian>(self.time_end_ps)?;
        w.write_u64::<LittleEndian>(self.prev_segment_offset)?;
        w.write_u32::<LittleEndian>(self.checkpoint_size)?;
        w.write_u32::<LittleEndian>(self.deltas_compressed_size)?;
        w.write_u32::<LittleEndian>(self.deltas_raw_size)?;
        w.write_u32::<LittleEndian>(self.num_frames)?;
        w.write_u32::<LittleEndian>(self.num_frames_active)?;
        w.write_u32::<LittleEndian>(self.reserved)?;
        Ok(())
    }

    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        let mut magic = [0u8; 4];
        r.read_exact(&mut magic)?;
        if magic != SEG_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid segment magic",
            ));
        }
        Ok(Self {
            segment_magic: magic,
            flags: r.read_u32::<LittleEndian>()?,
            time_start_ps: r.read_u64::<LittleEndian>()?,
            time_end_ps: r.read_u64::<LittleEndian>()?,
            prev_segment_offset: r.read_u64::<LittleEndian>()?,
            checkpoint_size: r.read_u32::<LittleEndian>()?,
            deltas_compressed_size: r.read_u32::<LittleEndian>()?,
            deltas_raw_size: r.read_u32::<LittleEndian>()?,
            num_frames: r.read_u32::<LittleEndian>()?,
            num_frames_active: r.read_u32::<LittleEndian>()?,
            reserved: r.read_u32::<LittleEndian>()?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct CheckpointBlock {
    pub storage_id: u16,
    pub reserved: u16,
    pub size: u32,
}

impl CheckpointBlock {
    pub const HEADER_SIZE: usize = 8;

    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_u16::<LittleEndian>(self.storage_id)?;
        w.write_u16::<LittleEndian>(self.reserved)?;
        w.write_u32::<LittleEndian>(self.size)?;
        Ok(())
    }

    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        Ok(Self {
            storage_id: r.read_u16::<LittleEndian>()?,
            reserved: r.read_u16::<LittleEndian>()?,
            size: r.read_u32::<LittleEndian>()?,
        })
    }
}

// ── Delta ops ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct DeltaOp {
    pub action: u8,
    pub reserved: u8,
    pub storage_id: u16,
    pub slot_index: u16,
    pub field_index: u16,
    pub value: u64,
}

impl DeltaOp {
    pub const SIZE: usize = 16;

    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_u8(self.action)?;
        w.write_u8(self.reserved)?;
        w.write_u16::<LittleEndian>(self.storage_id)?;
        w.write_u16::<LittleEndian>(self.slot_index)?;
        w.write_u16::<LittleEndian>(self.field_index)?;
        w.write_u64::<LittleEndian>(self.value)?;
        Ok(())
    }

    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        Ok(Self {
            action: r.read_u8()?,
            reserved: r.read_u8()?,
            storage_id: r.read_u16::<LittleEndian>()?,
            slot_index: r.read_u16::<LittleEndian>()?,
            field_index: r.read_u16::<LittleEndian>()?,
            value: r.read_u64::<LittleEndian>()?,
        })
    }

    /// Try to represent this op as a compact delta op.
    /// Returns None if storage_id > 255 or value > 65535.
    pub fn to_compact(&self) -> Option<DeltaOpCompact> {
        if self.storage_id > 255 || self.value > 65535 {
            return None;
        }
        Some(DeltaOpCompact {
            action: self.action,
            storage_id_lo: self.storage_id as u8,
            slot_index: self.slot_index,
            field_index: self.field_index,
            value16: self.value as u16,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DeltaOpCompact {
    pub action: u8,
    pub storage_id_lo: u8,
    pub slot_index: u16,
    pub field_index: u16,
    pub value16: u16,
}

impl DeltaOpCompact {
    pub const SIZE: usize = 8;

    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_u8(self.action)?;
        w.write_u8(self.storage_id_lo)?;
        w.write_u16::<LittleEndian>(self.slot_index)?;
        w.write_u16::<LittleEndian>(self.field_index)?;
        w.write_u16::<LittleEndian>(self.value16)?;
        Ok(())
    }

    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        Ok(Self {
            action: r.read_u8()?,
            storage_id_lo: r.read_u8()?,
            slot_index: r.read_u16::<LittleEndian>()?,
            field_index: r.read_u16::<LittleEndian>()?,
            value16: r.read_u16::<LittleEndian>()?,
        })
    }

    pub fn to_wide(&self) -> DeltaOp {
        DeltaOp {
            action: self.action,
            reserved: 0,
            storage_id: self.storage_id_lo as u16,
            slot_index: self.slot_index,
            field_index: self.field_index,
            value: self.value16 as u64,
        }
    }
}

// ── Event record ───────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct EventRecord {
    pub event_type_id: u16,
    pub reserved: u16,
    pub payload_size: u32,
    pub payload: Vec<u8>,
}

impl EventRecord {
    pub const HEADER_SIZE: usize = 8;

    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_u16::<LittleEndian>(self.event_type_id)?;
        w.write_u16::<LittleEndian>(self.reserved)?;
        w.write_u32::<LittleEndian>(self.payload_size)?;
        w.write_all(&self.payload)?;
        Ok(())
    }

    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        let event_type_id = r.read_u16::<LittleEndian>()?;
        let reserved = r.read_u16::<LittleEndian>()?;
        let payload_size = r.read_u32::<LittleEndian>()?;
        let mut payload = vec![0u8; payload_size as usize];
        r.read_exact(&mut payload)?;
        Ok(Self {
            event_type_id,
            reserved,
            payload_size,
            payload,
        })
    }
}

// ── Section table ──────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SectionEntry {
    pub section_type: u16,
    pub flags: u16,
    pub reserved: u32,
    pub offset: u64,
    pub size: u64,
}

impl SectionEntry {
    pub const SIZE: usize = 24;

    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_u16::<LittleEndian>(self.section_type)?;
        w.write_u16::<LittleEndian>(self.flags)?;
        w.write_u32::<LittleEndian>(self.reserved)?;
        w.write_u64::<LittleEndian>(self.offset)?;
        w.write_u64::<LittleEndian>(self.size)?;
        Ok(())
    }

    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        Ok(Self {
            section_type: r.read_u16::<LittleEndian>()?,
            flags: r.read_u16::<LittleEndian>()?,
            reserved: r.read_u32::<LittleEndian>()?,
            offset: r.read_u64::<LittleEndian>()?,
            size: r.read_u64::<LittleEndian>()?,
        })
    }
}

// ── Segment index (in segment table section) ───────────────────────

#[derive(Debug, Clone)]
pub struct SegmentIndexEntry {
    pub offset: u64,
    pub time_start_ps: u64,
    pub time_end_ps: u64,
}

impl SegmentIndexEntry {
    pub const SIZE: usize = 24;

    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_u64::<LittleEndian>(self.offset)?;
        w.write_u64::<LittleEndian>(self.time_start_ps)?;
        w.write_u64::<LittleEndian>(self.time_end_ps)?;
        Ok(())
    }

    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        Ok(Self {
            offset: r.read_u64::<LittleEndian>()?,
            time_start_ps: r.read_u64::<LittleEndian>()?,
            time_end_ps: r.read_u64::<LittleEndian>()?,
        })
    }
}

// ── String table ───────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct StringTableHeader {
    pub num_entries: u32,
    pub reserved: u32,
}

impl StringTableHeader {
    pub const SIZE: usize = 8;

    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_u32::<LittleEndian>(self.num_entries)?;
        w.write_u32::<LittleEndian>(self.reserved)?;
        Ok(())
    }

    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        Ok(Self {
            num_entries: r.read_u32::<LittleEndian>()?,
            reserved: r.read_u32::<LittleEndian>()?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct StringIndex {
    pub offset: u32,
    pub length: u32,
}

impl StringIndex {
    pub const SIZE: usize = 8;

    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_u32::<LittleEndian>(self.offset)?;
        w.write_u32::<LittleEndian>(self.length)?;
        Ok(())
    }

    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        Ok(Self {
            offset: r.read_u32::<LittleEndian>()?,
            length: r.read_u32::<LittleEndian>()?,
        })
    }
}

// ── Summary section ────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SummaryHeader {
    pub num_levels: u32,
    pub fan_out: u32,
    pub entry_size: u32,
    pub reserved: u32,
    pub base_interval_ps: u64,
}

impl SummaryHeader {
    pub const SIZE: usize = 24;

    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_u32::<LittleEndian>(self.num_levels)?;
        w.write_u32::<LittleEndian>(self.fan_out)?;
        w.write_u32::<LittleEndian>(self.entry_size)?;
        w.write_u32::<LittleEndian>(self.reserved)?;
        w.write_u64::<LittleEndian>(self.base_interval_ps)?;
        Ok(())
    }

    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        Ok(Self {
            num_levels: r.read_u32::<LittleEndian>()?,
            fan_out: r.read_u32::<LittleEndian>()?,
            entry_size: r.read_u32::<LittleEndian>()?,
            reserved: r.read_u32::<LittleEndian>()?,
            base_interval_ps: r.read_u64::<LittleEndian>()?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct LevelDesc {
    pub offset: u64,
    pub num_entries: u32,
    pub reserved: u32,
}

impl LevelDesc {
    pub const SIZE: usize = 16;

    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_u64::<LittleEndian>(self.offset)?;
        w.write_u32::<LittleEndian>(self.num_entries)?;
        w.write_u32::<LittleEndian>(self.reserved)?;
        Ok(())
    }

    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        Ok(Self {
            offset: r.read_u64::<LittleEndian>()?,
            num_entries: r.read_u32::<LittleEndian>()?,
            reserved: r.read_u32::<LittleEndian>()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn file_header_roundtrip() {
        let hdr = FileHeader::new();
        let mut buf = Vec::new();
        hdr.write_to(&mut buf).unwrap();
        assert_eq!(buf.len(), FileHeader::SIZE);

        let decoded = FileHeader::read_from(&mut Cursor::new(&buf)).unwrap();
        assert_eq!(decoded.magic, MAGIC);
        assert_eq!(decoded.version_major, VERSION_MAJOR);
        assert_eq!(decoded.version_minor, VERSION_MINOR);
        assert_eq!(decoded.flags, hdr.flags);
    }

    #[test]
    fn preamble_chunk_roundtrip() {
        let chunk = PreambleChunk::new(CHUNK_TRACE_CONFIG, vec![1, 2, 3, 4, 5]);
        let mut buf = Vec::new();
        chunk.write_to(&mut buf).unwrap();
        // 8 header + 5 payload + 3 padding = 16
        assert_eq!(buf.len(), 16);

        let decoded = PreambleChunk::read_from(&mut Cursor::new(&buf)).unwrap();
        assert_eq!(decoded.chunk_type, CHUNK_TRACE_CONFIG);
        assert_eq!(decoded.payload, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn segment_header_roundtrip() {
        let seg = SegmentHeader {
            segment_magic: SEG_MAGIC,
            flags: 0,
            time_start_ps: 1000,
            time_end_ps: 2000,
            prev_segment_offset: 0,
            checkpoint_size: 128,
            deltas_compressed_size: 64,
            deltas_raw_size: 100,
            num_frames: 10,
            num_frames_active: 5,
            reserved: 0,
        };
        let mut buf = Vec::new();
        seg.write_to(&mut buf).unwrap();
        assert_eq!(buf.len(), SegmentHeader::SIZE);

        let decoded = SegmentHeader::read_from(&mut Cursor::new(&buf)).unwrap();
        assert_eq!(decoded.time_start_ps, 1000);
        assert_eq!(decoded.time_end_ps, 2000);
        assert_eq!(decoded.num_frames, 10);
    }

    #[test]
    fn delta_op_roundtrip() {
        let op = DeltaOp {
            action: DA_SLOT_SET,
            reserved: 0,
            storage_id: 1,
            slot_index: 42,
            field_index: 2,
            value: 0xDEAD_BEEF,
        };
        let mut buf = Vec::new();
        op.write_to(&mut buf).unwrap();
        assert_eq!(buf.len(), DeltaOp::SIZE);

        let decoded = DeltaOp::read_from(&mut Cursor::new(&buf)).unwrap();
        assert_eq!(decoded.action, DA_SLOT_SET);
        assert_eq!(decoded.storage_id, 1);
        assert_eq!(decoded.slot_index, 42);
        assert_eq!(decoded.value, 0xDEAD_BEEF);
    }

    #[test]
    fn compact_conversion() {
        let op = DeltaOp {
            action: DA_SLOT_SET,
            reserved: 0,
            storage_id: 5,
            slot_index: 100,
            field_index: 3,
            value: 42,
        };
        let compact = op.to_compact().unwrap();
        assert_eq!(compact.storage_id_lo, 5);
        assert_eq!(compact.value16, 42);

        let wide = compact.to_wide();
        assert_eq!(wide.storage_id, 5);
        assert_eq!(wide.value, 42);

        // Cannot compact large storage_id or value
        let big = DeltaOp { storage_id: 300, ..op };
        assert!(big.to_compact().is_none());

        let big_val = DeltaOp { value: 70000, ..op };
        assert!(big_val.to_compact().is_none());
    }

    #[test]
    fn field_type_sizes() {
        assert_eq!(FieldType::U8.size(), 1);
        assert_eq!(FieldType::U16.size(), 2);
        assert_eq!(FieldType::U32.size(), 4);
        assert_eq!(FieldType::U64.size(), 8);
        assert_eq!(FieldType::Bool.size(), 1);
        assert_eq!(FieldType::Enum.size(), 1);
        assert_eq!(FieldType::StringRef.size(), 4);
    }

    #[test]
    fn section_entry_roundtrip() {
        let entry = SectionEntry {
            section_type: SECTION_SEGMENTS,
            flags: 0,
            reserved: 0,
            offset: 4096,
            size: 1024,
        };
        let mut buf = Vec::new();
        entry.write_to(&mut buf).unwrap();
        assert_eq!(buf.len(), SectionEntry::SIZE);

        let decoded = SectionEntry::read_from(&mut Cursor::new(&buf)).unwrap();
        assert_eq!(decoded.section_type, SECTION_SEGMENTS);
        assert_eq!(decoded.offset, 4096);
        assert_eq!(decoded.size, 1024);
    }
}
