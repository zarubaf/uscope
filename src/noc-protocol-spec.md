# µScope `noc` Protocol Specification

**Version:** 0.1-draft
**Protocol identifier:** `noc`
**Transport version:** µScope 0.x

---

## 1. Overview

The `noc` protocol defines conventions for tracing any on-chip
interconnect — crossbar, mesh, ring, tree, or point-to-point — using
the µScope transport layer. It works with any bus protocol: AXI4, CHI,
ACE, TileLink, UCIe, or proprietary fabrics.

Like the `cpu` protocol, it does not prescribe a fixed schema. Instead,
it defines **semantic conventions** that a DUT writer follows and a
viewer relies on to render transaction Gantt charts, topology maps,
latency histograms, and traffic heatmaps without prior knowledge of the
specific interconnect microarchitecture.

### 1.1 Design Principles

1. **Generic over specific.** The protocol works for a single-port AXI
   crossbar and a 64-node CHI mesh alike. The DUT declares its
   structures; the viewer renders whatever it finds.

2. **Convention over configuration.** Semantics are conveyed through
   field names, storage shapes, and scope properties — not through
   protocol-specific binary metadata.

3. **Entity-centric.** Every in-flight transaction has a unique ID in a
   transaction catalog. All buffers, events, and stages reference
   transactions by this ID. The viewer joins on it to build
   per-transaction timelines.

4. **Topology-agnostic.** The protocol does not encode topology in the
   data model. Topology is declared via scope properties; the viewer
   uses it for visualization only.

---

## 2. Concepts

### 2.1 Transactions (Entities)

A **transaction** is an in-flight bus operation (read, write, snoop,
etc.). Each transaction occupies a slot in the **transaction catalog**
storage and is referenced by its slot index throughout the interconnect.

- **Transaction ID** = slot index in the transaction catalog (`U32`).
- When a transaction is issued, the writer allocates a slot
  (`DA_SLOT_SET` on its fields). When it completes, the writer clears
  the slot (`DA_SLOT_CLEAR`). The slot can then be reused.
- The transaction catalog must be **sparse**.

Transactions in the `noc` protocol are the direct analogue of entities
in the `cpu` protocol (cpu spec §2.1).

### 2.2 Buffers

A **buffer** is any storage whose slots hold transaction references — a
hardware structure that transactions pass through or reside in.
Examples: virtual channel (VC) buffers, reorder buffers, outstanding
request tables, credit pools.

A storage is recognized as a buffer if it contains a field named
`txn_id` (§3.2). The viewer automatically tracks transaction membership
in every buffer.

### 2.3 Stages

The viewer renders a per-transaction **Gantt chart** showing which
pipeline stage each transaction is in over time. Since a transaction can
occupy multiple buffers simultaneously (e.g., outstanding request table
+ VC buffer + arbitrating), stage progression is tracked **explicitly**
via `stage_transition` events (§5.1), not inferred from buffer
membership.

Buffers and stages are orthogonal:
- **Buffers** model where a transaction physically resides (VC slot 3,
  ROB entry 7). A transaction can be in multiple buffers at once.
- **Stages** model logical progression through the interconnect (issue
  → route → arbitrate → traverse → deliver → respond). A transaction is
  in exactly one stage at any time.

The DUT declares the stage ordering via `noc.pipeline_stages` (§4.1)
and emits a `stage_transition` event each time a transaction advances.
The viewer maintains a `current_stage` per transaction and draws Gantt
bars from stage entry/exit times.

### 2.4 Counters

A **counter** is a 1-slot, non-sparse storage with numeric fields,
mutated via `DA_SLOT_ADD`. The viewer infers counters from this shape
and renders them as line graphs or sparklines. No protocol markup is
needed.

### 2.5 Events

Events model instantaneous occurrences attached to transactions or to
the timeline. The protocol defines standard event names (§5). The
viewer renders recognized events with specific visualizations and
unknown events generically.

