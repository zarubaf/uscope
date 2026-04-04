# uscope-mcp: MCP Server for AI-Assisted Debugging

**Binary:** `uscope-mcp`
**Location:** `crates/uscope-mcp/`

---

## Overview

`uscope-mcp` is a Model Context Protocol (MCP) server that lets Claude inspect µScope CPU pipeline traces. It exposes the `uscope-cpu` query API as MCP tools, enabling natural-language performance debugging.

---

## Quick Start

### 1. Start the server

```bash
cargo run --bin uscope-mcp -- --trace /path/to/trace.uscope
```

### 2. Configure Claude Code

Add to `.claude/settings.json`:

```json
{
  "mcpServers": {
    "uscope": {
      "command": "cargo",
      "args": ["run", "--release", "--bin", "uscope-mcp", "--",
               "--trace", "/path/to/trace.uscope"],
      "cwd": "/path/to/uscope/repo"
    }
  }
}
```

Or with a pre-built binary:

```json
{
  "mcpServers": {
    "uscope": {
      "command": "/path/to/uscope-mcp",
      "args": ["--trace", "/path/to/trace.uscope"]
    }
  }
}
```

### 3. Ask Claude

> "What's the IPC between cycles 100 and 500?"

> "Show me the pipeline stages for entity 42"

> "Why is the ROB full at cycle 200?"

> "What caused the pipeline stall at cycle 350?"

---

## MCP Tools

### `file_info`

Returns trace header, schema, segments, counters, buffers, and metadata.

**Parameters:** none

### `state_at_cycle`

Returns buffer contents at a specific cycle — slot values, entity fields, and storage properties.

**Parameters:**
- `cycle` (number, required): cycle number to query

### `entity_timeline`

Returns the complete lifecycle of an instruction: stages with durations, disasm, annotations, retire/flush status.

**Parameters:**
- `entity_id` (number, required): entity ID to trace

### `counter_values`

Returns counter data over a cycle range with per-cycle values, deltas, and rates.

**Parameters:**
- `counter` (string, required): counter name (e.g., `"retired_insns"`)
- `start_cycle` (number, required): range start
- `end_cycle` (number, required): range end

### `buffer_occupancy`

Returns buffer fill level at a cycle — occupied slots, pointer pair positions, fill percentage.

**Parameters:**
- `buffer` (string, required): buffer name (e.g., `"rob"`)
- `cycle` (number, required): cycle to query

### `analyze_performance`

Returns a structured performance summary over a cycle range:
- Instruction counts (total, retired, flushed, in-flight)
- IPC (instructions per cycle)
- Flush rate
- Per-counter totals and rates
- Buffer occupancy snapshots at start/mid/end
- Per-stage average latency, sorted by bottleneck

**Parameters:**
- `start_cycle` (number, required): range start
- `end_cycle` (number, required): range end

---

## Protocol

The server implements the [Model Context Protocol](https://modelcontextprotocol.io/) over stdio using JSON-RPC 2.0. It handles:

- `initialize` — server capabilities and info
- `notifications/initialized` — acknowledged silently
- `tools/list` — returns tool definitions with JSON Schema
- `tools/call` — dispatches to tool handlers

All tool responses are structured JSON, formatted for AI reasoning. Errors are returned as MCP tool errors (not JSON-RPC errors) so Claude can see error messages.

Logging goes to stderr (stdout is the MCP channel).
