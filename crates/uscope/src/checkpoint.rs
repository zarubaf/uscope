/// Checkpoint serialization and deserialization.
/// A checkpoint captures the full state of all storages at a point in time.
use crate::types::*;
// byteorder used for CheckpointBlock read/write (via types.rs)
use std::io::{self, Read, Write};

/// In-memory representation of a single storage's slot state.
#[derive(Debug, Clone)]
pub struct StorageState {
    pub storage_id: u16,
    pub num_slots: u16,
    pub is_sparse: bool,
    pub slot_size: usize,
    /// Per-slot validity (only meaningful for sparse storages).
    pub valid: Vec<bool>,
    /// Flat array: slot_data[slot_index * slot_size .. (slot_index+1) * slot_size].
    pub data: Vec<u8>,
    /// Storage-level property values (tightly packed, v0.3).
    pub property_data: Vec<u8>,
    /// Total size of property data in bytes.
    pub property_size: usize,
}

impl StorageState {
    pub fn new(storage_def: &StorageDef) -> Self {
        let slot_size = storage_def.slot_size();
        let num_slots = storage_def.num_slots as usize;
        let property_size = storage_def.property_data_size();
        Self {
            storage_id: storage_def.storage_id,
            num_slots: storage_def.num_slots,
            is_sparse: storage_def.is_sparse(),
            slot_size,
            valid: vec![false; num_slots],
            data: vec![0u8; num_slots * slot_size],
            property_data: vec![0u8; property_size],
            property_size,
        }
    }

    /// Set a field value in a slot.
    pub fn set_field(&mut self, slot: u16, field_index: u16, value: u64, field_type: FieldType) {
        let slot = slot as usize;
        if slot >= self.num_slots as usize {
            return;
        }
        self.valid[slot] = true;
        let offset = self.field_offset(field_index, field_type);
        let base = slot * self.slot_size + offset;
        let data = &mut self.data[base..];
        match field_type {
            FieldType::U8 | FieldType::I8 | FieldType::Bool | FieldType::Enum => {
                data[0] = value as u8;
            }
            FieldType::U16 | FieldType::I16 => {
                let v = value as u16;
                data[..2].copy_from_slice(&v.to_le_bytes());
            }
            FieldType::U32 | FieldType::I32 | FieldType::StringRef => {
                let v = value as u32;
                data[..4].copy_from_slice(&v.to_le_bytes());
            }
            FieldType::U64 | FieldType::I64 => {
                data[..8].copy_from_slice(&value.to_le_bytes());
            }
        }
    }

    /// Add a value to a field in a slot.
    pub fn add_field(&mut self, slot: u16, field_index: u16, value: u64, field_type: FieldType) {
        let current = self.get_field(slot, field_index, field_type);
        self.set_field(slot, field_index, current.wrapping_add(value), field_type);
    }

    /// Get a field value from a slot.
    pub fn get_field(&self, slot: u16, field_index: u16, field_type: FieldType) -> u64 {
        let slot = slot as usize;
        if slot >= self.num_slots as usize {
            return 0;
        }
        let offset = self.field_offset(field_index, field_type);
        let base = slot * self.slot_size + offset;
        let data = &self.data[base..];
        match field_type {
            FieldType::U8 | FieldType::I8 | FieldType::Bool | FieldType::Enum => data[0] as u64,
            FieldType::U16 | FieldType::I16 => u16::from_le_bytes([data[0], data[1]]) as u64,
            FieldType::U32 | FieldType::I32 | FieldType::StringRef => {
                u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as u64
            }
            FieldType::U64 | FieldType::I64 => u64::from_le_bytes([
                data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
            ]),
        }
    }

    /// Clear a slot (mark invalid).
    pub fn clear_slot(&mut self, slot: u16) {
        let slot = slot as usize;
        if slot >= self.num_slots as usize {
            return;
        }
        self.valid[slot] = false;
        let base = slot * self.slot_size;
        self.data[base..base + self.slot_size].fill(0);
    }

