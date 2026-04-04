use clap::{Parser, Subcommand};
use serde::Serialize;
use uscope::types::FieldType;
use uscope_cpu::types::RetireStatus;
use uscope_cpu::CpuTrace;

/// CLI for inspecting uScope CPU pipeline traces.
#[derive(Parser)]
#[command(name = "uscope-cli", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// File header, schema, segments
    Info {
        /// Path to a .uscope trace file
        file: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Storage state at a given cycle
    State {
        /// Path to a .uscope trace file
        file: String,
        /// Cycle to query
        #[arg(long)]
        cycle: u32,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Entity lifecycle (stages, retire/flush)
    Timeline {
        /// Path to a .uscope trace file
        file: String,
        /// Entity ID to trace
        #[arg(long)]
        entity: u32,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Counter values
    Counters {
        /// Path to a .uscope trace file
        file: String,
        /// Cycle range as START:END (e.g. 0:100)
        #[arg(long)]
        range: Option<String>,
        /// Counter name filter
        #[arg(long)]
        counter: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Buffer occupancy and pointers
    Buffers {
        /// Path to a .uscope trace file
        file: String,
        /// Cycle to query
        #[arg(long)]
        cycle: u32,
        /// Buffer name filter
        #[arg(long)]
        buffer: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

// ── Serializable output types ──────────────────────────────────────

#[derive(Serialize)]
struct InfoOutput {
    version: String,
    segment_count: usize,
    total_instructions: u64,
    max_cycle: u32,
    period_ps: u64,
    metadata: Vec<(String, String)>,
    stage_names: Vec<String>,
    counter_names: Vec<String>,
    buffer_names: Vec<String>,
    schema: SchemaOutput,
}

#[derive(Serialize)]
struct SchemaOutput {
    storages: Vec<StorageInfo>,
    events: Vec<EventInfo>,
    enums: Vec<EnumInfo>,
}

#[derive(Serialize)]
struct StorageInfo {
    name: String,
    storage_id: u16,
    num_slots: u16,
    fields: Vec<FieldInfo>,
    properties: Vec<FieldInfo>,
    flags: Vec<String>,
}

#[derive(Serialize)]
struct EventInfo {
    name: String,
    event_type_id: u16,
    fields: Vec<FieldInfo>,
}

#[derive(Serialize)]
struct EnumInfo {
    name: String,
    values: Vec<EnumValueInfo>,
}

#[derive(Serialize)]
struct EnumValueInfo {
    value: u8,
    name: String,
}

#[derive(Serialize)]
struct FieldInfo {
    name: String,
    field_type: String,
}

#[derive(Serialize)]
struct BufferOutput {
    name: String,
    capacity: u16,
    occupied: usize,
    slots: Vec<SlotOutput>,
    properties: Vec<PropertyOutput>,
}

#[derive(Serialize)]
struct SlotOutput {
    slot: u16,
    fields: Vec<u64>,
    entity_fields: Vec<(String, u64)>,
}

#[derive(Serialize)]
struct PropertyOutput {
    name: String,
    value: u64,
    role: String,
}

#[derive(Serialize)]
struct TimelineOutput {
    entity_id: u32,
    disasm: String,
    first_cycle: u32,
    last_cycle: u32,
    retire_status: String,
    stages: Vec<StageOutput>,
    tooltip: String,
}

#[derive(Serialize)]
struct StageOutput {
    name: String,
    start_cycle: u32,
    end_cycle: u32,
}

#[derive(Serialize)]
struct CounterOutput {
    name: String,
    values: Vec<CounterValueOutput>,
}

#[derive(Serialize)]
struct CounterValueOutput {
    cycle: u32,
    value: u64,
}

// ── Helpers ────────────────────────────────────────────────────────

fn field_type_name(ft: u8) -> String {
    match FieldType::from_u8(ft) {
        Some(FieldType::U8) => "u8".into(),
        Some(FieldType::U16) => "u16".into(),
        Some(FieldType::U32) => "u32".into(),
        Some(FieldType::U64) => "u64".into(),
        Some(FieldType::I8) => "i8".into(),
        Some(FieldType::I16) => "i16".into(),
        Some(FieldType::I32) => "i32".into(),
        Some(FieldType::I64) => "i64".into(),
        Some(FieldType::Bool) => "bool".into(),
        Some(FieldType::StringRef) => "string_ref".into(),
        Some(FieldType::Enum) => "enum".into(),
        None => format!("unknown(0x{:02x})", ft),
    }
}

fn role_name(role: u8) -> &'static str {
    match role {
        1 => "HEAD_PTR",
        2 => "TAIL_PTR",
        _ => "plain",
    }
}

fn retire_status_str(rs: &RetireStatus) -> &'static str {
    match rs {
        RetireStatus::Retired => "retired",
        RetireStatus::Flushed => "flushed",
        RetireStatus::InFlight => "in_flight",
    }
}

fn parse_range(s: &str) -> Option<(u32, u32)> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() == 2 {
        let start = parts[0].parse().ok()?;
        let end = parts[1].parse().ok()?;
        Some((start, end))
    } else {
        None
    }
}

// ── Subcommand implementations ────────────────────────────────────

fn cmd_info(file: &str, json: bool) {
    let trace = CpuTrace::open(file).unwrap_or_else(|e| {
        eprintln!("error: failed to open '{}': {}", file, e);
        std::process::exit(1);
    });

    let info = trace.file_info();
    let schema = trace.schema();

    let storages: Vec<StorageInfo> = schema
        .storages
        .iter()
        .map(|s| {
            let name = schema.get_string(s.name).unwrap_or("?").to_string();
            let fields: Vec<FieldInfo> = s
                .fields
                .iter()
                .map(|f| FieldInfo {
                    name: schema.get_string(f.name).unwrap_or("?").to_string(),
                    field_type: field_type_name(f.field_type),
                })
                .collect();
            let properties: Vec<FieldInfo> = s
                .properties
                .iter()
                .map(|f| FieldInfo {
                    name: schema.get_string(f.name).unwrap_or("?").to_string(),
                    field_type: field_type_name(f.field_type),
                })
                .collect();
            let mut flags = Vec::new();
            if s.is_sparse() {
                flags.push("sparse".to_string());
            }
            if s.is_buffer() {
                flags.push("buffer".to_string());
            }
            StorageInfo {
                name,
                storage_id: s.storage_id,
                num_slots: s.num_slots,
                fields,
                properties,
                flags,
            }
        })
        .collect();

    let events: Vec<EventInfo> = schema
        .events
        .iter()
        .map(|e| {
            let name = schema.get_string(e.name).unwrap_or("?").to_string();
            let fields: Vec<FieldInfo> = e
                .fields
                .iter()
                .map(|f| FieldInfo {
                    name: schema.get_string(f.name).unwrap_or("?").to_string(),
                    field_type: field_type_name(f.field_type),
                })
                .collect();
            EventInfo {
                name,
                event_type_id: e.event_type_id,
                fields,
            }
        })
        .collect();

    let enums: Vec<EnumInfo> = schema
        .enums
        .iter()
        .map(|e| {
            let name = schema.get_string(e.name).unwrap_or("?").to_string();
            let values: Vec<EnumValueInfo> = e
                .values
                .iter()
                .map(|v| EnumValueInfo {
                    value: v.value,
                    name: schema.get_string(v.name).unwrap_or("?").to_string(),
                })
                .collect();
            EnumInfo { name, values }
        })
        .collect();

    let counter_names: Vec<String> = trace
        .counter_names()
        .iter()
        .map(|(_, name)| name.clone())
        .collect();
    let buffer_names: Vec<String> = trace
        .buffer_infos()
        .iter()
        .map(|b| b.name.clone())
        .collect();

    let output = InfoOutput {
        version: info.version,
        segment_count: info.segment_count,
        total_instructions: info.total_instructions,
        max_cycle: info.max_cycle,
        period_ps: info.period_ps,
        metadata: info.metadata,
        stage_names: trace.stage_names().to_vec(),
        counter_names,
        buffer_names,
        schema: SchemaOutput {
            storages,
            events,
            enums,
        },
    };

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&output).expect("JSON serialization failed")
        );
    } else {
        print_info_human(&output);
    }
}

