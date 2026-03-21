# Rust Crate API Reference

**Crate:** `uscope`
**Location:** `crates/uscope/`

---

## 1. Overview

The `uscope` Rust crate provides a complete reader and writer for the µScope
trace format. It implements the transport layer (file header, preamble, schema,
segments, checkpoints, deltas, string table, section table) and the CPU
protocol layer (entity catalog, pipeline stages, typed events).

### Dependencies

| Crate | Purpose |
|-------|---------|
| `byteorder` | Little-endian integer read/write |
| `lz4_flex` | Pure-Rust LZ4 compression |

No other runtime dependencies.

---

## 2. Schema Building

Use `SchemaBuilder` and `DutDescBuilder` to define the trace structure before
writing.

### 2.1 SchemaBuilder

```rust
use uscope::schema::{SchemaBuilder, FieldSpec};
use uscope::types::SF_SPARSE;

let mut sb = SchemaBuilder::new();

// Clock domain: 5 GHz (200 ps period)
let clk = sb.clock_domain("core_clk", 200);

// Scope hierarchy
sb.scope("root", None, None, None);
let scope = sb.scope("core0", Some(0), Some("cpu"), Some(clk));

// Enum type
let stage_enum = sb.enum_type(
    "pipeline_stage",
    &["fetch", "decode", "execute", "writeback"],
);

// Storage (entity catalog)
let entities = sb.storage(
    "entities", scope, 512, SF_SPARSE,
    &[
        ("entity_id", FieldSpec::U32),
        ("pc",        FieldSpec::U64),
        ("inst_bits", FieldSpec::U32),
    ],
);

// Event type
let stage_ev = sb.event(
    "stage_transition", scope,
    &[
        ("entity_id", FieldSpec::U32),
        ("stage",     FieldSpec::Enum(stage_enum)),
    ],
);

let schema = sb.build();
```

**Methods:**

| Method | Returns | Description |
|--------|---------|-------------|
| `clock_domain(name, period_ps)` | `u8` | Add a clock domain |
| `scope(name, parent, protocol, clock_id)` | `u16` | Add a scope |
| `enum_type(name, values)` | `u8` | Add an enum type |
| `storage(name, scope, slots, flags, fields)` | `u16` | Add a storage definition |
| `event(name, scope, fields)` | `u16` | Add an event type |
| `summary_field(name, type, scope)` | — | Add a summary field |
| `strings_mut()` | `&mut StringPoolBuilder` | Access the string pool |
| `build()` | `Schema` | Consume builder, produce schema |

### 2.2 DutDescBuilder

```rust
use uscope::schema::DutDescBuilder;

let mut dut = DutDescBuilder::new();
dut.property("dut_name", "boom_core_0")
   .property("cpu.isa", "RV64GC")
   .property("cpu.pipeline_stages", "fetch,decode,execute,writeback");

// Build using the schema's shared string pool
let dut_desc = dut.build(sb.strings_mut());
```

### 2.3 FieldSpec

| Variant | Wire type | Size |
|---------|-----------|------|
| `FieldSpec::U8` | `FT_U8` | 1 |
| `FieldSpec::U16` | `FT_U16` | 2 |
| `FieldSpec::U32` | `FT_U32` | 4 |
| `FieldSpec::U64` | `FT_U64` | 8 |
| `FieldSpec::I8` | `FT_I8` | 1 |
| `FieldSpec::I16` | `FT_I16` | 2 |
| `FieldSpec::I32` | `FT_I32` | 4 |
| `FieldSpec::I64` | `FT_I64` | 8 |
| `FieldSpec::Bool` | `FT_BOOL` | 1 |
| `FieldSpec::StringRef` | `FT_STRING_REF` | 4 |
| `FieldSpec::Enum(id)` | `FT_ENUM` | 1 |

---

## 3. Writer

`Writer<W>` writes µScope trace files in streaming, append-only fashion.

### 3.1 Creating a Writer

```rust
use uscope::writer::Writer;
use std::fs::File;

let file = File::create("trace.uscope")?;
let mut w = Writer::create(file, &dut_desc, &schema, checkpoint_interval_ps)?;
```

The `checkpoint_interval_ps` parameter controls how often a full checkpoint is
written. Smaller intervals allow faster random-access seeks at the cost of
larger files.

### 3.2 Writing Cycles

All storage mutations and events must occur within a `begin_cycle` /
`end_cycle` pair. Time must be monotonically non-decreasing.

```rust
w.begin_cycle(time_ps);

// Mutate storage slots
w.slot_set(storage_id, slot, field, value);
w.slot_add(storage_id, slot, field, delta);
w.slot_clear(storage_id, slot);

// Emit events (payload is pre-serialized, fields concatenated LE)
w.event(event_type_id, &payload_bytes);

w.end_cycle()?;
```