    /// Check if a slot is valid.
    pub fn slot_valid(&self, slot: u16) -> bool {
        (slot as usize) < self.valid.len() && self.valid[slot as usize]
    }

    fn field_offset(&self, field_index: u16, _field_type: FieldType) -> usize {
        // Fields are packed in order; we need to compute offset by summing
        // preceding field sizes. For efficiency, this is computed at call time
        // using the field_type parameter. In practice, the caller would use
        // precomputed offsets. For now, this is a simplified approach where
        // we assume uniform layout.
        // TODO: use precomputed field offset table from StorageDef
        _ = field_index;
        0
    }

    /// Serialize this storage's checkpoint.
    pub fn write_checkpoint<W: Write>(
        &self,
        w: &mut W,
        field_types: &[FieldType],
    ) -> io::Result<()> {
        let mut payload = Vec::new();

        if self.is_sparse {
            // Write valid_mask
            let mask_bytes = (self.num_slots as usize + 7) / 8;
            let mut mask = vec![0u8; mask_bytes];
            for (i, &v) in self.valid.iter().enumerate() {
                if v {
                    mask[i / 8] |= 1 << (i % 8);
                }
            }
            payload.extend_from_slice(&mask);

            // Write data for valid slots only
            for (i, &v) in self.valid.iter().enumerate() {
                if v {
                    let base = i * self.slot_size;
                    payload.extend_from_slice(&self.data[base..base + self.slot_size]);
                }
            }
        } else {
            // Dense: write all slots
            payload.extend_from_slice(&self.data[..self.num_slots as usize * self.slot_size]);
        }

        // v0.3: append property data after slot data
        if self.property_size > 0 {
            payload.extend_from_slice(&self.property_data[..self.property_size]);
        }

        let block = CheckpointBlock {
            storage_id: self.storage_id,
            reserved: 0,
            size: payload.len() as u32,
        };
        block.write_to(w)?;
        w.write_all(&payload)?;
        _ = field_types; // used by callers for computing field offsets
        Ok(())
    }

    /// Deserialize this storage's checkpoint.
    pub fn read_checkpoint<R: Read>(&mut self, r: &mut R, size: u32) -> io::Result<()> {
        let mut data = vec![0u8; size as usize];
        r.read_exact(&mut data)?;

        let slot_data_end;
        if self.is_sparse {
            let mask_bytes = (self.num_slots as usize + 7) / 8;
            if data.len() < mask_bytes {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "checkpoint too small for valid mask",
                ));
            }
            let mask = &data[..mask_bytes];
            let mut pos = mask_bytes;

            self.valid.fill(false);
            self.data.fill(0);

            for i in 0..self.num_slots as usize {
                let is_valid = (mask[i / 8] >> (i % 8)) & 1 != 0;
                self.valid[i] = is_valid;
                if is_valid {
                    if data.len() - pos < self.slot_size {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "checkpoint truncated",
                        ));
                    }
                    let base = i * self.slot_size;
                    self.data[base..base + self.slot_size]
                        .copy_from_slice(&data[pos..pos + self.slot_size]);
                    pos += self.slot_size;
                }
            }
            slot_data_end = pos;
        } else {
            let expected = self.num_slots as usize * self.slot_size;
            if data.len() < expected {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "checkpoint too small for dense storage",
                ));
            }
            self.data[..expected].copy_from_slice(&data[..expected]);
            self.valid.fill(true);
            slot_data_end = expected;
        }

        // v0.3: read property data from trailing bytes
        if self.property_size > 0 {
            let remaining = data.len() - slot_data_end;
            if remaining >= self.property_size {
                self.property_data[..self.property_size]
                    .copy_from_slice(&data[slot_data_end..slot_data_end + self.property_size]);
            }
            // If not enough trailing bytes, properties stay zero-initialized (forward compat).
        }
        Ok(())
    }
}

/// Precomputed field offset table for a storage.
#[derive(Debug, Clone)]
pub struct FieldOffsets {
    /// Byte offset of each field within a slot.
    pub offsets: Vec<usize>,
    /// Type of each field.
    pub types: Vec<FieldType>,
    /// Byte offset of each property in the property data block (v0.3).
    pub prop_offsets: Vec<usize>,
    /// Type of each property (v0.3).
    pub prop_types: Vec<FieldType>,
    /// Total size of all properties in bytes (v0.3).
    pub prop_size: usize,
}