### 2.6 Router Sub-Scopes

For multi-router interconnects, each router can be a **child scope**
with `protocol="noc.router"`. This enables per-router buffers, counters,
and events while keeping the transaction catalog on the nearest ancestor
`noc` scope.

```
/                     (protocol=none)
  noc0                (protocol="noc")        ← transaction catalog here
    router_0_0        (protocol="noc.router") ← per-router buffers/counters
    router_0_1        (protocol="noc.router")
    router_1_0        (protocol="noc.router")
    router_1_1        (protocol="noc.router")
```

A `noc.router` scope does **not** have its own transaction catalog. It
references transactions from the parent `noc` scope's catalog via the
`txn_id` field. The viewer resolves `txn_id` by walking up the scope
tree to the nearest `noc` scope.

### 2.7 Cross-Scope Transaction Handoff

When a transaction crosses a scope boundary — e.g., a chiplet-to-chiplet
transfer via a D2D link, or a protocol bridge (AXI→CHI) — it receives
a new `txn_id` in the destination scope. The `txn_handoff` event (§5.7)
stitches the two identities together, enabling end-to-end latency
tracking across scope boundaries.

The `txn_handoff` event is emitted at a **common ancestor scope** of the
source and destination scopes. The viewer joins on these events to build
cross-scope transaction timelines.

---

## 3. Transaction Catalog

### 3.1 Storage Convention

The transaction catalog is a storage named `transactions`.

| Property   | Value                                        |
| ---------- | -------------------------------------------- |
| Name       | `transactions`                               |
| Sparse     | yes (`SF_SPARSE`)                            |
| Num slots  | max concurrent in-flight transactions (DUT-specific) |

### 3.2 Required Fields

| Field name  | Type   | Description                                    |
| ----------- | ------ | ---------------------------------------------- |
| `txn_id`    | `U32`  | Unique transaction ID (equals the slot index)  |
| `opcode`    | `ENUM` | Transaction type (read, write, snoop, etc.)    |
| `addr`      | `U64`  | Target address                                 |
| `len`       | `U16`  | Burst length (number of beats)                 |
| `size`      | `U8`   | Beat size (log2 bytes, e.g., 3 = 8 bytes)      |
| `src_port`  | `U16`  | Source port / initiator ID                     |
| `dst_port`  | `U16`  | Destination port / target ID                   |

### 3.3 Optional Fields

The DUT may add any additional fields. Common examples:

| Field name   | Type   | Description                                  |
| ------------ | ------ | -------------------------------------------- |
| `qos`        | `U8`   | Quality-of-service priority                  |
| `txn_class`  | `ENUM` | Transaction class (posted, non-posted, etc.) |
| `prot`       | `U8`   | Protection bits (privileged, secure, etc.)   |
| `cache`      | `U8`   | Cache allocation hints                       |
| `snoop`      | `U8`   | Snoop attribute bits                         |
| `domain`     | `ENUM` | Shareability domain                          |
| `excl`       | `BOOL` | Exclusive access flag                        |
| `tag`        | `U16`  | Transaction tag (for reorder tracking)       |

### 3.4 Transaction Lifecycle

```
Issue:      DA_SLOT_SET  transactions[id].txn_id = id
            DA_SLOT_SET  transactions[id].opcode = ...
            DA_SLOT_SET  transactions[id].addr = ...
            DA_SLOT_SET  transactions[id].len = ...
            DA_SLOT_SET  transactions[id].size = ...
            DA_SLOT_SET  transactions[id].src_port = ...
            DA_SLOT_SET  transactions[id].dst_port = ...

Complete:   DA_SLOT_CLEAR transactions[id]
```

The `txn_id` field is always equal to the slot index. It is stored
explicitly so that buffer storages and events can reference it using a
uniform `U32` field, independent of the transport's slot indexing.

---

## 4. Buffers and Stages

