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

/// Replay delta blob, starting from checkpoint state, up to a target time.
/// Returns the final state and all events encountered.
pub fn replay_deltas(
    delta_data: &[u8],
    storages: &mut Vec<StorageState>,
    field_offsets: &[FieldOffsets],
    start_time_ps: u64,
    target_time_ps: Option<u64>,
    compact_deltas: bool,
) -> io::Result<(u64, Vec<TimedEvent>)> {
    let mut cursor = Cursor::new(delta_data);
    let mut time_ps = start_time_ps;
    let mut events = Vec::new();

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
                match op.action {
                    DA_SLOT_SET => {
                        storages[sid].set_field_at(
                            op.slot_index,
                            &field_offsets[sid],
                            op.field_index,
                            op.value,
                        );
                    }
                    DA_SLOT_CLEAR => {
                        storages[sid].clear_slot(op.slot_index);
                    }
                    DA_SLOT_ADD => {
                        storages[sid].add_field_at(
                            op.slot_index,
                            &field_offsets[sid],
                            op.field_index,
                            op.value,
                        );
                    }
                    _ => {}
                }
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

    Ok((time_ps, events))
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
            fields: vec![FieldDef {
                name: 0,
                field_type: FieldType::U32 as u8,
                enum_id: 0,
                reserved: [0; 4],
            }],
        }
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

        let (final_time, events) =
            replay_deltas(&delta, &mut storages, &offsets, 1000, None, false).unwrap();

        assert_eq!(final_time, 1000); // delta was 0
        assert!(events.is_empty());
        assert!(storages[0].slot_valid(2));
        assert_eq!(storages[0].get_field_at(2, &offsets[0], 0), 42);
    }
}