impl FieldOffsets {
    pub fn from_storage_def(storage_def: &StorageDef) -> Self {
        let mut offsets = Vec::with_capacity(storage_def.fields.len());
        let mut types = Vec::with_capacity(storage_def.fields.len());
        let mut offset = 0;
        for f in &storage_def.fields {
            offsets.push(offset);
            let ft = FieldType::from_u8(f.field_type).unwrap_or(FieldType::U8);
            types.push(ft);
            offset += ft.size();
        }

        let mut prop_offsets = Vec::with_capacity(storage_def.properties.len());
        let mut prop_types = Vec::with_capacity(storage_def.properties.len());
        let mut prop_offset = 0;
        for f in &storage_def.properties {
            prop_offsets.push(prop_offset);
            let ft = FieldType::from_u8(f.field_type).unwrap_or(FieldType::U8);
            prop_types.push(ft);
            prop_offset += ft.size();
        }

        Self {
            offsets,
            types,
            prop_offsets,
            prop_types,
            prop_size: prop_offset,
        }
    }

    pub fn field_type(&self, field_index: u16) -> FieldType {
        self.types[field_index as usize]
    }

    pub fn field_offset(&self, field_index: u16) -> usize {
        self.offsets[field_index as usize]
    }
}

/// Extended StorageState that uses precomputed field offsets.
impl StorageState {
    /// Set a field using precomputed offsets.
    pub fn set_field_at(
        &mut self,
        slot: u16,
        offsets: &FieldOffsets,
        field_index: u16,
        value: u64,
    ) {
        let slot_idx = slot as usize;
        if slot_idx >= self.num_slots as usize {
            return;
        }
        self.valid[slot_idx] = true;
        let fi = field_index as usize;
        let ft = offsets.types[fi];
        let base = slot_idx * self.slot_size + offsets.offsets[fi];
        let data = &mut self.data[base..];
        match ft {
            FieldType::U8 | FieldType::I8 | FieldType::Bool | FieldType::Enum => {
                data[0] = value as u8;
            }
            FieldType::U16 | FieldType::I16 => {
                data[..2].copy_from_slice(&(value as u16).to_le_bytes());
            }
            FieldType::U32 | FieldType::I32 | FieldType::StringRef => {
                data[..4].copy_from_slice(&(value as u32).to_le_bytes());
            }
            FieldType::U64 | FieldType::I64 => {
                data[..8].copy_from_slice(&value.to_le_bytes());
            }
        }
    }

    /// Add a value using precomputed offsets.
    pub fn add_field_at(
        &mut self,
        slot: u16,
        offsets: &FieldOffsets,
        field_index: u16,
        value: u64,
    ) {
        let current = self.get_field_at(slot, offsets, field_index);
        self.set_field_at(slot, offsets, field_index, current.wrapping_add(value));
    }

    /// Set a storage-level property value (v0.3).
    pub fn set_property(&mut self, offsets: &FieldOffsets, prop_index: u16, value: u64) {
        let pi = prop_index as usize;
        if pi >= offsets.prop_types.len() {
            return;
        }
        let ft = offsets.prop_types[pi];
        let base = offsets.prop_offsets[pi];
        let data = &mut self.property_data[base..];
        match ft {
            FieldType::U8 | FieldType::I8 | FieldType::Bool | FieldType::Enum => {
                data[0] = value as u8;
            }
            FieldType::U16 | FieldType::I16 => {
                data[..2].copy_from_slice(&(value as u16).to_le_bytes());
            }
            FieldType::U32 | FieldType::I32 | FieldType::StringRef => {
                data[..4].copy_from_slice(&(value as u32).to_le_bytes());
            }
            FieldType::U64 | FieldType::I64 => {
                data[..8].copy_from_slice(&value.to_le_bytes());
            }
        }
    }

