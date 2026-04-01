/// State reconstruction: load checkpoint, replay deltas to reconstruct
/// storage state at any point in time.
use crate::checkpoint::{FieldOffsets, StorageState};
use crate::leb128;
use crate::types::*;
use byteorder::{LittleEndian, ReadBytesExt};
use std::io::{self, Cursor};

/// Reconstructed state at a point in time.
#[derive(Debug)]
pub struct TraceState {
    pub time_ps: u64,
    pub storages: Vec<StorageState>,
}

impl TraceState {
    pub fn slot_valid(&self, storage_id: u16, slot: u16) -> bool {
        let sid = storage_id as usize;
        if sid < self.storages.len() {
            self.storages[sid].slot_valid(slot)
        } else {
            false
        }
    }

    pub fn slot_field(
        &self,
        storage_id: u16,
        slot: u16,
        field: u16,
        offsets: &FieldOffsets,
    ) -> u64 {
        let sid = storage_id as usize;
        if sid < self.storages.len() {
            self.storages[sid].get_field_at(slot, offsets, field)
        } else {
            0
        }
    }
}

/// Event observed during delta replay.
#[derive(Debug, Clone)]
pub struct TimedEvent {
    pub time_ps: u64,
    pub event_type_id: u16,
    pub payload: Vec<u8>,
}

/// Storage operation observed during delta replay.
#[derive(Debug, Clone)]
pub struct TimedOp {
    pub time_ps: u64,
    pub storage_id: u16,
    pub slot: u16,
    pub action: u8,
    pub field_index: u16,
    pub value: u64,
}

/// An ordered item from the v0.2 interleaved format.
#[derive(Debug, Clone)]
pub enum TimedItem {
    Op(TimedOp),
    Event(TimedEvent),
}

impl TimedItem {
    pub fn time_ps(&self) -> u64 {
        match self {
            TimedItem::Op(op) => op.time_ps,
            TimedItem::Event(ev) => ev.time_ps,
        }
    }
}

/// Replay delta blob, starting from checkpoint state, up to a target time.
/// Returns the final state and all events encountered.
///
/// For v0.1 format (compact_deltas flag selects compact vs wide ops).
/// For v0.2 interleaved format, use `replay_deltas_v2`.
pub fn replay_deltas(
    delta_data: &[u8],
    storages: &mut Vec<StorageState>,
    field_offsets: &[FieldOffsets],
    start_time_ps: u64,
    target_time_ps: Option<u64>,
    compact_deltas: bool,
) -> io::Result<(u64, Vec<TimedEvent>, Vec<TimedOp>)> {
    let mut cursor = Cursor::new(delta_data);
    let mut time_ps = start_time_ps;
    let mut events = Vec::new();
    let mut ops = Vec::new();

    while (cursor.position() as usize) < delta_data.len() {
        // Read time_delta (LEB128)
        let remaining = &delta_data[cursor.position() as usize..];
        let (time_delta, consumed) = leb128::decode_u64(remaining)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        cursor.set_position(cursor.position() + consumed as u64);
        time_ps += time_delta;

        // If we've passed the target time, stop
        if let Some(target) = target_time_ps {
            if time_ps > target {
                break;
            }
        }

        let op_format = cursor.read_u8()?;
        let _reserved = cursor.read_u8()?;
        let num_ops = cursor.read_u16::<LittleEndian>()?;
        let num_events = cursor.read_u16::<LittleEndian>()?;

        // Read ops
        for _ in 0..num_ops {
            let op = if op_format == 1 && compact_deltas {
                DeltaOpCompact::read_from(&mut cursor)?.to_wide()
            } else {
                DeltaOp::read_from(&mut cursor)?
            };

            let sid = op.storage_id as usize;
            if sid < storages.len() {
                ops.push(TimedOp {
                    time_ps,
                    storage_id: op.storage_id,
                    slot: op.slot_index,
                    action: op.action,
                    field_index: op.field_index,
                    value: op.value,
                });
                apply_op(
                    storages,
                    field_offsets,
                    sid,
                    op.slot_index,
                    op.action,
                    op.field_index,
                    op.value,
                );
            }
        }

        // Read events
        for _ in 0..num_events {
            let ev = EventRecord::read_from(&mut cursor)?;
            events.push(TimedEvent {
                time_ps,
                event_type_id: ev.event_type_id,
                payload: ev.payload,
            });
        }
    }

    Ok((time_ps, events, ops))
}