### 4.1 Stage Ordering via Scope Properties

Each `noc` scope declares pipeline stages using a scope property:

```
noc.pipeline_stages = "issue,route,arbitrate,traverse,deliver,respond"
```

The value is a comma-separated list in pipeline order (earliest first).
The viewer uses this ordering for Gantt chart column layout and
coloring. Stage names must match the values used in `stage_transition`
events (§5.1). Each `noc` scope declares its own stages, enabling
heterogeneous interconnects in the same trace.

### 4.2 Buffer Storage Convention

Any storage with a field named `txn_id` of type `U32` is a buffer.

| Property   | Value                           |
| ---------- | ------------------------------- |
| Sparse     | yes (`SF_SPARSE`)               |
| Num slots  | hardware structure capacity     |

### 4.3 Required Buffer Fields

| Field name | Type  | Description                           |
| ---------- | ----- | ------------------------------------- |
| `txn_id`   | `U32` | References transaction catalog slot   |

### 4.4 Optional Buffer Fields

The DUT may add structure-specific fields:

| Field name   | Type   | Description                            |
| ------------ | ------ | -------------------------------------- |
| `vc`         | `U8`   | Virtual channel assignment             |
| `priority`   | `U8`   | Arbitration priority                   |
| `flit_type`  | `ENUM` | Flit type (header, data, tail)         |
| `credits`    | `U8`   | Available credits                      |

### 4.5 Buffer Operations

```
Insert:  DA_SLOT_SET  vc_buf[slot].txn_id = id
Remove:  DA_SLOT_CLEAR vc_buf[slot]
Update:  DA_SLOT_SET  vc_buf[slot].credits = 3
```

### 4.6 Common Buffers

| Buffer name       | Models                                         |
| ----------------- | ---------------------------------------------- |
| `vc_buf_<port>`   | Per-port virtual channel buffer                |
| `rob`             | Reorder buffer for out-of-order completion     |
| `ort`             | Outstanding request table / tracker            |
| `snoop_filter`    | Snoop filter entries                           |
| `retry_buf`       | Transactions awaiting retry                    |

### 4.7 Example Stage Sets

**AXI4 crossbar:**
```
noc.pipeline_stages = "ar_issue,route,arbitrate,transport,target_accept,r_data,r_last"
```

**CHI mesh:**
```
noc.pipeline_stages = "req_issue,req_accept,snoop_send,snoop_resp,dat_transfer,comp_ack"
```

**TileLink ring:**
```
noc.pipeline_stages = "acquire,route,grant,grant_ack"
```

---

## 5. Standard Events

The protocol defines the following event names. `stage_transition` is
required for Gantt chart rendering; all others are optional. The viewer
renders recognized events with specific visualizations and unknown
events generically (name + fields in a tooltip).

### 5.1 `stage_transition`

Explicit stage change for a transaction. The DUT emits this event each
time a transaction advances to a new pipeline stage.

| Field name | Type                     | Description                     |
| ---------- | ------------------------ | ------------------------------- |
| `txn_id`   | `U32`                    | Transaction that advanced       |
| `stage`    | `ENUM(pipeline_stage)`   | Stage the transaction entered   |

The `pipeline_stage` enum values must match the names declared in the
`noc.pipeline_stages` scope property (§4.1). For example (AXI4):

| Value | Name             |
| ----- | ---------------- |
| 0     | `ar_issue`       |
| 1     | `route`          |
| 2     | `arbitrate`      |
| 3     | `transport`      |
| 4     | `target_accept`  |
| 5     | `r_data`         |
| 6     | `r_last`         |

The enum is DUT-defined — a simple crossbar might have just
`issue, arbitrate, transfer, complete`.

The viewer maintains a `current_stage` per transaction. A Gantt bar for
a stage spans from the cycle the transaction entered it until the cycle
it entered the next stage (or was cleared).

### 5.2 `beat`

