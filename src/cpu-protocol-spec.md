# µScope `cpu` Protocol Specification

**Version:** 1.0-draft
**Protocol identifier:** `cpu`
**Transport version:** µScope 4.x

---

## 1. Overview

The `cpu` protocol defines conventions for tracing any pipelined CPU —
in-order, out-of-order, VLIW, or multi-threaded — using the µScope
transport layer. It does not prescribe a fixed schema. Instead, it
defines **semantic conventions** that a DUT writer follows and a viewer
relies on to render pipeline visualizations, occupancy charts, and
performance summaries without prior knowledge of the specific
microarchitecture.

### 1.1 Design Principles

1. **Generic over specific.** The protocol works for a 5-stage in-order
   core and a 20-stage OoO core alike. The DUT declares its structures;
   the viewer renders whatever it finds.

2. **Convention over configuration.** Semantics are conveyed through
   field names, storage shapes, and DUT properties — not through
   protocol-specific binary metadata.

3. **Viewer decodes, trace stores data.** The trace carries raw values
   (PC, instruction bits). The viewer decodes disassembly, register
   names, etc. using the ELF and ISA knowledge.

4. **Entity-centric.** Every in-flight instruction has a unique ID. All
   structures reference entities by ID. The viewer joins on this ID to
   build per-instruction timelines.

---

## 2. Concepts

### 2.1 Entities

An **entity** is an in-flight instruction (or micro-op). Each entity
occupies a slot in the **entity catalog** storage and is referenced by
its slot index throughout the pipeline.

- **Entity ID** = slot index in the entity catalog (`U32`).
- When an instruction is fetched, the writer allocates a slot
  (`DA_SLOT_SET` on its fields). When it retires or is flushed, the
  writer clears the slot (`DA_SLOT_CLEAR`). The slot can then be
  reused.
- The entity catalog must be **sparse**.

### 2.2 Buffers

A **buffer** is any storage whose slots hold entity references — a
hardware structure that entities pass through or reside in. Examples:
ROB, issue queues, load/store queues, scoreboards, reservation
stations.

A storage is recognized as a buffer if it contains a field named
`entity_id` (§3.2). The viewer automatically tracks entity
membership in every buffer.

### 2.3 Stages

The viewer renders a per-entity **Gantt chart** showing which pipeline
stage each instruction is in over time. Since an entity can occupy
multiple buffers simultaneously (e.g., ROB + issue queue + executing),
stage progression is tracked **explicitly** via `stage_transition`
events (§5.1), not inferred from buffer membership.

Buffers and stages are orthogonal:
- **Buffers** model where an entity physically resides (ROB slot 42,
  LQ slot 7). An entity can be in multiple buffers at once.
- **Stages** model logical pipeline progress (fetch → decode → ... →
  retire). An entity is in exactly one stage at any time.

The DUT declares the stage ordering via `pipeline_stages` (§4.1) and
emits a `stage_transition` event each time an entity advances. The
viewer maintains a `current_stage` per entity and draws Gantt bars
from stage entry/exit times.

### 2.4 Counters

A **counter** is a 1-slot, non-sparse storage with numeric fields,
mutated via `DA_SLOT_ADD`. The viewer infers counters from this shape
and renders them as line graphs or sparklines. No protocol markup is
needed.

### 2.5 Events

Events model instantaneous occurrences attached to entities or to the
timeline. The protocol defines standard event names (§5). The viewer
renders recognized events with specific visualizations and unknown
events generically.

---

## 3. Entity Catalog

### 3.1 Storage Convention

The entity catalog is a storage named `entities`.

| Property   | Value                           |
| ---------- | ------------------------------- |
| Name       | `entities`                      |
| Sparse     | yes (`SF_SPARSE`)               |
| Num slots  | max concurrent in-flight entities (DUT-specific) |

### 3.2 Required Fields

| Field name   | Type   | Description                               |
| ------------ | ------ | ----------------------------------------- |
| `entity_id`  | `U32`  | Unique entity ID (equals the slot index)  |
| `pc`         | `U64`  | Program counter                           |
| `inst_bits`  | `U32`  | Raw instruction bits                      |