fn print_info_human(info: &InfoOutput) {
    println!("=== Trace Info ===");
    println!("  Version:            {}", info.version);
    println!("  Segments:           {}", info.segment_count);
    println!("  Total instructions: {}", info.total_instructions);
    println!("  Max cycle:          {}", info.max_cycle);
    println!("  Period (ps):        {}", info.period_ps);
    println!();

    if !info.metadata.is_empty() {
        println!("--- Metadata ---");
        let max_key = info
            .metadata
            .iter()
            .map(|(k, _)| k.len())
            .max()
            .unwrap_or(0);
        for (k, v) in &info.metadata {
            println!("  {:width$}  {}", k, v, width = max_key);
        }
        println!();
    }

    if !info.stage_names.is_empty() {
        println!("--- Pipeline Stages ---");
        for (i, name) in info.stage_names.iter().enumerate() {
            println!("  [{:2}] {}", i, name);
        }
        println!();
    }

    if !info.counter_names.is_empty() {
        println!("--- Counters ---");
        for name in &info.counter_names {
            println!("  {}", name);
        }
        println!();
    }

    if !info.buffer_names.is_empty() {
        println!("--- Buffers ---");
        for name in &info.buffer_names {
            println!("  {}", name);
        }
        println!();
    }

    println!("--- Schema: Storages ({}) ---", info.schema.storages.len());
    for s in &info.schema.storages {
        let flags_str = if s.flags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", s.flags.join(", "))
        };
        println!(
            "  {:20} id={:<3} slots={:<4}{}",
            s.name, s.storage_id, s.num_slots, flags_str
        );
        for f in &s.fields {
            println!("    .{:20} {}", f.name, f.field_type);
        }
        if !s.properties.is_empty() {
            for p in &s.properties {
                println!("    @{:20} {}", p.name, p.field_type);
            }
        }
    }
    println!();

    println!("--- Schema: Events ({}) ---", info.schema.events.len());
    for e in &info.schema.events {
        println!("  {:24} id={}", e.name, e.event_type_id);
        for f in &e.fields {
            println!("    .{:20} {}", f.name, f.field_type);
        }
    }
    println!();

    if !info.schema.enums.is_empty() {
        println!("--- Schema: Enums ({}) ---", info.schema.enums.len());
        for e in &info.schema.enums {
            println!("  {}", e.name);
            for v in &e.values {
                println!("    {:3} = {}", v.value, v.name);
            }
        }
        println!();
    }
}

