# µScope

Binary trace format for cycle-accurate hardware introspection.

## What's in the box

| Crate | Path | Description |
|-------|------|-------------|
| **uscope** | `crates/uscope/` | Transport layer: reader, writer, schema, checkpoints, delta replay, mipmaps |
| **uscope-cpu** | `crates/uscope-cpu/` | CPU protocol library: instruction lifecycle, counters, buffers, performance analysis |
| **uscope-cli** | `crates/uscope-cli/` | CLI for inspecting traces: info, state, timeline, counters, buffers |
| **uscope-mcp** | `crates/uscope-mcp/` | MCP server for AI-assisted trace debugging with Claude |
| **gen-uscope** | `crates/gen-uscope/` | Synthetic trace generator for testing |
| **konata2uscope** | `crates/konata2uscope/` | Converts Konata pipeline logs to µScope |

Also:
- **Specification** (`src/`) — Transport layer and CPU protocol specs
- **C DPI library** (`dpi/`) — Standalone C99 writer for simulator integration

## Architecture

```
┌──────────────────────────────────────────────┐
│ Consumers: uscope-cli, uscope-mcp, Reflex    │
└──────────────┬───────────────────────────────┘
               │
┌──────────────▼───────────────────────────────┐
│ uscope-cpu: CPU protocol interpretation      │
│  CpuTrace, instructions, stages, counters,   │
│  buffers, lazy loading, performance analysis  │
└──────────────┬───────────────────────────────┘
               │
┌──────────────▼───────────────────────────────┐
│ uscope: Transport layer (format only)        │
│  Reader, Writer, Schema, Checkpoints, Deltas │
└──────────────────────────────────────────────┘
```

## Quick start

### Rust library

```toml
[dependencies]
uscope = { path = "crates/uscope" }       # transport only
uscope-cpu = { path = "crates/uscope-cpu" } # + CPU protocol
```

```rust
use uscope_cpu::CpuTrace;

let mut trace = CpuTrace::open("trace.uscope")?;
println!("{:#?}", trace.file_info());
println!("IPC at cycle 100: {}", trace.counter_rate_at(0, 100, 64));
```

### CLI

```bash
# File overview
cargo run --bin uscope-cli -- info trace.uscope

# Buffer state at cycle 50
cargo run --bin uscope-cli -- state trace.uscope --cycle 50

# Instruction lifecycle
cargo run --bin uscope-cli -- timeline trace.uscope --entity 42

# Counter values over a range
cargo run --bin uscope-cli -- counters trace.uscope --range 100:200

# Buffer occupancy with pointer positions
cargo run --bin uscope-cli -- buffers trace.uscope --cycle 50

# JSON output for scripting
cargo run --bin uscope-cli -- info trace.uscope --json
```

### MCP server (for Claude)

```bash
# Start MCP server for a trace file
cargo run --bin uscope-mcp -- --trace trace.uscope

# Claude Code configuration (.claude/settings.json):
{
  "mcpServers": {
    "uscope": {
      "command": "cargo",
      "args": ["run", "--bin", "uscope-mcp", "--", "--trace", "/path/to/trace.uscope"]
    }
  }
}
```

MCP tools available to Claude:
- `file_info` — trace header, schema, metadata
- `state_at_cycle` — buffer contents at a specific cycle
- `entity_timeline` — instruction lifecycle (stages, retire/flush)
- `counter_values` — counter data over a range
- `buffer_occupancy` — buffer fill level with pointer positions
- `analyze_performance` — IPC, stall analysis, bottleneck identification

### C DPI

```
make -C dpi
# link with -luscope_dpi
```

### konata2uscope

```
cargo run -p konata2uscope -- trace.log -o trace.uscope
```

## Documentation

Built with [mdbook](https://rust-lang.github.io/mdBook/):

```
mdbook serve
```

## Tests

```
cargo test --workspace   # All Rust tests
make -C dpi test         # C library test (writes trace, verified by Rust reader)
```

## License

- Code: [Apache-2.0](LICENSE-APACHE)
- Specification text: [CC-BY-4.0](LICENSE-CC-BY)