/// Replay delta blob in v0.2 interleaved format.
/// Items are returned in their original insertion order.
pub fn replay_deltas_v2(
    delta_data: &[u8],
    storages: &mut Vec<StorageState>,
    field_offsets: &[FieldOffsets],
    start_time_ps: u64,
    target_time_ps: Option<u64>,
) -> io::Result<(u64, Vec<TimedItem>)> {
    let mut cursor = Cursor::new(delta_data);
    let mut time_ps = start_time_ps;
    let mut items = Vec::new();

    while (cursor.position() as usize) < delta_data.len() {
        // Read time_delta (LEB128)
        let remaining = &delta_data[cursor.position() as usize..];
        let (time_delta, consumed) = leb128::decode_u64(remaining)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        cursor.set_position(cursor.position() + consumed as u64);
        time_ps += time_delta;

        // If we've passed the target time, stop
        if let Some(target) = target_time_ps {
            if time_ps > target {
                break;
            }
        }

        let num_items = cursor.read_u16::<LittleEndian>()?;

        for _ in 0..num_items {
            let tag = cursor.read_u8()?;
            match tag {
                TAG_WIDE_OP => {
                    // 15 more bytes: action:u8 storage_id:u16 slot:u16 field:u16 value:u64
                    let action = cursor.read_u8()?;
                    let storage_id = cursor.read_u16::<LittleEndian>()?;
                    let slot_index = cursor.read_u16::<LittleEndian>()?;
                    let field_index = cursor.read_u16::<LittleEndian>()?;
                    let value = cursor.read_u64::<LittleEndian>()?;

                    let sid = storage_id as usize;
                    if sid < storages.len() {
                        apply_op(
                            storages,
                            field_offsets,
                            sid,
                            slot_index,
                            action,
                            field_index,
                            value,
                        );
                        items.push(TimedItem::Op(TimedOp {
                            time_ps,
                            storage_id,
                            slot: slot_index,
                            action,
                            field_index,
                            value,
                        }));
                    }
                }
                TAG_COMPACT_OP => {
                    // 7 more bytes: action:u8 storage_id_lo:u8 slot:u16 field:u16 value16:u16
                    let action = cursor.read_u8()?;
                    let storage_id_lo = cursor.read_u8()?;
                    let slot_index = cursor.read_u16::<LittleEndian>()?;
                    let field_index = cursor.read_u16::<LittleEndian>()?;
                    let value16 = cursor.read_u16::<LittleEndian>()?;

                    let storage_id = storage_id_lo as u16;
                    let value = value16 as u64;
                    let sid = storage_id as usize;
                    if sid < storages.len() {
                        apply_op(
                            storages,
                            field_offsets,
                            sid,
                            slot_index,
                            action,
                            field_index,
                            value,
                        );
                        items.push(TimedItem::Op(TimedOp {
                            time_ps,
                            storage_id,
                            slot: slot_index,
                            action,
                            field_index,
                            value,
                        }));
                    }
                }
                TAG_EVENT => {
                    // 7+ more bytes: reserved:u8 event_type_id:u16 payload_size:u32 payload[N]
                    let _reserved = cursor.read_u8()?;
                    let event_type_id = cursor.read_u16::<LittleEndian>()?;
                    let payload_size = cursor.read_u32::<LittleEndian>()?;
                    let mut payload = vec![0u8; payload_size as usize];
                    std::io::Read::read_exact(&mut cursor, &mut payload)?;

                    items.push(TimedItem::Event(TimedEvent {
                        time_ps,
                        event_type_id,
                        payload,
                    }));
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("unknown item tag: 0x{:02x}", tag),
                    ));
                }
            }
        }
    }

    Ok((time_ps, items))
}

