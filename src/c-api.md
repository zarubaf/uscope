# C DPI Library API Reference

**Header:** `uscope_dpi.h`
**Location:** `dpi/`

---

## 1. Overview

The C DPI library is a standalone, write-only µScope trace library designed for
integration with hardware simulators via DPI (Direct Programming Interface). It
produces trace files that are binary-compatible with the Rust reader.

### Design Principles

- **Single .c + .h** (plus vendored LZ4) — easy to integrate
- **C99** — compiles with any standard C compiler
- **No dynamic allocation during per-cycle operations** — pre-allocated buffers
- **Write-only** — no reader (use the Rust crate for reading)
- **Zero Rust dependency** — fully self-contained

### Building

```
make -C dpi            # builds libuscope_dpi.a
make -C dpi test       # builds and runs the test program
```

Link with `-luscope_dpi` (or include `uscope_dpi.c` and `lz4.c` directly).

---

## 2. Schema Building

Before opening a writer, define the trace schema.

### 2.1 Create / Free

```c
uscope_schema_def_t *schema = uscope_schema_new();
// ... add clocks, scopes, enums, storages, events ...
// Schema is consumed by uscope_writer_open() — do not free after open.
// If not opening a writer, free with:
uscope_schema_free(schema);
```

### 2.2 Clock Domains

```c
uint8_t clk = uscope_schema_add_clock(schema, "core_clk", 1000); // 1 GHz
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `name` | `const char *` | Clock name |
| `period_ps` | `uint32_t` | Period in picoseconds |
| **Returns** | `uint8_t` | Clock domain ID |

### 2.3 Scopes

```c
uscope_schema_add_scope(schema, "root", 0xFFFF, NULL, 0xFF);
uint16_t scope = uscope_schema_add_scope(schema, "core0", 0, "cpu", clk);
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `name` | `const char *` | Scope name |
| `parent` | `uint16_t` | Parent scope ID (`0xFFFF` = root) |
| `protocol` | `const char *` | Protocol name (`NULL` = none) |
| `clock_id` | `uint8_t` | Clock domain (`0xFF` = inherit) |
| **Returns** | `uint16_t` | Scope ID |

### 2.4 Enums

```c
const char *stages[] = {"fetch", "decode", "execute", "writeback"};
uint8_t stage_enum = uscope_schema_add_enum(schema, "pipeline_stage", stages, 4);
```

### 2.5 Storages

Fields are passed as parallel arrays of names, types, and enum IDs.

```c
const char  *fields[]    = {"entity_id", "pc",          "inst_bits"};
uint8_t      types[]     = {USCOPE_FT_U32, USCOPE_FT_U64, USCOPE_FT_U32};
uint8_t      enum_ids[]  = {0,             0,              0};

uint16_t entities = uscope_schema_add_storage(
    schema, "entities", scope, /*num_slots=*/512, USCOPE_SF_SPARSE,
    /*num_fields=*/3, fields, types, enum_ids);
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `name` | `const char *` | Storage name |
| `scope_id` | `uint16_t` | Owning scope |
| `num_slots` | `uint16_t` | Number of slots |
| `flags` | `uint16_t` | `USCOPE_SF_SPARSE` or `0` (dense) |
| `num_fields` | `uint16_t` | Number of fields |
| `field_names` | `const char **` | Field name array |
| `field_types` | `const uint8_t *` | Field type array |
| `field_enum_ids` | `const uint8_t *` | Enum ID array (or `NULL`) |
| **Returns** | `uint16_t` | Storage ID |

### 2.6 Events

```c
const char  *st_fields[] = {"entity_id",    "stage"};
uint8_t      st_types[]  = {USCOPE_FT_U32,  USCOPE_FT_ENUM};
uint8_t      st_enums[]  = {0,              stage_enum};

uint16_t st_event = uscope_schema_add_event(
    schema, "stage_transition", scope,
    /*num_fields=*/2, st_fields, st_types, st_enums);
```

---

## 3. Field Type Constants

| Constant | Value | Size | Description |
|----------|-------|------|-------------|
| `USCOPE_FT_U8` | `0x01` | 1 | Unsigned 8-bit |
| `USCOPE_FT_U16` | `0x02` | 2 | Unsigned 16-bit |
| `USCOPE_FT_U32` | `0x03` | 4 | Unsigned 32-bit |
| `USCOPE_FT_U64` | `0x04` | 8 | Unsigned 64-bit |
| `USCOPE_FT_I8` | `0x05` | 1 | Signed 8-bit |
| `USCOPE_FT_I16` | `0x06` | 2 | Signed 16-bit |
| `USCOPE_FT_I32` | `0x07` | 4 | Signed 32-bit |
| `USCOPE_FT_I64` | `0x08` | 8 | Signed 64-bit |
| `USCOPE_FT_BOOL` | `0x09` | 1 | Boolean |
| `USCOPE_FT_STRING_REF` | `0x0A` | 4 | String table index |
| `USCOPE_FT_ENUM` | `0x0B` | 1 | Enum value |

---

## 4. Writer

### 4.1 Open / Close

```c
uscope_dut_property_t props[] = {
    {"dut_name", "boom_core_0"},
    {"cpu.isa",  "RV64GC"},
};

uscope_writer_t *w = uscope_writer_open(
    "trace.uscope",
    props, /*num_props=*/2,
    schema,                    // consumed — do not free
    /*checkpoint_interval_ps=*/1000000);

// ... write cycles ...

uscope_writer_close(w);  // finalizes and frees
```

`uscope_writer_open` takes ownership of the schema. Do not call
`uscope_schema_free` after opening.

`uscope_writer_close` writes the string table, segment table, section table,
sets `F_COMPLETE`, and frees all resources.

### 4.2 Per-Cycle Operations

All mutations must occur within a `begin_cycle` / `end_cycle` pair. Time must
be monotonically non-decreasing.

```c
uscope_begin_cycle(w, time_ps);