### 3.3 Optional Fields

The DUT may add any additional fields. Common examples:

| Field name      | Type   | Description                                |
| --------------- | ------ | ------------------------------------------ |
| `thread_id`     | `U16`  | Hardware thread / hart ID                  |
| `is_compressed` | `BOOL` | Compressed instruction (RVC, Thumb, ...)   |
| `priv_level`    | `ENUM` | Privilege level at fetch                   |

### 3.4 Entity Lifecycle

```
Fetch:   DA_SLOT_SET  entities[id].entity_id = id
         DA_SLOT_SET  entities[id].pc = ...
         DA_SLOT_SET  entities[id].inst_bits = ...

Retire:  DA_SLOT_CLEAR entities[id]

Flush:   DA_SLOT_CLEAR entities[id]
         (plus a flush event, §5.4)
```

The `entity_id` field is always equal to the slot index. It is stored
explicitly so that buffer storages and events can reference it using a
uniform `U32` field, independent of the transport's slot indexing.

**Slot reuse:** After `DA_SLOT_CLEAR`, the slot may be reused for a new
instruction. The new occupant is a logically distinct entity — the viewer
treats each clear/set cycle as a new entity lifetime. The viewer must not
carry state (stage, annotations, dependencies) across a clear boundary.

---

## 4. Buffers and Stages

### 4.1 Stage Ordering via DUT Properties

The DUT declares pipeline stages using a DUT property:

```
pipeline_stages = "fetch,decode,rename,dispatch,issue,execute,complete,retire"
```

The value is a comma-separated list in pipeline order (earliest first).
The viewer uses this ordering for Gantt chart column layout and
coloring. Stage names must match the values used in `stage_transition`
events (§5.1).

### 4.2 Buffer Storage Convention

Any storage with a field named `entity_id` of type `U32` is a buffer.

| Property   | Value                           |
| ---------- | ------------------------------- |
| Sparse     | yes (`SF_SPARSE`)               |
| Num slots  | hardware structure capacity     |

### 4.3 Required Buffer Fields

| Field name   | Type  | Description                     |
| ------------ | ----- | ------------------------------- |
| `entity_id`  | `U32` | References entity catalog slot  |

### 4.4 Optional Buffer Fields

The DUT may add structure-specific fields:

| Field name   | Type   | Description                       |
| ------------ | ------ | --------------------------------- |
| `completed`  | `BOOL` | Execution completed (ROB)         |
| `addr`       | `U64`  | Memory address (LQ/SQ)           |
| `ready`      | `BOOL` | Operands ready (IQ/scoreboard)    |
| `fu_type`    | `ENUM` | Functional unit assigned          |

### 4.5 Buffer Operations

```
Insert:  DA_SLOT_SET  rob[slot].entity_id = id
Remove:  DA_SLOT_CLEAR rob[slot]
Update:  DA_SLOT_SET  rob[slot].completed = 1
```

---

## 5. Standard Events

The protocol defines the following event names. `stage_transition` is
required for Gantt chart rendering; all others are optional. The viewer
renders recognized events with specific visualizations and unknown
events generically (name + fields in a tooltip).

### 5.1 `stage_transition`

Explicit pipeline stage change for an entity. The DUT emits this event
each time an instruction advances to a new pipeline stage. Superscalar
cores emit multiple `stage_transition` events in the same cycle frame
(e.g., a 4-wide machine retiring 4 instructions produces 4 events).

| Field name   | Type                      | Description              |
| ------------ | ------------------------- | ------------------------ |
| `entity_id`  | `U32`                     | Entity that advanced     |
| `stage`      | `ENUM(pipeline_stage)`    | Stage the entity entered |

The enum must be named `pipeline_stage` in the schema. Its values must
match the names declared in the `pipeline_stages` DUT property (§4.1).
For example:

| Value | Name       |
| ----- | ---------- |
| 0     | `fetch`    |
| 1     | `decode`   |
| 2     | `rename`   |
| 3     | `dispatch` |
| 4     | `issue`    |
| 5     | `execute`  |
| 6     | `complete` |
| 7     | `retire`   |