| Method | Description |
|--------|-------------|
| `begin_cycle(time_ps)` | Start a cycle frame at the given time |
| `slot_set(storage, slot, field, value)` | Set a field value (marks slot valid) |
| `slot_clear(storage, slot)` | Mark slot invalid (sparse only) |
| `slot_add(storage, slot, field, delta)` | Add to a field value |
| `event(type_id, payload)` | Emit an event with raw payload |
| `end_cycle()` | Finish the cycle frame |

### 3.3 String Table

For `STRING_REF` fields, insert strings into the writer's string table:

```rust
let text_idx = w.string_table.insert("addi x0, x0, 0");
// Use text_idx as the u32 value for a STRING_REF field in event payloads
```

### 3.4 Finalization

```rust
let file = w.close()?;  // Writes string table, segment table, section table
```

Calling `close()` sets `F_COMPLETE`, writes the section table, and returns the
underlying writer. The file is then readable by `Reader`.

---

## 4. Reader

`Reader` opens µScope trace files for random-access reading.

### 4.1 Opening a File

```rust
use uscope::reader::Reader;

let mut r = Reader::open("trace.uscope")?;
```

Handles both finalized (`F_COMPLETE`) and in-progress files. For finalized
files, the section table is used for fast segment lookup. For in-progress
files, the segment chain is walked from `tail_offset`.

### 4.2 Metadata Access

```rust
let header = r.header();           // FileHeader
let schema = r.schema();           // Schema (clock domains, scopes, storages, events)
let dut = r.dut_desc();            // DutDesc (key-value properties)
let config = r.trace_config();     // TraceConfig (checkpoint_interval_ps)
let offsets = r.field_offsets();    // Precomputed field offsets per storage

// Look up a DUT property by key
let isa = r.dut_property("cpu.isa");  // Some("RV64GC")

// String table (for STRING_REF field values)
if let Some(st) = r.string_table() {
    let text = st.get(0);  // Some("addi x0, x0, 0")
}
```

### 4.3 State Reconstruction

Reconstruct the full storage state at any point in time. The reader finds the
appropriate segment, loads its checkpoint, and replays deltas up to the target
time.

```rust
let state = r.state_at(time_ps)?;

// Query storage state
let valid = state.slot_valid(storage_id, slot);
let value = state.slot_field(storage_id, slot, field_index, &offsets[storage_id]);
```

### 4.4 Event Queries

```rust
let events = r.events_in_range(t0_ps, t1_ps)?;
for ev in &events {
    println!("t={} type={} payload={:?}", ev.time_ps, ev.event_type_id, ev.payload);
}
```

### 4.5 Segment-Level Access

```rust
let n = r.segment_count();
let (storages, events, ops) = r.segment_replay(seg_idx)?;
```

`segment_replay` returns the checkpoint state after full delta replay, plus
all events and storage operations (`TimedOp`) in the segment.

### 4.6 Live Tailing

For traces being written concurrently:

```rust
loop {
    if r.poll_new_segments()? {
        // New segments available — re-query events or state
    }
    std::thread::sleep(std::time::Duration::from_millis(100));
}
```

---

## 5. CPU Protocol Helpers

The `protocols::cpu` module provides higher-level APIs that implement the CPU
protocol conventions on top of the transport-layer primitives.

### 5.1 CpuSchemaBuilder

Constructs a complete CPU-protocol schema with all standard enums, storages,
and events.

```rust
use uscope::protocols::cpu::CpuSchemaBuilder;
use uscope::schema::FieldSpec;

let (dut_builder, mut schema_builder, ids) = CpuSchemaBuilder::new("core0")
    .isa("RV64GC")
    .pipeline_stages(&["fetch", "decode", "rename", "dispatch",
                        "issue", "execute", "complete", "retire"])
    .fetch_width(4)
    .commit_width(4)
    .entity_slots(512)
    .buffer("rob", 256, &[("completed", FieldSpec::Bool)])
    .buffer("iq_int", 48, &[])
    .counter("committed_insns")
    .counter("bp_misses")
    .build();

let dut = dut_builder.build(schema_builder.strings_mut());
let schema = schema_builder.build();
```

**Builder methods:**

| Method | Description |
|--------|-------------|
| `isa(name)` | Set ISA (e.g. `"RV64GC"`) |
| `pipeline_stages(names)` | Define pipeline stage enum |
| `fetch_width(n)` | Set fetch width DUT property |
| `commit_width(n)` | Set commit width DUT property |
| `entity_slots(n)` | Max in-flight entities (default: 512) |
| `elf_path(path)` | Set ELF path for disassembly |
| `vendor(name)` | Set vendor DUT property |
| `buffer(name, slots, fields)` | Add a hardware buffer storage |
| `counter(name)` | Add a counter (1-slot dense storage) |
| `stall_reasons(names)` | Override default stall reason enum |