    /// Get a storage-level property value (v0.3).
    pub fn get_property(&self, offsets: &FieldOffsets, prop_index: u16) -> u64 {
        let pi = prop_index as usize;
        if pi >= offsets.prop_types.len() {
            return 0;
        }
        let ft = offsets.prop_types[pi];
        let base = offsets.prop_offsets[pi];
        let data = &self.property_data[base..];
        match ft {
            FieldType::U8 | FieldType::I8 | FieldType::Bool | FieldType::Enum => data[0] as u64,
            FieldType::U16 | FieldType::I16 => u16::from_le_bytes([data[0], data[1]]) as u64,
            FieldType::U32 | FieldType::I32 | FieldType::StringRef => {
                u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as u64
            }
            FieldType::U64 | FieldType::I64 => u64::from_le_bytes([
                data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
            ]),
        }
    }

    /// Get a field value using precomputed offsets.
    pub fn get_field_at(&self, slot: u16, offsets: &FieldOffsets, field_index: u16) -> u64 {
        let slot_idx = slot as usize;
        if slot_idx >= self.num_slots as usize {
            return 0;
        }
        let fi = field_index as usize;
        let ft = offsets.types[fi];
        let base = slot_idx * self.slot_size + offsets.offsets[fi];
        let data = &self.data[base..];
        match ft {
            FieldType::U8 | FieldType::I8 | FieldType::Bool | FieldType::Enum => data[0] as u64,
            FieldType::U16 | FieldType::I16 => u16::from_le_bytes([data[0], data[1]]) as u64,
            FieldType::U32 | FieldType::I32 | FieldType::StringRef => {
                u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as u64
            }
            FieldType::U64 | FieldType::I64 => u64::from_le_bytes([
                data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
            ]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_storage() -> StorageDef {
        StorageDef {
            name: 0,
            storage_id: 0,
            num_slots: 4,
            num_fields: 2,
            flags: SF_SPARSE,
            scope_id: 0,
            num_properties: 0,
            reserved_v3: 0,
            fields: vec![
                FieldDef {
                    name: 0,
                    field_type: FieldType::U32 as u8,
                    enum_id: 0,
                    role: 0,
                    pair_id: 0,
                    reserved: [0; 2],
                },
                FieldDef {
                    name: 0,
                    field_type: FieldType::U64 as u8,
                    enum_id: 0,
                    role: 0,
                    pair_id: 0,
                    reserved: [0; 2],
                },
            ],
            properties: vec![],
        }
    }

    #[test]
    fn storage_state_set_get() {
        let def = make_test_storage();
        let offsets = FieldOffsets::from_storage_def(&def);
        let mut state = StorageState::new(&def);

        assert!(!state.slot_valid(0));

        state.set_field_at(0, &offsets, 0, 42);
        assert!(state.slot_valid(0));
        assert_eq!(state.get_field_at(0, &offsets, 0), 42);

        state.set_field_at(0, &offsets, 1, 0xDEAD_BEEF_CAFE);
        assert_eq!(state.get_field_at(0, &offsets, 1), 0xDEAD_BEEF_CAFE);

        state.clear_slot(0);
        assert!(!state.slot_valid(0));
        assert_eq!(state.get_field_at(0, &offsets, 0), 0);
    }

    #[test]
    fn storage_state_add() {
        let def = make_test_storage();
        let offsets = FieldOffsets::from_storage_def(&def);
        let mut state = StorageState::new(&def);

        state.set_field_at(1, &offsets, 1, 100);
        state.add_field_at(1, &offsets, 1, 50);
        assert_eq!(state.get_field_at(1, &offsets, 1), 150);
    }

    #[test]
    fn checkpoint_roundtrip_sparse() {
        let def = make_test_storage();
        let offsets = FieldOffsets::from_storage_def(&def);
        let mut state = StorageState::new(&def);

        state.set_field_at(1, &offsets, 0, 42);
        state.set_field_at(1, &offsets, 1, 0x1234);
        state.set_field_at(3, &offsets, 0, 99);

        let ft: Vec<FieldType> = def
            .fields
            .iter()
            .map(|f| FieldType::from_u8(f.field_type).unwrap())
            .collect();

        let mut buf = Vec::new();
        state.write_checkpoint(&mut buf, &ft).unwrap();

        // Read back
        let mut state2 = StorageState::new(&def);
        let mut cursor = std::io::Cursor::new(&buf);
        let block = CheckpointBlock::read_from(&mut cursor).unwrap();
        assert_eq!(block.storage_id, 0);
        state2.read_checkpoint(&mut cursor, block.size).unwrap();

        assert!(!state2.slot_valid(0));
        assert!(state2.slot_valid(1));
        assert!(!state2.slot_valid(2));
        assert!(state2.slot_valid(3));
        assert_eq!(state2.get_field_at(1, &offsets, 0), 42);
        assert_eq!(state2.get_field_at(1, &offsets, 1), 0x1234);
        assert_eq!(state2.get_field_at(3, &offsets, 0), 99);
    }

    #[test]
    fn checkpoint_roundtrip_with_properties() {
        let mut def = make_test_storage();
        def.num_properties = 2;
        def.properties = vec![
            FieldDef {
                name: 0,
                field_type: FieldType::U16 as u8,
                enum_id: 0,
                role: 0,
                pair_id: 0,
                reserved: [0; 2],
            },
            FieldDef {
                name: 0,
                field_type: FieldType::U32 as u8,
                enum_id: 0,
                role: 0,
                pair_id: 0,
                reserved: [0; 2],
            },
        ];
        let offsets = FieldOffsets::from_storage_def(&def);
        let mut state = StorageState::new(&def);

        // Set slot data
        state.set_field_at(1, &offsets, 0, 42);
        state.set_field_at(1, &offsets, 1, 0x1234);

        // Set property data
        state.set_property(&offsets, 0, 5); // u16 property
        state.set_property(&offsets, 1, 99); // u32 property

        assert_eq!(state.get_property(&offsets, 0), 5);
        assert_eq!(state.get_property(&offsets, 1), 99);

        let ft: Vec<FieldType> = def
            .fields
            .iter()
            .map(|f| FieldType::from_u8(f.field_type).unwrap())
            .collect();

        let mut buf = Vec::new();
        state.write_checkpoint(&mut buf, &ft).unwrap();

        // Read back
        let mut state2 = StorageState::new(&def);
        let mut cursor = std::io::Cursor::new(&buf);
        let block = CheckpointBlock::read_from(&mut cursor).unwrap();
        state2.read_checkpoint(&mut cursor, block.size).unwrap();

        // Verify slot data
        assert!(state2.slot_valid(1));
        assert_eq!(state2.get_field_at(1, &offsets, 0), 42);
        assert_eq!(state2.get_field_at(1, &offsets, 1), 0x1234);

        // Verify property data
        assert_eq!(state2.get_property(&offsets, 0), 5);
        assert_eq!(state2.get_property(&offsets, 1), 99);
    }

    #[test]
    fn checkpoint_roundtrip_dense() {
        let mut def = make_test_storage();
        def.flags = 0; // dense
        let offsets = FieldOffsets::from_storage_def(&def);
        let mut state = StorageState::new(&def);

        state.set_field_at(0, &offsets, 0, 10);
        state.set_field_at(2, &offsets, 1, 20);

        let ft: Vec<FieldType> = def
            .fields
            .iter()
            .map(|f| FieldType::from_u8(f.field_type).unwrap())
            .collect();

        let mut buf = Vec::new();
        state.write_checkpoint(&mut buf, &ft).unwrap();

        let mut state2 = StorageState::new(&def);
        state2.is_sparse = false;
        let mut cursor = std::io::Cursor::new(&buf);
        let block = CheckpointBlock::read_from(&mut cursor).unwrap();
        state2.read_checkpoint(&mut cursor, block.size).unwrap();

        assert_eq!(state2.get_field_at(0, &offsets, 0), 10);
        assert_eq!(state2.get_field_at(2, &offsets, 1), 20);
    }
}
