# uscope-cli: Command-Line Trace Inspector

**Binary:** `uscope-cli`
**Location:** `crates/uscope-cli/`

---

## Overview

`uscope-cli` is a standalone command-line tool for inspecting µScope CPU pipeline traces. It provides quick access to trace metadata, buffer state, instruction timelines, and counter data without needing the Reflex GUI.

All commands support `--json` for structured JSON output, making it suitable for scripting and CI pipelines.

---

## Installation

```bash
cargo install --path crates/uscope-cli
# or run directly:
cargo run --bin uscope-cli -- <command> <file>
```

---

## Commands

### `info` — File overview

```bash
uscope-cli info trace.uscope
```

Prints: file header (version, flags, segments, duration), metadata (DUT properties), pipeline stage names, counter names, buffer names, and full schema dump (storages, events, enums).

```bash
# JSON output for scripting
uscope-cli info trace.uscope --json | jq '.counters'
```

### `state` — Buffer state at a cycle

```bash
uscope-cli state trace.uscope --cycle 50
```

Shows the state of all buffers at the given cycle: occupied slots with field values, entity fields (rbid, fpb_id, etc.), and storage properties (pointer positions).

```bash
# Check ROB state at cycle 100
uscope-cli state trace.uscope --cycle 100 --json | jq '.buffers[] | select(.name == "rob")'
```

### `timeline` — Instruction lifecycle

```bash
uscope-cli timeline trace.uscope --entity 42
```

Shows the complete lifecycle of instruction entity 42: fetch cycle, all stage transitions with durations, annotations, and retire/flush status.

```bash
# Find when entity 42 was in the execute stage
uscope-cli timeline trace.uscope --entity 42 --json | jq '.stages[] | select(.name == "Ex")'
```

### `counters` — Counter values

```bash
# Show final counter values
uscope-cli counters trace.uscope

# Per-cycle values over a range
uscope-cli counters trace.uscope --range 100:200

# Filter by counter name
uscope-cli counters trace.uscope --counter retired_insns --range 0:50
```

### `buffers` — Buffer occupancy

```bash
uscope-cli buffers trace.uscope --cycle 50
```

Like `state` but focused on buffer fill level, pointer pair positions, and occupancy percentage. Filter by buffer name with `--buffer`.

```bash
uscope-cli buffers trace.uscope --cycle 50 --buffer rob
```

---

## Output Formats

| Flag | Format | Use case |
|------|--------|----------|
| *(default)* | Human-readable aligned table | Interactive inspection |
| `--json` | Pretty-printed JSON | Scripting, piping to `jq`, CI |

---

## Examples

```bash
# Quick sanity check: does the trace have data?
uscope-cli info trace.uscope

# Debugging: what's in the ROB at cycle 50?
uscope-cli state trace.uscope --cycle 50

# Performance: what's the IPC?
uscope-cli counters trace.uscope --counter retired_insns

# Entity debugging: what happened to instruction 17?
uscope-cli timeline trace.uscope --entity 17

# Scripting: extract all counter names
uscope-cli info trace.uscope --json | jq -r '.counters[]'
```