**CpuIds** — returned by `build()`, contains all assigned IDs:

| Field | Type | Description |
|-------|------|-------------|
| `scope_id` | `u16` | CPU scope ID |
| `entities_storage_id` | `u16` | Entity catalog storage ID |
| `stage_transition_event_id` | `u16` | Stage transition event type |
| `annotate_event_id` | `u16` | Annotation event type |
| `dependency_event_id` | `u16` | Dependency event type |
| `flush_event_id` | `u16` | Flush event type |
| `stall_event_id` | `u16` | Stall event type |
| `field_entity_id` | `u16` | Field index: entity_id |
| `field_pc` | `u16` | Field index: pc |
| `field_inst_bits` | `u16` | Field index: inst_bits |
| `buffers` | `Vec<(String, u16)>` | Buffer (name, storage_id) pairs |
| `counters` | `Vec<(String, u16, u16)>` | Counter (name, storage_id, field) triples |

### 5.2 CpuWriter

Typed helpers that emit the correct transport-layer operations for CPU protocol
semantics.

```rust
use uscope::protocols::cpu::CpuWriter;

let cpu = CpuWriter::new(ids);

w.begin_cycle(time_ps);

// Fetch: allocate entity in catalog
cpu.fetch(&mut w, entity_id, pc, inst_bits);

// Stage transition
cpu.stage_transition(&mut w, entity_id, stage_index);

// Retire: clear entity from catalog
cpu.retire(&mut w, entity_id);

// Flush: emit flush event + clear entity
cpu.flush(&mut w, entity_id, reason);

// Annotation: insert text into string table + emit event
cpu.annotate(&mut w, entity_id, "decoded: addi x1, x0, 1");

// Dependency: record data/structural dependency
cpu.dependency(&mut w, src_entity, dst_entity, dep_type);

// Stall
cpu.stall(&mut w, reason);

// Counter increment
cpu.counter_add(&mut w, "committed_insns", 1);

w.end_cycle()?;
```

| Method | Transport ops | Description |
|--------|---------------|-------------|
| `fetch(w, id, pc, bits)` | 3 × `slot_set` | Allocate entity |
| `stage_transition(w, id, stage)` | 1 × `event` | Pipeline stage change |
| `retire(w, id)` | 1 × `slot_clear` | Normal retirement |
| `flush(w, id, reason)` | 1 × `event` + 1 × `slot_clear` | Squash |
| `annotate(w, id, text)` | 1 × `string_insert` + 1 × `event` | Text annotation |
| `dependency(w, src, dst, type)` | 1 × `event` | Data dependency |
| `stall(w, reason)` | 1 × `event` | Pipeline stall |
| `counter_add(w, name, delta)` | 1 × `slot_add` | Increment counter |

---

## 6. Example: Full Write-Read Cycle

```rust
use uscope::protocols::cpu::{CpuSchemaBuilder, CpuWriter};
use uscope::writer::Writer;
use uscope::reader::Reader;
use std::fs::File;

// Build schema
let (dut_builder, mut sb, ids) = CpuSchemaBuilder::new("core0")
    .isa("RV64GC")
    .pipeline_stages(&["fetch", "decode", "execute", "writeback"])
    .entity_slots(16)
    .build();

let dut = dut_builder.build(sb.strings_mut());
let schema = sb.build();

// Write
let file = File::create("trace.uscope").unwrap();
let mut w = Writer::create(file, &dut, &schema, 10_000).unwrap();
let cpu = CpuWriter::new(ids.clone());

w.begin_cycle(0);
cpu.fetch(&mut w, 0, 0x8000_0000, 0x13);
cpu.stage_transition(&mut w, 0, 0);
w.end_cycle().unwrap();

w.begin_cycle(1000);
cpu.stage_transition(&mut w, 0, 1);
w.end_cycle().unwrap();

w.begin_cycle(2000);
cpu.stage_transition(&mut w, 0, 2);
w.end_cycle().unwrap();

w.begin_cycle(3000);
cpu.stage_transition(&mut w, 0, 3);
cpu.retire(&mut w, 0);
w.end_cycle().unwrap();

w.close().unwrap();

// Read
let mut r = Reader::open("trace.uscope").unwrap();
assert_eq!(r.header().total_time_ps, 3000);

let state = r.state_at(1500).unwrap();
assert!(state.slot_valid(ids.entities_storage_id, 0)); // still in-flight

let state = r.state_at(3000).unwrap();
assert!(!state.slot_valid(ids.entities_storage_id, 0)); // retired

let events = r.events_in_range(0, 3000).unwrap();
assert_eq!(events.len(), 4); // 4 stage transitions
```