fn cmd_state(file: &str, cycle: u32, json: bool) {
    let mut trace = CpuTrace::open(file).unwrap_or_else(|e| {
        eprintln!("error: failed to open '{}': {}", file, e);
        std::process::exit(1);
    });

    let buffer_infos: Vec<(String, usize)> = trace
        .buffer_infos()
        .iter()
        .enumerate()
        .map(|(i, b)| (b.name.clone(), i))
        .collect();

    let mut outputs: Vec<BufferOutput> = Vec::new();
    for (name, idx) in &buffer_infos {
        let state = trace.buffer_state_at(*idx, cycle);
        let slots: Vec<SlotOutput> = state
            .slots
            .iter()
            .map(|(slot, fields, entity_fields)| SlotOutput {
                slot: *slot,
                fields: fields.clone(),
                entity_fields: entity_fields.clone(),
            })
            .collect();
        let properties: Vec<PropertyOutput> = state
            .properties
            .iter()
            .map(|p| PropertyOutput {
                name: p.name.clone(),
                value: p.value,
                role: role_name(p.role).to_string(),
            })
            .collect();
        outputs.push(BufferOutput {
            name: name.clone(),
            capacity: state.capacity,
            occupied: state.slots.len(),
            slots,
            properties,
        });
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&outputs).expect("JSON serialization failed")
        );
    } else {
        println!("=== State at cycle {} ===", cycle);
        println!();
        for buf in &outputs {
            println!(
                "--- {} (capacity={}, occupied={}) ---",
                buf.name, buf.capacity, buf.occupied
            );
            if !buf.properties.is_empty() {
                for p in &buf.properties {
                    let role_suffix = if p.role == "plain" {
                        String::new()
                    } else {
                        format!(" ({})", p.role)
                    };
                    println!("  @{}: {}{}", p.name, p.value, role_suffix);
                }
            }
            if buf.slots.is_empty() {
                println!("  (empty)");
            } else {
                for slot in &buf.slots {
                    let fields_str: String = slot
                        .fields
                        .iter()
                        .map(|v| format!("{}", v))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let entity_str: String = slot
                        .entity_fields
                        .iter()
                        .map(|(k, v)| format!("{}={}", k, v))
                        .collect::<Vec<_>>()
                        .join(" ");
                    if entity_str.is_empty() {
                        println!("  slot {:3}: [{}]", slot.slot, fields_str);
                    } else {
                        println!("  slot {:3}: [{}]  {}", slot.slot, fields_str, entity_str);
                    }
                }
            }
            println!();
        }
    }
}