/// Apply a delta op to storage state.
fn apply_op(
    storages: &mut [StorageState],
    field_offsets: &[FieldOffsets],
    sid: usize,
    slot_index: u16,
    action: u8,
    field_index: u16,
    value: u64,
) {
    match action {
        DA_SLOT_SET => {
            storages[sid].set_field_at(slot_index, &field_offsets[sid], field_index, value);
        }
        DA_SLOT_CLEAR => {
            storages[sid].clear_slot(slot_index);
        }
        DA_SLOT_ADD => {
            storages[sid].add_field_at(slot_index, &field_offsets[sid], field_index, value);
        }
        DA_PROP_SET => {
            // field_index is used as prop_index, slot_index is unused
            storages[sid].set_property(&field_offsets[sid], field_index, value);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint::FieldOffsets;

    fn make_storage_def() -> StorageDef {
        StorageDef {
            name: 0,
            storage_id: 0,
            num_slots: 4,
            num_fields: 1,
            flags: SF_SPARSE,
            scope_id: 0,
            num_properties: 0,
            reserved_v3: 0,
            fields: vec![FieldDef {
                name: 0,
                field_type: FieldType::U32 as u8,
                enum_id: 0,
                role: 0,
                pair_id: 0,
                reserved: [0; 2],
            }],
            properties: vec![],
        }
    }

    #[test]
    fn replay_prop_set() {
        let mut def = make_storage_def();
        def.num_properties = 1;
        def.properties = vec![FieldDef {
            name: 0,
            field_type: FieldType::U16 as u8,
            enum_id: 0,
            role: 0,
            pair_id: 0,
            reserved: [0; 2],
        }];
        let offsets = vec![FieldOffsets::from_storage_def(&def)];
        let mut storages = vec![StorageState::new(&def)];

        // Build a v0.2 interleaved delta with a DA_PROP_SET op
        let mut delta = Vec::new();
        // time_delta=0
        leb128::encode_u64_vec(0, &mut delta);
        // num_items=1
        delta.extend_from_slice(&1u16.to_le_bytes());
        // Wide op: DA_PROP_SET
        delta.push(TAG_WIDE_OP);
        delta.push(DA_PROP_SET); // action
        delta.extend_from_slice(&0u16.to_le_bytes()); // storage_id=0
        delta.extend_from_slice(&0u16.to_le_bytes()); // slot_index=0 (unused)
        delta.extend_from_slice(&0u16.to_le_bytes()); // field_index=0 (prop_index)
        delta.extend_from_slice(&42u64.to_le_bytes()); // value=42

        let (_final_time, items) =
            replay_deltas_v2(&delta, &mut storages, &offsets, 0, None).unwrap();

        assert_eq!(items.len(), 1);
        assert_eq!(storages[0].get_property(&offsets[0], 0), 42);
    }

    #[test]
    fn replay_simple_deltas() {
        let def = make_storage_def();
        let offsets = vec![FieldOffsets::from_storage_def(&def)];
        let mut storages = vec![StorageState::new(&def)];

        // Build a delta blob manually
        let mut delta = Vec::new();
        // Frame 1: time_delta=0, one SET op
        leb128::encode_u64_vec(0, &mut delta);
        delta.push(0); // op_format = wide
        delta.push(0); // reserved
        delta.extend_from_slice(&1u16.to_le_bytes()); // num_ops=1
        delta.extend_from_slice(&0u16.to_le_bytes()); // num_events=0
        let op = DeltaOp {
            action: DA_SLOT_SET,
            reserved: 0,
            storage_id: 0,
            slot_index: 2,
            field_index: 0,
            value: 42,
        };
        op.write_to(&mut delta).unwrap();

        let (final_time, events, ops) =
            replay_deltas(&delta, &mut storages, &offsets, 1000, None, false).unwrap();

        assert_eq!(final_time, 1000); // delta was 0
        assert!(events.is_empty());
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].action, DA_SLOT_SET);
        assert_eq!(ops[0].slot, 2);
        assert_eq!(ops[0].value, 42);
        assert!(storages[0].slot_valid(2));
        assert_eq!(storages[0].get_field_at(2, &offsets[0], 0), 42);
    }
}