Individual data beat in a burst transfer.

| Field name   | Type   | Description                            |
| ------------ | ------ | -------------------------------------- |
| `txn_id`     | `U32`  | Parent transaction                     |
| `beat_num`   | `U16`  | Beat number within burst (0-based)     |
| `data_bytes` | `U16`  | Bytes transferred in this beat         |

Viewer: shows beat markers on the transaction's Gantt bar during the
data transfer stage. Useful for identifying partial transfers and
stalls between beats.

### 5.3 `retry`

Transaction retry — the target or interconnect rejected the
transaction and it must be re-attempted.

| Field name | Type                  | Description             |
| ---------- | --------------------- | ----------------------- |
| `txn_id`   | `U32`                 | Retried transaction     |
| `reason`   | `ENUM(retry_reason)`  | Cause of retry          |

Standard `retry_reason` enum values:

| Value | Name              |
| ----- | ----------------- |
| 0     | `target_busy`     |
| 1     | `no_credits`      |
| 2     | `vc_full`         |
| 3     | `arb_lost`        |
| 4     | `protocol_retry`  |

Viewer: marks a retry indicator on the transaction's Gantt bar.

### 5.4 `timeout`

Watchdog timeout — a transaction exceeded the expected completion time.

| Field name       | Type   | Description                         |
| ---------------- | ------ | ----------------------------------- |
| `txn_id`         | `U32`  | Timed-out transaction               |
| `threshold_cycles` | `U32` | Watchdog threshold that was exceeded |

Viewer: marks a timeout indicator on the transaction's Gantt bar and
highlights it in the topology view.

### 5.5 `link_credit`

Credit flow control update on a link.

| Field name | Type                        | Description                    |
| ---------- | --------------------------- | ------------------------------ |
| `port`     | `U16`                       | Port ID                        |
| `direction`| `ENUM(credit_direction)`    | Credit grant or consume        |
| `credits`  | `U8`                        | Number of credits              |

Standard `credit_direction` enum values:

| Value | Name      |
| ----- | --------- |
| 0     | `grant`   |
| 1     | `consume` |

Viewer: renders credit level as a per-port sparkline.

### 5.6 `arb_decision`

Arbitration outcome — records which transaction won arbitration at a
port.

| Field name      | Type   | Description                      |
| --------------- | ------ | -------------------------------- |
| `winner_txn`    | `U32`  | Transaction that won arbitration |
| `port`          | `U16`  | Port where arbitration occurred  |
| `num_contenders`| `U8`   | Number of competing transactions |

Viewer: shows arbitration events in the timeline. High `num_contenders`
values indicate congestion hotspots.

### 5.7 `txn_handoff`

Cross-scope transaction stitching — links a transaction in one scope
to its continuation in another scope.

| Field name   | Type   | Description                                    |
| ------------ | ------ | ---------------------------------------------- |
| `src_scope`  | `U16`  | Scope ID of the source transaction             |
| `src_txn_id` | `U32`  | Transaction ID in the source scope             |
| `dst_scope`  | `U16`  | Scope ID of the destination transaction        |
| `dst_txn_id` | `U32`  | Transaction ID in the destination scope        |

This event is emitted at a **common ancestor scope** of `src_scope` and
`dst_scope`. It enables end-to-end latency tracking across chiplet
boundaries, protocol bridges, or any other scope boundary where a
transaction receives a new identity.

Viewer: draws a handoff arrow between the two transaction timelines
and computes end-to-end latency by joining the linked transactions.

### 5.8 `annotate`

Free-text annotation attached to a transaction.

| Field name | Type         | Description            |
| ---------- | ------------ | ---------------------- |
| `txn_id`   | `U32`        | Target transaction     |
| `text`     | `STRING_REF` | Annotation text        |

Viewer: shows as a label on the transaction's Gantt bar.

---

## 6. Counters