uscope_slot_set(w, storage_id, slot, field, value);
uscope_slot_clear(w, storage_id, slot);
uscope_slot_add(w, storage_id, slot, field, delta);
uscope_event(w, event_type_id, payload, payload_size);

uscope_end_cycle(w);
```

| Function | Description |
|----------|-------------|
| `uscope_begin_cycle(w, time_ps)` | Start a cycle at the given time |
| `uscope_slot_set(w, stor, slot, field, val)` | Set field value (marks slot valid) |
| `uscope_slot_clear(w, stor, slot)` | Mark slot invalid |
| `uscope_slot_add(w, stor, slot, field, val)` | Add to field value |
| `uscope_event(w, type_id, payload, size)` | Emit event with raw payload |
| `uscope_end_cycle(w)` | End cycle, flush segment if needed |

### 4.3 Event Payloads

Event payloads are the field values concatenated in schema-definition order,
little-endian, with no padding. Build them manually:

```c
// stage_transition: entity_id (U32) + stage (ENUM/U8)
uint8_t payload[5];
uint32_t entity_id = 42;
memcpy(payload, &entity_id, 4);  // little-endian on LE platforms
payload[4] = 2;                  // stage index
uscope_event(w, st_event, payload, 5);
```

### 4.4 String Table

For `STRING_REF` fields in event payloads:

```c
uint32_t idx = uscope_string_insert(w, "addi x0, x0, 0");
// Use idx as the 4-byte value in a STRING_REF field
```

---

## 5. Limits

| Resource | Maximum |
|----------|---------|
| String pool (schema) | 64 KB |
| Clock domains | 16 |
| Scopes | 256 |
| Enum types | 64 |
| Enum values per type | 256 |
| Storages | 256 |
| Event types | 256 |
| Fields per storage/event | 32 |
| Ops per cycle | 4096 |
| Events per cycle | 1024 |
| Event payload size | 256 bytes |
| Segments | 65536 |
| Delta buffer | 4 MB (auto-grows) |

---

## 6. Example: CPU Pipeline Trace

```c
#include "uscope_dpi.h"
#include <string.h>

int main(void) {
    // Schema
    uscope_schema_def_t *s = uscope_schema_new();
    uint8_t clk = uscope_schema_add_clock(s, "clk", 1000);
    uscope_schema_add_scope(s, "root", 0xFFFF, NULL, 0xFF);
    uint16_t scope = uscope_schema_add_scope(s, "core0", 0, "cpu", clk);

    const char *stages[] = {"fetch", "decode", "execute", "writeback"};
    uint8_t se = uscope_schema_add_enum(s, "pipeline_stage", stages, 4);

    const char *ef[] = {"entity_id", "pc", "inst_bits"};
    uint8_t et[] = {USCOPE_FT_U32, USCOPE_FT_U64, USCOPE_FT_U32};
    uint16_t ent = uscope_schema_add_storage(s, "entities", scope,
                                              256, USCOPE_SF_SPARSE,
                                              3, ef, et, NULL);

    const char *sf[] = {"entity_id", "stage"};
    uint8_t st[] = {USCOPE_FT_U32, USCOPE_FT_ENUM};
    uint8_t sen[] = {0, se};
    uint16_t sev = uscope_schema_add_event(s, "stage_transition", scope,
                                            2, sf, st, sen);

    // DUT properties
    uscope_dut_property_t props[] = {
        {"dut_name", "core0"},
        {"cpu.isa", "RV64GC"},
        {"cpu.pipeline_stages", "fetch,decode,execute,writeback"},
    };

    // Open
    uscope_writer_t *w = uscope_writer_open("trace.uscope",
                                             props, 3, s, 100000);

    // Fetch instruction 0
    uscope_begin_cycle(w, 0);
    uscope_slot_set(w, ent, 0, 0, 0);          // entity_id
    uscope_slot_set(w, ent, 0, 1, 0x80000000); // pc
    uscope_slot_set(w, ent, 0, 2, 0x13);       // inst_bits
    uint8_t payload[5];
    uint32_t eid = 0;
    memcpy(payload, &eid, 4);
    payload[4] = 0; // fetch stage
    uscope_event(w, sev, payload, 5);
    uscope_end_cycle(w);

    // Decode
    uscope_begin_cycle(w, 1000);
    payload[4] = 1;
    uscope_event(w, sev, payload, 5);
    uscope_end_cycle(w);

    // Execute
    uscope_begin_cycle(w, 2000);
    payload[4] = 2;
    uscope_event(w, sev, payload, 5);
    uscope_end_cycle(w);

    // Writeback + retire
    uscope_begin_cycle(w, 3000);
    payload[4] = 3;
    uscope_event(w, sev, payload, 5);
    uscope_slot_clear(w, ent, 0);
    uscope_end_cycle(w);

    uscope_writer_close(w);
    return 0;
}
```

---

## 7. Integration with Simulators

### SystemVerilog DPI

```systemverilog
import "DPI-C" function chandle uscope_writer_open(
    input string path,
    /* ... */
);
import "DPI-C" function void uscope_begin_cycle(
    input chandle w, input longint unsigned time_ps
);
// ... etc
```

### Verilator

Include `uscope_dpi.c` and `lz4.c` in the Verilator build:

```
verilator --cc top.sv --exe sim_main.cpp uscope_dpi.c lz4.c
```

Call the C API from `sim_main.cpp` or from DPI-exported functions in the
SystemVerilog testbench.
