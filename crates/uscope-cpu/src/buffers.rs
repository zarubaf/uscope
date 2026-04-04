use crate::types::{BufferInfo, BufferState, PropertyValue};
use uscope::reader::Reader;

/// Query buffer storage state at a given cycle.
///
/// Returns occupied slots with field values, entity fields, and storage-level
/// property values (with pointer-pair metadata for ROB visualization).
///
/// Returns an empty `BufferState` if the query fails.
pub fn buffer_state_at(
    reader: &mut Reader,
    buffer: &BufferInfo,
    entities_storage_id: u16,
    period_ps: u64,
    cycle: u32,
) -> BufferState {
    let time_ps = cycle as u64 * period_ps;
    let trace_state = match reader.state_at(time_ps) {
        Ok(s) => s,
        Err(_) => return BufferState::default(),
    };

    let storage_id = buffer.storage_id;
    let storage = match trace_state
        .storages
        .iter()
        .find(|s| s.storage_id == storage_id)
    {
        Some(s) => s,
        None => return BufferState::default(),
    };

    let offsets = reader.field_offsets().get(storage_id as usize).cloned();
    let offsets = match offsets {
        Some(o) => o,
        None => return BufferState::default(),
    };

    // Find the entities storage and its field names for entity field lookup.
    let entities_storage = trace_state
        .storages
        .iter()
        .find(|s| s.storage_id == entities_storage_id);
    let entities_offsets = reader
        .field_offsets()
        .get(entities_storage_id as usize)
        .cloned();
    let schema = reader.schema();
    let entity_field_names: Vec<String> = schema
        .storages
        .iter()
        .find(|s| s.storage_id == entities_storage_id)
        .map(|s| {
            s.fields
                .iter()
                .map(|f| schema.get_string(f.name).unwrap_or("?").to_string())
                .collect()
        })
        .unwrap_or_default();

    let num_fields = buffer.fields.len() as u16;

    // Pre-build entity_id -> slot index map for O(1) lookup (avoids O(B*E) nested scan).
    let entity_slot_map: std::collections::HashMap<u64, u16> =
        if let (Some(es), Some(ref eo)) = (&entities_storage, &entities_offsets) {
            (0..es.num_slots)
                .filter(|&s| es.valid.get(s as usize).copied().unwrap_or(false))
                .map(|s| (es.get_field_at(s, eo, 0), s))
                .collect()
        } else {
            std::collections::HashMap::new()
        };

    let mut slots = Vec::new();

    for slot in 0..storage.num_slots {
        if storage.is_sparse && !storage.valid.get(slot as usize).copied().unwrap_or(false) {
            continue;
        }
        let mut field_values = Vec::with_capacity(num_fields as usize);
        for fi in 0..num_fields {
            field_values.push(storage.get_field_at(slot, &offsets, fi));
        }
        let entity_id = storage.get_field_at(slot, &offsets, 0);

        // Look up entity fields via pre-built map (O(1) per slot).
        let mut entity_fields = Vec::new();
        if let (Some(es), Some(ref eo)) = (&entities_storage, &entities_offsets) {
            if let Some(&es_slot) = entity_slot_map.get(&entity_id) {
                for (fi, name) in entity_field_names.iter().enumerate().skip(2) {
                    let val = es.get_field_at(es_slot, eo, fi as u16);
                    entity_fields.push((name.clone(), val));
                }
            }
        }

        slots.push((slot, field_values, entity_fields));
    }

    // Read storage-level property values (v0.3: pointer pairs, etc.).
    let mut properties = Vec::new();
    let buf_schema = schema.storages.iter().find(|s| s.storage_id == storage_id);
    if let Some(bsd) = buf_schema {
        for (pi, prop) in bsd.properties.iter().enumerate() {
            let val = storage.get_property(&offsets, pi as u16);
            let name = schema.get_string(prop.name).unwrap_or("?").to_string();
            properties.push(PropertyValue {
                name,
                value: val,
                role: prop.role,
                pair_id: prop.pair_id,
            });
        }
    }

    BufferState {
        slots,
        properties,
        capacity: buffer.capacity,
    }
}