The enum is DUT-defined — an in-order core might have just
`fetch, decode, execute, memory, writeback`.

The viewer maintains a `current_stage` per entity. A Gantt bar for a
stage spans from the time the entity entered it until the time it
entered the next stage (or was cleared/flushed). Multi-cycle stages
(e.g., a long-latency divide in `execute`) require no special handling —
the entity simply stays in its current stage until the next
`stage_transition` event.

### 5.2 `annotate`

Free-text annotation attached to an entity.

| Field name   | Type         | Description                   |
| ------------ | ------------ | ----------------------------- |
| `entity_id`  | `U32`        | Target entity                 |
| `text`       | `STRING_REF` | Annotation text               |

Viewer: shows as a label on the entity's Gantt bar.

### 5.3 `dependency`

Data or structural dependency between two entities.

| Field name   | Type              | Description            |
| ------------ | ----------------- | ---------------------- |
| `src_id`     | `U32`             | Producer entity        |
| `dst_id`     | `U32`             | Consumer entity        |
| `dep_type`   | `ENUM(dep_type)`  | Dependency kind        |

Standard `dep_type` enum values:

| Value | Name           |
| ----- | -------------- |
| 0     | `raw`          |
| 1     | `war`          |
| 2     | `waw`          |
| 3     | `structural`   |

Viewer: draws an arrow from producer to consumer in the Gantt chart.

### 5.4 `flush`

Entity was squashed before retirement.

| Field name   | Type                  | Description    |
| ------------ | --------------------- | -------------- |
| `entity_id`  | `U32`                 | Flushed entity |
| `reason`     | `ENUM(flush_reason)`  | Cause          |

Standard `flush_reason` enum values:

| Value | Name              |
| ----- | ----------------- |
| 0     | `mispredict`      |
| 1     | `exception`       |
| 2     | `interrupt`       |
| 3     | `pipeline_clear`  |

Viewer: marks the entity's Gantt bar with a squash indicator.

### 5.5 `stall`

Pipeline stall (not tied to a specific entity).

| Field name   | Type                  | Description         |
| ------------ | --------------------- | ------------------- |
| `reason`     | `ENUM(stall_reason)`  | Stall cause         |

Standard `stall_reason` enum values are DUT-defined. Common examples:
`rob_full`, `iq_full`, `lq_full`, `sq_full`, `fetch_miss`,
`dcache_miss`, `frontend_stall`.

Viewer: renders a colored band on the timeline.

---

## 6. Counters

No special protocol convention beyond shape detection. A 1-slot,
non-sparse storage is a counter. The storage name is the counter label.

Common counters:

| Storage name     | Fields                          | Meaning                        |
| ---------------- | ------------------------------- | ------------------------------ |
| `committed_insns`| `count: U64`                    | Retired instructions           |
| `bp_misses`      | `count: U64`                    | Branch mispredictions          |
| `dcache_misses`  | `count: U64`                    | D-cache misses                 |
| `icache_misses`  | `count: U64`                    | I-cache misses                 |

Writer updates via `DA_SLOT_ADD`:
```
uscope_slot_add(w, STOR_COMMITTED_INSNS, 0, FIELD_COUNT, 4);  // retired 4 this cycle
```

---

## 7. Summary Fields

The protocol defines standard summary field names for mipmap rendering.
The viewer recognizes these and aggregates them appropriately.

| Field name        | Type  | Meaning                           |
| ----------------- | ----- | --------------------------------- |
| `committed`       | `U32` | Instructions committed in bucket  |
| `cycles_active`   | `U32` | Non-idle cycles in bucket         |
| `flushes`         | `U16` | Flush events in bucket            |
| `bp_misses`       | `U16` | Branch mispredictions in bucket   |

Per-buffer occupancy summaries use the naming pattern
`<storage_name>_occ` (e.g., `rob_occ`). The value is the sum of
occupancy samples in the bucket; divide by `cycles_active` for average.

DUT-specific summary fields are rendered as generic bar charts.

---

## 8. DUT Properties

Properties use the `cpu.` key prefix so they coexist with other
protocols in multi-protocol traces.

### 8.1 Required Properties