No special protocol convention beyond shape detection. A 1-slot,
non-sparse storage is a counter. The storage name is the counter label.

Common counters:

| Storage name       | Fields       | Meaning                                 |
| ------------------ | ------------ | --------------------------------------- |
| `bytes_tx`         | `count: U64` | Bytes transmitted                       |
| `bytes_rx`         | `count: U64` | Bytes received                          |
| `arb_conflicts`    | `count: U64` | Arbitration conflicts (>1 contender)    |
| `retries`          | `count: U64` | Transaction retries                     |
| `txn_completed`    | `count: U64` | Transactions completed                  |

Writer updates via `DA_SLOT_ADD`:
```
uscope_slot_add(w, STOR_BYTES_TX, 0, FIELD_COUNT, 64);  // 64 bytes this cycle
```

For per-router counters, place the counter storage on the router's
sub-scope (§2.6).

---

## 7. Summary Fields

The protocol defines standard summary field names for mipmap rendering.
Each summary field is scoped to its `noc` scope (via `scope_id` in
`summary_field_def_t`), so multi-interconnect traces have independent
summaries without name collisions.

| Field name          | Type  | Meaning                              |
| ------------------- | ----- | ------------------------------------ |
| `txn_completed`     | `U32` | Transactions completed in bucket     |
| `bytes_transferred` | `U64` | Total bytes transferred in bucket    |
| `avg_latency_ticks` | `U32` | Average transaction latency in bucket|
| `retries`           | `U16` | Retry events in bucket               |

Per-buffer occupancy summaries use the naming pattern
`<storage_name>_occ` (e.g., `vc_buf_0_occ`). The value is the sum of
occupancy samples in the bucket; divide by active cycles for average.

DUT-specific summary fields are rendered as generic bar charts.

---

## 8. Scope Properties

Properties are stored on each scope (transport spec §3.4.1). The `noc`
protocol uses the `noc.` key prefix. Each `noc` scope carries its own
properties, enabling heterogeneous interconnects in the same trace.

Properties that describe the overall trace (e.g., `dut_name`) belong
on the root scope.

### 8.1 Required Properties (on each `noc` scope)

| Key                       | Description                               | Example                                          |
| ------------------------- | ----------------------------------------- | ------------------------------------------------ |
| `noc.protocol_version`    | Version of the `noc` protocol             | `0.1`                                            |
| `noc.bus_protocol`        | Underlying bus protocol                   | `AXI4`, `CHI`, `TileLink`, `UCIe`                |
| `noc.topology`            | Interconnect topology                     | `crossbar`, `mesh`, `ring`, `tree`, `p2p`        |
| `noc.pipeline_stages`     | Comma-separated stage names, in order     | `issue,route,arbitrate,traverse,deliver,respond` |
| `clock.period_ps`         | Clock period in picoseconds               | `1000` (1 GHz)                                   |

### 8.2 Optional Properties (on each `noc` scope)

| Key                       | Description                               | Example     |
| ------------------------- | ----------------------------------------- | ----------- |
| `noc.dim_x`              | Mesh X dimension                           | `4`         |
| `noc.dim_y`              | Mesh Y dimension                           | `4`         |
| `noc.num_vcs`            | Number of virtual channels per port        | `4`         |
| `noc.data_width`         | Data bus width in bits                     | `128`       |
| `noc.addr_width`         | Address bus width in bits                  | `48`        |
| `noc.num_ports`          | Total number of ports                      | `16`        |
| `noc.routing`            | Routing algorithm                          | `xy`, `adaptive` |

### 8.3 Root Scope Properties

| Key          | Description               | Example    |
| ------------ | ------------------------- | ---------- |
| `dut_name`   | DUT instance name         | `my_soc`   |
| `vendor`     | DUT vendor (top-level)    | `acme`     |

---

## 9. Viewer Reconstruction

### 9.1 Opening a Trace