fn cmd_timeline(file: &str, entity_id: u32, json: bool) {
    let mut trace = CpuTrace::open(file).unwrap_or_else(|e| {
        eprintln!("error: failed to open '{}': {}", file, e);
        std::process::exit(1);
    });

    // Load all segments and search for the entity
    let seg_count = trace.segment_count();
    let all_indices: Vec<usize> = (0..seg_count).collect();

    let result = trace.load_segments(&all_indices).unwrap_or_else(|e| {
        eprintln!("error: failed to load segments: {}", e);
        std::process::exit(1);
    });

    let stage_names = trace.stage_names().to_vec();

    // Find instruction(s) matching the entity ID
    let matches: Vec<_> = result
        .instructions
        .iter()
        .filter(|inst| inst.id == entity_id)
        .collect();

    if matches.is_empty() {
        eprintln!("error: entity {} not found in trace", entity_id);
        std::process::exit(1);
    }

    let mut outputs: Vec<TimelineOutput> = Vec::new();
    for inst in &matches {
        let stages: Vec<StageOutput> = (inst.stage_range.start..inst.stage_range.end)
            .filter_map(|si| {
                result.stages.get(si as usize).map(|s| {
                    let name = stage_names
                        .get(s.stage_name_idx as usize)
                        .cloned()
                        .unwrap_or_else(|| format!("stage_{}", s.stage_name_idx));
                    StageOutput {
                        name,
                        start_cycle: s.start_cycle,
                        end_cycle: s.end_cycle,
                    }
                })
            })
            .collect();

        outputs.push(TimelineOutput {
            entity_id: inst.id,
            disasm: inst.disasm.clone(),
            first_cycle: inst.first_cycle,
            last_cycle: inst.last_cycle,
            retire_status: retire_status_str(&inst.retire_status).to_string(),
            stages,
            tooltip: inst.tooltip.clone(),
        });
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&outputs).expect("JSON serialization failed")
        );
    } else {
        for tl in &outputs {
            println!("=== Entity {} ===", tl.entity_id);
            println!("  Disasm:  {}", tl.disasm);
            println!("  Cycles:  {} - {}", tl.first_cycle, tl.last_cycle);
            println!("  Status:  {}", tl.retire_status);
            if !tl.tooltip.is_empty() {
                println!("  Tooltip: {}", tl.tooltip.replace('\n', "\n           "));
            }
            println!();
            if tl.stages.is_empty() {
                println!("  (no stages)");
            } else {
                println!(
                    "  {:20} {:>8} {:>8} {:>8}",
                    "Stage", "Start", "End", "Duration"
                );
                println!(
                    "  {:20} {:>8} {:>8} {:>8}",
                    "-----", "-----", "---", "--------"
                );
                for s in &tl.stages {
                    let duration = s.end_cycle.saturating_sub(s.start_cycle);
                    println!(
                        "  {:20} {:>8} {:>8} {:>8}",
                        s.name, s.start_cycle, s.end_cycle, duration
                    );
                }
            }
            println!();
        }
    }
}