| Key                      | Description                              | Example           |
| ------------------------ | ---------------------------------------- | ----------------- |
| `dut_name`               | DUT instance name                        | `boom_core_0`     |
| `cpu.protocol_version`   | Version of the `cpu` protocol            | `1.0`             |
| `cpu.isa`                | Instruction set architecture             | `RV64GC`          |
| `cpu.pipeline_stages`    | Comma-separated stage names, in order    | `fetch,...,retire` |

### 8.2 Optional Properties

| Key                      | Description                              | Example           |
| ------------------------ | ---------------------------------------- | ----------------- |
| `cpu.fetch_width`        | Instructions fetched per cycle           | `4`               |
| `cpu.commit_width`       | Instructions retired per cycle           | `4`               |
| `cpu.elf_path`           | Path to ELF for disassembly              | `/path/to/fw.elf` |
| `cpu.vendor`             | DUT vendor                               | `sifive`          |

---

## 9. Viewer Reconstruction

### 9.1 Opening a Trace

1. Read preamble → parse schema and DUT properties
2. Walk scope tree from root `/` → find all scopes with
   `protocol = "cpu"`; each is a core
3. Per core scope: identify `entities` storage (entity catalog), find
   all buffers (storages with `entity_id` field), identify counters
   (1-slot non-sparse storages)
4. Read `cpu.pipeline_stages` property → build ordered stage list
5. If `cpu.elf_path` property exists, load ELF for disassembly

### 9.2 Gantt Chart Rendering

For a time range `[T0, T1)` in picoseconds:

1. Seek to segment covering `T0` (binary search or chain walk)
2. Load checkpoint → initial state of all storages
3. Replay deltas and events `T0..T1`, tracking per-entity:
   - **Birth**: entity slot becomes valid in `entities`
   - **Stage transitions**: `stage_transition` event → record
     `(entity_id, stage, timestamp)`
   - **Death**: entity slot cleared in `entities` (retire or flush)
4. For each entity, emit Gantt bars: each stage spans from its
   `stage_transition` timestamp until the next transition (or death)
5. Entity labels: read `pc` and `inst_bits` from entity catalog,
   decode via ISA disassembler
6. Dependency arrows: `dependency` events in the range
7. Flush markers: `flush` events in the range
8. Convert timestamps to domain-local cycle numbers for display
   using the scope's clock domain period

### 9.3 Occupancy View

For each buffer, count valid slots per cycle. The mipmap summary
(`<name>_occ` fields) gives this at coarse granularity; delta replay
gives exact per-cycle values when zoomed in.

### 9.4 Counter Graphs

Read counter storages at each cycle frame (via `DA_SLOT_ADD` deltas).
Compute rates (delta / cycles) for display. Mipmap summaries provide
pre-aggregated values for zoomed-out views.

---

## 10. Example: BOOM-like OoO Core

### 10.1 DUT Properties

```
dut_name              = "boom_tile0_core0"
cpu.isa               = "RV64GC"
cpu.fetch_width       = "4"
cpu.commit_width      = "4"
cpu.elf_path          = "/workspace/fw.elf"
cpu.pipeline_stages   = "fetch,decode,rename,dispatch,issue,execute,complete,retire"
```

### 10.2 Schema

```
Scopes:
  /       (id=0, root,      protocol=none)
  core0   (id=1, parent=0,  protocol="cpu")

Enums:
  pipeline_stage: fetch(0), decode(1), rename(2), dispatch(3),
                  issue(4), execute(5), complete(6), retire(7)
  dep_type:       raw(0), war(1), waw(2), structural(3)
  flush_reason:   mispredict(0), exception(1), interrupt(2)
  stall_reason:   rob_full(0), iq_full(1), lq_full(2), sq_full(3),
                  fetch_miss(4), dcache_miss(5)

Storages (all scope=core0):
  entities    (sparse, 512 slots):  entity_id:U32, pc:U64, inst_bits:U32
  rob         (sparse, 256 slots):  entity_id:U32, completed:BOOL
  iq_int      (sparse, 48 slots):   entity_id:U32
  iq_fp       (sparse, 32 slots):   entity_id:U32
  iq_mem      (sparse, 48 slots):   entity_id:U32
  lq          (sparse, 32 slots):   entity_id:U32, addr:U64
  sq          (sparse, 32 slots):   entity_id:U32, addr:U64
  committed   (dense, 1 slot):      count:U64
  bp_misses   (dense, 1 slot):      count:U64

Events (all scope=core0):
  stage_transition: entity_id:U32, stage:ENUM(pipeline_stage)
  annotate:         entity_id:U32, text:STRING_REF
  dependency:       src_id:U32, dst_id:U32, type:ENUM(dep_type)
  flush:            entity_id:U32, reason:ENUM(flush_reason)
  stall:            reason:ENUM(stall_reason)
```