1. Read preamble → parse schema (including scope properties)
2. Walk scope tree from root `/` → find all scopes with
   `protocol = "noc"`; each is an interconnect instance
3. Per `noc` scope:
   a. Read scope properties → `noc.pipeline_stages`, `noc.bus_protocol`,
      `noc.topology`, etc.
   b. Identify `transactions` storage (transaction catalog)
   c. Find all buffers (storages with `txn_id` field)
   d. Identify counters (1-slot non-sparse storages)
   e. Find child scopes with `protocol = "noc.router"` for per-router
      detail
4. Per `noc` scope: build ordered stage list from
   `noc.pipeline_stages`
5. If `noc.topology = "mesh"`, read `noc.dim_x` and `noc.dim_y` for
   topology rendering

### 9.2 Transaction Gantt Chart

For a cycle range `[C0, C1)`:

1. Seek to segment covering `C0` (binary search or chain walk)
2. Load checkpoint → initial state of all storages
3. Replay deltas and events `C0..C1`, tracking per-transaction:
   - **Birth**: transaction slot becomes valid in `transactions`
   - **Stage transitions**: `stage_transition` event → record
     `(txn_id, stage, cycle)`
   - **Death**: transaction slot cleared in `transactions` (completion)
4. For each transaction, emit Gantt bars: each stage spans from its
   `stage_transition` cycle until the next transition (or death)
5. Transaction labels: read `opcode`, `addr`, `src_port`, `dst_port`
   from the transaction catalog
6. Retry markers: `retry` events in the range
7. Beat markers: `beat` events in the range
8. Timeout markers: `timeout` events in the range

### 9.3 Topology View

Using the `noc.topology` scope property and `src_port`/`dst_port` fields
from the transaction catalog:

1. Render the interconnect topology (mesh grid, ring, tree, etc.)
2. Animate transaction flow by mapping `stage_transition` events to
   router positions
3. Color links by utilization (bytes per cycle / data width)
4. Highlight congestion hotspots using `arb_decision` contention data

For mesh topologies, map port IDs to (x, y) coordinates using
`noc.dim_x` and `noc.dim_y`.

### 9.4 Latency Histogram

Compute per-transaction latency from birth-to-death ticks in the
`transactions` catalog. Group by `opcode`, `src_port`, `dst_port`, or
address range for drill-down analysis.

### 9.5 Cross-Scope Stitching

1. Find `txn_handoff` events across all `noc` scopes
2. Join `(src_scope, src_txn_id)` to `(dst_scope, dst_txn_id)`
3. Build end-to-end transaction timelines spanning multiple scopes
4. Compute end-to-end latency by summing per-scope stage durations

### 9.6 Occupancy View

For each buffer, count valid slots per cycle. The mipmap summary
(`<name>_occ` fields) gives this at coarse granularity; delta replay
gives exact per-cycle values when zoomed in.

### 9.7 Counter Graphs

Read counter storages at each cycle frame (via `DA_SLOT_ADD` deltas).
Compute rates (delta / cycles) for display. Mipmap summaries provide
pre-aggregated values for zoomed-out views.

---

## 10. Examples

### 10.1 AXI4 Crossbar

A simple single-scope NoC tracing an AXI4 crossbar with 4 initiator
ports and 2 target ports.