fn cmd_counters(file: &str, range: Option<String>, counter_filter: Option<String>, json: bool) {
    let trace = CpuTrace::open(file).unwrap_or_else(|e| {
        eprintln!("error: failed to open '{}': {}", file, e);
        std::process::exit(1);
    });

    let counter_names: Vec<(usize, String)> = trace
        .counter_names()
        .iter()
        .enumerate()
        .map(|(i, (_, name))| (i, name.clone()))
        .collect();

    let filtered: Vec<(usize, String)> = if let Some(ref filter) = counter_filter {
        counter_names
            .into_iter()
            .filter(|(_, name)| name.contains(filter.as_str()))
            .collect()
    } else {
        counter_names
    };

    if filtered.is_empty() {
        if let Some(ref filter) = counter_filter {
            eprintln!("error: no counters matching '{}'", filter);
        } else {
            eprintln!("error: no counters in trace");
        }
        std::process::exit(1);
    }

    let max_cycle = trace.max_cycle();
    let (start, end) = if let Some(ref range_str) = range {
        parse_range(range_str).unwrap_or_else(|| {
            eprintln!(
                "error: invalid range format '{}', expected START:END",
                range_str
            );
            std::process::exit(1);
        })
    } else {
        (0, max_cycle)
    };

    let mut outputs: Vec<CounterOutput> = Vec::new();

    for (idx, name) in &filtered {
        let mut values = Vec::new();
        if range.is_some() {
            // Per-cycle values for the range
            for cycle in start..=end {
                let val = trace.counter_value_at(*idx, cycle);
                values.push(CounterValueOutput { cycle, value: val });
            }
        } else {
            // Just show value at max_cycle
            let val = trace.counter_value_at(*idx, max_cycle);
            values.push(CounterValueOutput {
                cycle: max_cycle,
                value: val,
            });
        }
        outputs.push(CounterOutput {
            name: name.clone(),
            values,
        });
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&outputs).expect("JSON serialization failed")
        );
    } else {
        if range.is_some() {
            // Table format for range output
            // Header
            let counter_names_str: Vec<&str> = outputs.iter().map(|o| o.name.as_str()).collect();
            let col_width = counter_names_str
                .iter()
                .map(|n| n.len().max(12))
                .collect::<Vec<_>>();

            print!("  {:>8}", "Cycle");
            for (i, name) in counter_names_str.iter().enumerate() {
                print!("  {:>width$}", name, width = col_width[i]);
            }
            println!();

            print!("  {:>8}", "-----");
            for cw in &col_width {
                print!("  {:>width$}", "-----", width = *cw);
            }
            println!();

            // Determine how many rows to print
            let num_rows = outputs[0].values.len();
            // If range is large, sample to avoid flooding terminal
            let step = if num_rows > 200 { num_rows / 200 } else { 1 };

            for row in (0..num_rows).step_by(step) {
                let cycle = outputs[0].values[row].cycle;
                print!("  {:>8}", cycle);
                for (i, o) in outputs.iter().enumerate() {
                    print!("  {:>width$}", o.values[row].value, width = col_width[i]);
                }
                println!();
            }
            // Always print last row if we skipped it
            if step > 1 && (num_rows - 1) % step != 0 {
                let row = num_rows - 1;
                let cycle = outputs[0].values[row].cycle;
                print!("  {:>8}", cycle);
                for (i, o) in outputs.iter().enumerate() {
                    print!("  {:>width$}", o.values[row].value, width = col_width[i]);
                }
                println!();
            }
        } else {
            // Summary table
            println!("=== Counters at cycle {} ===", max_cycle);
            let max_name = outputs.iter().map(|o| o.name.len()).max().unwrap_or(0);
            for o in &outputs {
                println!(
                    "  {:width$}  {}",
                    o.name,
                    o.values[0].value,
                    width = max_name
                );
            }
        }
    }
}

