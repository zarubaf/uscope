# µScope Format TODO

## Done (v4.0)

- ~~Varint/delta cycle encoding in cycle frames~~ (spec §8.3.1)
- ~~Entities removed~~ — modeled as storages
- ~~Annotations removed~~ — modeled as events
- ~~Counters removed~~ — modeled as 1-slot storages with DA_SLOT_ADD
- ~~SF_CIRCULAR, SF_CAM, DA_HEAD, DA_TAIL removed~~ — head/tail are fields
- ~~Display flags removed~~ — protocol/viewer concern
- ~~Summary source/aggregation semantics removed~~ — opaque to transport
- ~~Two string pools merged into one~~
- ~~DUT descriptor simplified~~
- ~~Delta actions: 7 → 3 (SET, CLEAR, ADD)~~

## Medium Priority

### 4-byte micro-op for small enum field sets
Add a 4-byte micro-op for the most common case: `DA_SLOT_SET` on a known
storage with an 8-bit enum value:

```c
typedef struct {
    uint8_t  action_and_storage;  // action in high 4 bits, storage in low 4
    uint8_t  slot_index;          // enough for most storages
    uint8_t  field_index;
    uint8_t  value8;
} delta_op_micro_t;               // 4 bytes
```

### "Repeat storage+field" prefix for batched slot updates
Add a run-length style header: "N ops for storage S, field F" followed by
`(slot, value)` pairs. Saves 2-3 bytes/op for bulk updates on the same
storage and field.

## Low Priority

### Sub-segment compression blocks
Consider sub-segment compression blocks (per-N-cycles or per-page) with
ZSTD dictionary mode for streaming decompression without needing the full
segment in memory.

## Future

### Multi-channel write support
Needed for multi-hart/multi-thread DUT instrumentation where different
writers emit concurrently.

---

## Spec Review — Resolved

All items from the spec review have been applied to the spec:

- ~~`storage_id` width mismatch~~ — widened `delta_op_t.storage_id` to `uint16_t`, updated writer/reader APIs and DPI bridge
- ~~`enum_id` width mismatch~~ — narrowed `schema_header_t.num_enums` to `uint8_t`
- ~~Compact delta bit-stuffing~~ — replaced with per-frame `op_format` field in cycle frame + `F_COMPACT_DELTAS` file flag
- ~~`num_deltas` ambiguity~~ — renamed to `num_cycle_frames`
- ~~Event payload layout~~ — added §8.6.5 Payload Wire Format (tightly packed, schema order, no padding)
- ~~Intra-segment alignment~~ — specified tightly packed in §8.6.5
- ~~Live-read atomicity~~ — specified write order in §2.2 (`tail_offset` is the commit point)
- ~~Endianness of payloads~~ — blanket LE statement added to file header
- ~~`field_def_t.size` redundancy~~ — removed `size` field, derived from type with lookup table
- ~~`summary_field_def_t.size`~~ — removed `size` field, derived from type
- ~~Compression method reserved values~~ — "2–7 reserved, must not be used" added to §2.1
- ~~`event_record_t.reserved`~~ — "must be 0" added
- ~~Compact delta flag~~ — `F_COMPACT_DELTAS` (bit 6) added to file header flags
- ~~`cycle_frame_t` pseudo-struct~~ — replaced with wire-format diagram

### Still open: String pool 64 KB limit
The string pool is capped at 64 KB (`uint16_t` offsets). For a DUT with
many storages, each having many enum types with verbose labels, this
could be tight. Not a blocker for v4.0 — document the limit and add a
writer-side validation check. A future version can add a `uint32_t`
offset mode behind a flag if needed.