```
Scopes:
  /           (id=0, root,      protocol=none)
    properties: dut_name="axi_xbar"
  noc0        (id=1, parent=0,  protocol="noc")
    properties: noc.protocol_version="0.1", noc.bus_protocol="AXI4",
                noc.topology="crossbar", noc.data_width="64",
                noc.num_ports="6", clock.period_ps="1000",
                noc.pipeline_stages="ar_issue,route,arbitrate,transport,target_accept,r_data,r_last"

Enums:
  opcode:         read(0), write(1), read_linked(2), write_cond(3)
  pipeline_stage: ar_issue(0), route(1), arbitrate(2), transport(3),
                  target_accept(4), r_data(5), r_last(6)
  retry_reason:   target_busy(0), no_credits(1), arb_lost(2)
  credit_direction: grant(0), consume(1)

Storages (all scope=noc0):
  transactions  (sparse, 64 slots):   txn_id:U32, opcode:ENUM(opcode), addr:U64,
                                      len:U16, size:U8, src_port:U16, dst_port:U16,
                                      qos:U8
  ort           (sparse, 32 slots):   txn_id:U32
  bytes_tx      (dense, 1 slot):      count:U64
  bytes_rx      (dense, 1 slot):      count:U64
  arb_conflicts (dense, 1 slot):      count:U64
  txn_completed (dense, 1 slot):      count:U64

Events (all scope=noc0):
  stage_transition: txn_id:U32, stage:ENUM(pipeline_stage)
  beat:             txn_id:U32, beat_num:U16, data_bytes:U16
  retry:            txn_id:U32, reason:ENUM(retry_reason)
  arb_decision:     winner_txn:U32, port:U16, num_contenders:U8
  link_credit:      port:U16, direction:ENUM(credit_direction), credits:U8
  annotate:         txn_id:U32, text:STRING_REF
```

### 10.2 CHI Mesh NoC

A 4x4 CHI mesh with per-router sub-scopes. The transaction catalog
lives on the parent `noc` scope; router sub-scopes hold local buffers
and counters.

```
Scopes:
  /                 (id=0,  root,       protocol=none)
    properties: dut_name="chi_mesh_soc"
  noc0              (id=1,  parent=0,   protocol="noc")
    properties: noc.protocol_version="0.1", noc.bus_protocol="CHI",
                noc.topology="mesh", noc.dim_x="4", noc.dim_y="4",
                noc.num_vcs="4", noc.data_width="256",
                clock.period_ps="500",
                noc.pipeline_stages="req_issue,req_accept,snoop_send,snoop_resp,dat_transfer,comp_ack"
  router_0_0        (id=2,  parent=1,   protocol="noc.router")
  router_0_1        (id=3,  parent=1,   protocol="noc.router")
  ...
  router_3_3        (id=17, parent=1,   protocol="noc.router")

Enums:
  opcode:         read_no_snp(0), read_once(1), read_shared(2), read_unique(3),
                  write_no_snp(4), write_unique(5), snoop_shared(6),
                  snoop_unique(7), comp_data(8), comp_ack(9)
  pipeline_stage: req_issue(0), req_accept(1), snoop_send(2),
                  snoop_resp(3), dat_transfer(4), comp_ack(5)
  retry_reason:   target_busy(0), no_credits(1), vc_full(2),
                  arb_lost(3), protocol_retry(4)
  txn_class:      req(0), snp(1), dat(2), rsp(3)

Storages (scope=noc0):
  transactions  (sparse, 256 slots):  txn_id:U32, opcode:ENUM(opcode), addr:U64,
                                      len:U16, size:U8, src_port:U16, dst_port:U16,
                                      qos:U8, txn_class:ENUM(txn_class)

Storages (scope=router_0_0, one set per router):
  vc_buf_n      (sparse, 4 slots):    txn_id:U32, vc:U8
  vc_buf_s      (sparse, 4 slots):    txn_id:U32, vc:U8
  vc_buf_e      (sparse, 4 slots):    txn_id:U32, vc:U8
  vc_buf_w      (sparse, 4 slots):    txn_id:U32, vc:U8
  vc_buf_local  (sparse, 4 slots):    txn_id:U32, vc:U8
  bytes_fwd     (dense, 1 slot):      count:U64
  arb_conflicts (dense, 1 slot):      count:U64

Events (scope=noc0):
  stage_transition: txn_id:U32, stage:ENUM(pipeline_stage)
  retry:            txn_id:U32, reason:ENUM(retry_reason)
  annotate:         txn_id:U32, text:STRING_REF

Events (scope=router_0_0, one set per router):
  arb_decision:     winner_txn:U32, port:U16, num_contenders:U8
  link_credit:      port:U16, direction:ENUM(credit_direction), credits:U8
```

