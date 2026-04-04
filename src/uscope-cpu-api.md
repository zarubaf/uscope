# uscope-cpu: CPU Protocol Library

**Crate:** `uscope-cpu`
**Location:** `crates/uscope-cpu/`

---

## Overview

The `uscope-cpu` crate provides the CPU protocol interpretation layer on top of the `uscope` transport crate. It understands instruction lifecycles, pipeline stages, performance counters, and hardware buffers — concepts that the transport layer treats as opaque storages and events.

### Architecture

```
uscope-cpu (this crate)          uscope (transport)
┌──────────────────────┐         ┌─────────────────┐
│ CpuTrace             │────────▶│ Reader           │
│  - instructions      │         │  - state_at()    │
│  - stages            │         │  - segment_replay│
│  - counters          │         │  - schema()      │
│  - buffers           │         └─────────────────┘
│  - lazy loading      │
│  - performance stats │
└──────────────────────┘
```

### Dependencies

| Crate | Purpose |
|-------|---------|
| `uscope` | Transport layer (Reader, Schema, state reconstruction) |
| `instruction-decoder` | RISC-V ISA decode (optional, behind `decode` feature) |

---

## CpuTrace

The main entry point. Opens a trace file, resolves the CPU protocol schema, and provides query methods.

### Opening a trace

```rust
use uscope_cpu::CpuTrace;

let mut trace = CpuTrace::open("trace.uscope")?;

// File overview
let info = trace.file_info();
println!("Version: {}.{}", info.version_major, info.version_minor);
println!("Segments: {}", info.num_segments);
println!("Max cycle: {}", trace.max_cycle());
println!("Period: {} ps", trace.period_ps());

// Schema access
for (name, _) in trace.counter_names() {
    println!("Counter: {}", name);
}
for buf in trace.buffer_infos() {
    println!("Buffer: {} ({} slots)", buf.name, buf.capacity);
}
```

### Counter queries

```rust
// Cumulative value at a cycle
let val = trace.counter_value_at(0, 100);

// Rate over a window (instructions per cycle)
let ipc = trace.counter_rate_at(0, 100, 64);

// Single-cycle delta
let delta = trace.counter_delta_at(0, 100);

// Downsample for sparkline rendering (min/max envelope)
let data = trace.counter_downsample(0, 0, 1000, 200);
for (min_rate, max_rate) in &data {
    // render bar from min to max
}
```

### Buffer state

```rust
let state = trace.buffer_state_at(0, 50)?;
println!("Capacity: {}", state.capacity);

// Occupied slots
for slot in &state.slots {
    println!("Slot 0x{:02x}: entity_id={}", slot.0, slot.1[0]);
}

// Storage-level properties (pointer pairs)
for prop in &state.properties {
    println!("{}: {} (role={}, pair_id={})",
        prop.name, prop.value, prop.role, prop.pair_id);
}
```

### Lazy segment loading

```rust
// Load specific segments (instruction/stage data)
let result = trace.load_segments(&[0, 1, 2])?;
println!("Loaded {} instructions", result.instructions.len());

// Or load segments covering a cycle range
let loaded = trace.ensure_loaded(100, 200);
```

### Metadata

```rust
for (key, value) in trace.metadata() {
    println!("{}: {}", key, value);
}
```

---

## Types

### InstructionData

```rust
pub struct InstructionData {
    pub id: u32,              // Entity ID
    pub sim_id: u64,          // Simulator-assigned ID
    pub thread_id: u16,
    pub rbid: Option<u32>,    // Retire buffer slot
    pub iq_id: Option<u32>,   // Issue queue ID
    pub dq_id: Option<u32>,   // Dispatch queue ID
    pub ready_cycle: Option<u32>,
    pub pc: u64,
    pub disasm: String,
    pub tooltip: String,
    pub stage_range: Range<u32>,  // Index range into stages vec
    pub retire_status: RetireStatus,
    pub first_cycle: u32,
    pub last_cycle: u32,
}
```

### StageSpan

```rust
pub struct StageSpan {
    pub stage_name_idx: u16,  // Index into stage name table
    pub lane: u16,
    pub start_cycle: u32,
    pub end_cycle: u32,
}
```

### BufferInfo

```rust
pub struct BufferInfo {
    pub name: String,
    pub storage_id: u16,
    pub capacity: u16,
    pub fields: Vec<(String, u8)>,
    pub properties: Vec<BufferPropertyDef>,
}

pub struct BufferPropertyDef {
    pub name: String,
    pub field_type: u8,
    pub role: u8,     // 0=plain, 1=HEAD_PTR, 2=TAIL_PTR
    pub pair_id: u8,  // Groups head/tail into pairs
}
```

### CounterSeries

```rust
pub struct CounterSeries {
    pub name: String,
    pub samples: Vec<(u32, u64)>,  // (cycle, cumulative_value)
    pub default_mode: CounterDisplayMode,
}
```

### SegmentIndex

```rust
pub struct SegmentIndex {
    pub segments: Vec<(u32, u32)>,  // (start_cycle, end_cycle)
}

impl SegmentIndex {
    pub fn segments_in_range(&self, start: u32, end: u32) -> Vec<usize>;
}
```

---

## Feature Flags

| Feature | Default | Description |
|---------|---------|-------------|
| `decode` | yes | RISC-V instruction decode via `instruction-decoder` |