fn cmd_buffers(file: &str, cycle: u32, buffer_filter: Option<String>, json: bool) {
    let mut trace = CpuTrace::open(file).unwrap_or_else(|e| {
        eprintln!("error: failed to open '{}': {}", file, e);
        std::process::exit(1);
    });

    let buffer_infos: Vec<(String, usize)> = trace
        .buffer_infos()
        .iter()
        .enumerate()
        .map(|(i, b)| (b.name.clone(), i))
        .collect();

    let filtered: Vec<(String, usize)> = if let Some(ref filter) = buffer_filter {
        buffer_infos
            .into_iter()
            .filter(|(name, _)| name.contains(filter.as_str()))
            .collect()
    } else {
        buffer_infos
    };

    if filtered.is_empty() {
        if let Some(ref filter) = buffer_filter {
            eprintln!("error: no buffers matching '{}'", filter);
        } else {
            eprintln!("error: no buffers in trace");
        }
        std::process::exit(1);
    }

    let mut outputs: Vec<BufferOutput> = Vec::new();
    for (name, idx) in &filtered {
        let state = trace.buffer_state_at(*idx, cycle);
        let slots: Vec<SlotOutput> = state
            .slots
            .iter()
            .map(|(slot, fields, entity_fields)| SlotOutput {
                slot: *slot,
                fields: fields.clone(),
                entity_fields: entity_fields.clone(),
            })
            .collect();
        let properties: Vec<PropertyOutput> = state
            .properties
            .iter()
            .map(|p| PropertyOutput {
                name: p.name.clone(),
                value: p.value,
                role: role_name(p.role).to_string(),
            })
            .collect();
        outputs.push(BufferOutput {
            name: name.clone(),
            capacity: state.capacity,
            occupied: slots.len(),
            slots,
            properties,
        });
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&outputs).expect("JSON serialization failed")
        );
    } else {
        println!("=== Buffers at cycle {} ===", cycle);
        println!();
        for buf in &outputs {
            let fill_pct = if buf.capacity > 0 {
                (buf.occupied as f64 / buf.capacity as f64) * 100.0
            } else {
                0.0
            };
            println!(
                "--- {} (capacity={}, occupied={}, fill={:.0}%) ---",
                buf.name, buf.capacity, buf.occupied, fill_pct
            );

            // Print pointer pairs
            let mut heads: Vec<(&str, u64)> = Vec::new();
            let mut tails: Vec<(&str, u64)> = Vec::new();
            for p in &buf.properties {
                match p.role.as_str() {
                    "HEAD_PTR" => heads.push((&p.name, p.value)),
                    "TAIL_PTR" => tails.push((&p.name, p.value)),
                    _ => {}
                }
            }
            if !heads.is_empty() || !tails.is_empty() {
                for p in &buf.properties {
                    let role_suffix = if p.role == "plain" {
                        String::new()
                    } else {
                        format!(" ({})", p.role)
                    };
                    println!("  @{}: {}{}", p.name, p.value, role_suffix);
                }
            }

            if buf.slots.is_empty() {
                println!("  (empty)");
            } else {
                for slot in &buf.slots {
                    let fields_str: String = slot
                        .fields
                        .iter()
                        .map(|v| format!("{}", v))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let entity_str: String = slot
                        .entity_fields
                        .iter()
                        .map(|(k, v)| format!("{}={}", k, v))
                        .collect::<Vec<_>>()
                        .join(" ");
                    if entity_str.is_empty() {
                        println!("  slot {:3}: [{}]", slot.slot, fields_str);
                    } else {
                        println!("  slot {:3}: [{}]  {}", slot.slot, fields_str, entity_str);
                    }
                }
            }
            println!();
        }
    }
}

// ── Main ───────────────────────────────────────────────────────────

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Info { file, json } => cmd_info(&file, json),
        Commands::State { file, cycle, json } => cmd_state(&file, cycle, json),
        Commands::Timeline { file, entity, json } => cmd_timeline(&file, entity, json),
        Commands::Counters {
            file,
            range,
            counter,
            json,
        } => cmd_counters(&file, range, counter, json),
        Commands::Buffers {
            file,
            cycle,
            buffer,
            json,
        } => cmd_buffers(&file, cycle, buffer, json),
    }
}