The viewer discovers all 16 routers as `noc.router` children of `noc0`,
maps them to a 4x4 grid via `noc.dim_x`/`noc.dim_y`, and renders
per-router buffer occupancy alongside the global transaction Gantt chart.

### 10.3 Multi-Chiplet with D2D

Two chiplets connected via a UCIe D2D link. Each chiplet has its own
`noc` scope with an independent transaction catalog. The `txn_handoff`
event on the SoC-level scope stitches transactions across the link.

```
Scopes:
  /                       (id=0, root,       protocol=none)
    properties: dut_name="multi_chiplet_soc"
  chiplet0                (id=1, parent=0,   protocol=none)
  chiplet0_noc            (id=2, parent=1,   protocol="noc")
    properties: noc.protocol_version="0.1", noc.bus_protocol="CHI",
                noc.topology="mesh", noc.dim_x="4", noc.dim_y="4",
                noc.pipeline_stages="req_issue,req_accept,dat_transfer,comp_ack",
                clock.period_ps="500"
  chiplet1                (id=3, parent=0,   protocol=none)
  chiplet1_noc            (id=4, parent=3,   protocol="noc")
    properties: noc.protocol_version="0.1", noc.bus_protocol="CHI",
                noc.topology="mesh", noc.dim_x="2", noc.dim_y="2",
                noc.pipeline_stages="req_issue,req_accept,dat_transfer,comp_ack",
                clock.period_ps="500"
  d2d_link                (id=5, parent=0,   protocol="noc")
    properties: noc.protocol_version="0.1", noc.bus_protocol="UCIe",
                noc.topology="p2p",
                noc.pipeline_stages="d2d_issue,phy_encode,link_traverse,phy_decode,d2d_deliver",
                clock.period_ps="500"

Storages:
  transactions (scope=chiplet0_noc, sparse, 256): txn_id:U32, opcode:ENUM, addr:U64,
                                                   len:U16, size:U8, src_port:U16, dst_port:U16
  transactions (scope=chiplet1_noc, sparse, 128): txn_id:U32, opcode:ENUM, addr:U64,
                                                   len:U16, size:U8, src_port:U16, dst_port:U16
  transactions (scope=d2d_link, sparse, 32):      txn_id:U32, opcode:ENUM, addr:U64,
                                                   len:U16, size:U8, src_port:U16, dst_port:U16

Events (scope=root):
  txn_handoff:  src_scope:U16, src_txn_id:U32, dst_scope:U16, dst_txn_id:U32
```

**Handoff sequence for a cross-chiplet read:**

1. Chiplet 0 issues a read → `transactions[42]` in `chiplet0_noc`
2. The read reaches the D2D egress port → `DA_SLOT_CLEAR` on
   `chiplet0_noc.transactions[42]`
3. D2D link picks it up → `transactions[7]` in `d2d_link`
4. Root scope emits `txn_handoff(src_scope=2, src_txn_id=42,
   dst_scope=5, dst_txn_id=7)`
5. D2D link delivers to chiplet 1 → `DA_SLOT_CLEAR` on
   `d2d_link.transactions[7]`
6. Chiplet 1 ingests the read → `transactions[19]` in `chiplet1_noc`
7. Root scope emits `txn_handoff(src_scope=5, src_txn_id=7,
   dst_scope=4, dst_txn_id=19)`
8. The viewer chains: `chiplet0_noc:42 → d2d_link:7 → chiplet1_noc:19`
   and computes end-to-end latency

---

## 11. Version History

| Version | Date       | Changes         |
| ------- | ---------- | --------------- |
| 0.1     | 2026-xx-xx | Initial draft   |