Note: transient stages (fetch, decode, execute, etc.) are modeled
purely via `stage_transition` events — no storages needed. Only
physical structures that hold entities (ROB, IQ, LQ, SQ) are storages.

### 10.3 Example: 5-Stage In-Order Core

Same protocol, minimal schema:

```
DUT properties:
  cpu.pipeline_stages  = "fetch,decode,execute,memory,writeback"

Scopes:
  /       (id=0, root,      protocol=none)
  core0   (id=1, parent=0,  protocol="cpu")

Enums:
  pipeline_stage: fetch(0), decode(1), execute(2), memory(3), writeback(4)

Storages (all scope=core0):
  entities    (sparse, 8 slots):    entity_id:U32, pc:U64, inst_bits:U32
  committed   (dense, 1 slot):      count:U64

Events (all scope=core0):
  stage_transition: entity_id:U32, stage:ENUM(pipeline_stage)
```

An in-order core may have no buffers at all — just the entity catalog
and stage transitions. The viewer renders a Gantt chart purely from
events.

### 10.4 Example: Dual-Core SoC

Multi-core uses transport-level scopes (§4.4 of the transport spec).
Each core is a scope with `protocol = "cpu"`. Storages and event types
are defined per-scope, so entity IDs are per-core and no `core_id`
field is needed in event payloads.

```
DUT properties:
  dut_name              = "my_soc"
  cpu.pipeline_stages   = "fetch,decode,rename,dispatch,issue,execute,complete,retire"
  cpu.isa               = "RV64GC"
  cpu.elf_path          = "/workspace/fw.elf"

Scopes:
  /            (id=0, root,         protocol=none)
  cpu_cluster  (id=1, parent=0,     protocol=none)
  core0        (id=2, parent=1,     protocol="cpu")
  core1        (id=3, parent=1,     protocol="cpu")

Enums (shared):
  pipeline_stage: fetch(0), decode(1), rename(2), dispatch(3),
                  issue(4), execute(5), complete(6), retire(7)

Storages:
  entities  (scope=core0, sparse, 512):  entity_id:U32, pc:U64, inst_bits:U32
  rob       (scope=core0, sparse, 256):  entity_id:U32
  committed (scope=core0, dense, 1):     count:U64

  entities  (scope=core1, sparse, 512):  entity_id:U32, pc:U64, inst_bits:U32
  rob       (scope=core1, sparse, 256):  entity_id:U32
  committed (scope=core1, dense, 1):     count:U64

Events:
  stage_transition (scope=core0): entity_id:U32, stage:ENUM(pipeline_stage)
  stage_transition (scope=core1): entity_id:U32, stage:ENUM(pipeline_stage)
  flush            (scope=core0): entity_id:U32, reason:ENUM(flush_reason)
  flush            (scope=core1): entity_id:U32, reason:ENUM(flush_reason)
```

The viewer finds all scopes with `protocol = "cpu"`, renders a
per-core pipeline view for each, and can show them side-by-side.

Storage names (`entities`, `rob`) repeat across scopes — the
`storage_id` is globally unique, but the name + scope combination
gives the viewer the display path (`core0/entities`, `core1/rob`).

Cross-core events (cache coherence, IPIs) can be defined at the
`cpu_cluster` scope with fields referencing the relevant scope IDs.

---

## 11. Version History

| Version | Date       | Changes         |
| ------- | ---------- | --------------- |
| 1.0     | 2026-xx-xx | Initial draft   |
