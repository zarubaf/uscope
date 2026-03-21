# konata2uscope

**Binary:** `konata2uscope`
**Location:** `crates/konata2uscope/`

---

## 1. Overview

`konata2uscope` converts [Konata](https://github.com/shioyadan/Konata)
(Kanata v0004) pipeline trace logs into µScope CPU protocol traces. This
enables viewing Konata-format traces in µScope-compatible viewers with
random-access seeking, mipmap summaries, and structured schema metadata.

---

## 2. Usage

```
konata2uscope <input.log[.gz]> -o <output.uscope> [options]
```

| Option | Default | Description |
|--------|---------|-------------|
| `-o <path>` | `output.uscope` | Output file path |
| `--clock-period-ps <ps>` | `1000` | Clock period in picoseconds (1000 = 1 GHz) |
| `--dut-name <name>` | `core0` | DUT name for the trace |

Gzip-compressed input (`.log.gz`) is detected automatically.

---

## 3. Two-Pass Architecture

### Pass 1: Scan

Reads the entire Konata log to discover metadata:

- All unique pipeline stage names (in first-occurrence order)
- Maximum number of simultaneously in-flight instructions
- Thread IDs
- Total cycle count

This information is needed to construct the µScope schema before writing any
trace data.

### Pass 2: Emit

Re-reads the log and emits µScope data using the CPU protocol writer:

- Entity allocation on instruction creation (`I`)
- Stage transitions on stage start (`S`, lane 0)
- Annotations on labels (`L`)
- Retirement on retire commands (`R`, type 0)
- Flushes on flush commands (`R`, type 1)
- Dependencies on dependency arrows (`W`)

---

## 4. Konata Format Mapping

### 4.1 Commands

| Konata | Description | µScope mapping |
|--------|-------------|----------------|
| `C=\t<cycle>` | Set absolute cycle | Time base |
| `C\t<delta>` | Advance by delta cycles | Time base |
| `I\t<id>\t<gid>\t<tid>` | Create instruction | `DA_SLOT_SET` on entities |
| `L\t<id>\t0\t<text>` | Disassembly label | `annotate` event; PC extraction |
| `L\t<id>\t1\t<text>` | Detail label | `annotate` event |
| `S\t<id>\t0\t<stage>` | Start stage (lane 0) | `stage_transition` event |
| `S\t<id>\t1+\t<stage>` | Start stall overlay | `annotate` event |
| `E\t<id>\t<lane>\t<stage>` | End stage | (implicit in µScope) |
| `R\t<id>\t<rid>\t0` | Retire | `DA_SLOT_CLEAR` + counter |
| `R\t<id>\t<rid>\t1` | Flush | `flush` event + `DA_SLOT_CLEAR` |
| `W\t<cons>\t<prod>\t<type>` | Dependency | `dependency` event |

### 4.2 PC Extraction

If a disassembly label (`L` type 0) starts with a hex address, it is extracted
as the instruction PC. Supported formats:

- `80000000 addi x0, x0, 0` → PC = `0x80000000`
- `0x80000000 addi x0, x0, 0` → PC = `0x80000000`
- `00001000: jal zero, 0x10` → PC = `0x00001000`

If no hex address is found, PC defaults to 0.

### 4.3 Stage Names

Konata stage names are arbitrary strings. Pass 1 collects them in pipeline
order (first occurrence). They become the `pipeline_stage` enum values in the
µScope schema and the `cpu.pipeline_stages` DUT property.

### 4.4 Time Model

Konata cycles are converted to picoseconds: `time_ps = cycle * clock_period_ps`.
The default clock period of 1000 ps corresponds to 1 GHz.

### 4.5 Lane Handling

Only lane 0 stage starts map to `stage_transition` events. Lane 1+ (stall
overlays in Konata) are emitted as `annotate` events with the text
`stall:<stage_name>`.

---

## 5. Example

### Input: `trace.log`

```
Kanata	0004
C=	0
I	0	0	0
L	0	0	80000000 addi x0, x0, 0
S	0	0	Fetch
C	1
E	0	0	Fetch
S	0	0	Decode
C	1
E	0	0	Decode
S	0	0	Execute
C	1
E	0	0	Execute
S	0	0	Writeback
R	0	0	0
```

### Conversion

```
$ konata2uscope trace.log -o trace.uscope --clock-period-ps 200
Pass 1: scanning trace.log...
  4 stages: [Fetch, Decode, Execute, Writeback]
  max in-flight: 1
  threads: 1
  total cycles: 3
Pass 2: emitting trace.uscope...
Done.
```

### Resulting Schema

- **Clock:** `core_clk` @ 200 ps (5 GHz)
- **Enum:** `pipeline_stage` = {Fetch, Decode, Execute, Writeback}
- **Storage:** `entities` (1 slot, sparse)
- **Events:** `stage_transition`, `annotate`, `dependency`, `flush`, `stall`
- **DUT:** `cpu.pipeline_stages = "Fetch,Decode,Execute,Writeback"`

The output file is a standard µScope trace readable by the Rust `Reader`.
