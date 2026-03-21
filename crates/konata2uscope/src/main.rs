/// konata2uscope: Convert Konata (Kanata v0004) pipeline trace logs to µScope format.
///
/// Usage: konata2uscope input.log -o output.uscope [--clock-period-ps 1000] [--dut-name "core0"]

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, BufRead, BufReader};

use flate2::read::GzDecoder;
use uscope::protocols::cpu::{CpuSchemaBuilder, CpuWriter};
use uscope::writer::Writer;

/// Parsed Konata command.
#[derive(Debug)]
enum KonataCmd {
    /// C=\t<cycle> — set absolute cycle
    CycleAbsolute(u64),
    /// C\t<delta> — advance by delta cycles
    CycleDelta(u64),
    /// I\t<id>\t<gid>\t<tid> — create instruction
    Instruction { id: u32, _gid: u32, tid: u32 },
    /// L\t<id>\t<type>\t<text> — label
    Label { id: u32, label_type: u32, text: String },
    /// S\t<id>\t<lane>\t<stage> — start stage
    StageStart { id: u32, lane: u32, stage: String },
    /// E\t<id>\t<lane>\t<stage> — end stage
    StageEnd { _id: u32, _lane: u32, _stage: String },
    /// R\t<id>\t<rid>\t<type> — retire (0) or flush (1)
    Retire { id: u32, _rid: u32, retire_type: u32 },
    /// W\t<consumer>\t<producer>\t<type> — dependency
    Dependency { consumer: u32, producer: u32, _dep_type: u32 },
}

fn parse_line(line: &str) -> Option<KonataCmd> {
    let parts: Vec<&str> = line.split('\t').collect();
    if parts.is_empty() {
        return None;
    }

    match parts[0] {
        "C=" => {
            let cycle: u64 = parts.get(1)?.parse().ok()?;
            Some(KonataCmd::CycleAbsolute(cycle))
        }
        "C" => {
            let delta: u64 = parts.get(1)?.parse().ok()?;
            Some(KonataCmd::CycleDelta(delta))
        }
        "I" => {
            let id: u32 = parts.get(1)?.parse().ok()?;
            let gid: u32 = parts.get(2)?.parse().ok()?;
            let tid: u32 = parts.get(3)?.parse().ok()?;
            Some(KonataCmd::Instruction { id, _gid: gid, tid })
        }
        "L" => {
            let id: u32 = parts.get(1)?.parse().ok()?;
            let label_type: u32 = parts.get(2)?.parse().ok()?;
            let text = parts.get(3).unwrap_or(&"").to_string();
            Some(KonataCmd::Label { id, label_type, text })
        }
        "S" => {
            let id: u32 = parts.get(1)?.parse().ok()?;
            let lane: u32 = parts.get(2)?.parse().ok()?;
            let stage = parts.get(3)?.to_string();
            Some(KonataCmd::StageStart { id, lane, stage })
        }
        "E" => {
            let id: u32 = parts.get(1)?.parse().ok()?;
            let lane: u32 = parts.get(2)?.parse().ok()?;
            let stage = parts.get(3)?.to_string();
            Some(KonataCmd::StageEnd { _id: id, _lane: lane, _stage: stage })
        }
        "R" => {
            let id: u32 = parts.get(1)?.parse().ok()?;
            let rid: u32 = parts.get(2)?.parse().ok()?;
            let retire_type: u32 = parts.get(3)?.parse().ok()?;
            Some(KonataCmd::Retire { id, _rid: rid, retire_type })
        }
        "W" => {
            let consumer: u32 = parts.get(1)?.parse().ok()?;
            let producer: u32 = parts.get(2)?.parse().ok()?;
            let dep_type: u32 = parts.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
            Some(KonataCmd::Dependency { consumer, producer, _dep_type: dep_type })
        }
        _ => None,
    }
}

/// Data gathered in pass 1 (scan).
struct ScanResult {
    stage_names: Vec<String>,
    max_in_flight: u32,
    thread_ids: HashSet<u32>,
    total_cycles: u64,
}

/// Open a file for reading, auto-detecting gzip.
fn open_input(path: &str) -> io::Result<Box<dyn BufRead>> {
    if path.ends_with(".gz") {
        let file = File::open(path)?;
        let decoder = GzDecoder::new(file);
        Ok(Box::new(BufReader::new(decoder)))
    } else {
        let file = File::open(path)?;
        Ok(Box::new(BufReader::new(file)))
    }
}

/// Pass 1: Scan the Konata log to discover metadata.
fn scan_pass(path: &str) -> io::Result<ScanResult> {
    let reader = open_input(path)?;
    let mut stage_names: Vec<String> = Vec::new();
    let mut stage_set: HashSet<String> = HashSet::new();
    let mut thread_ids: HashSet<u32> = HashSet::new();
    let mut in_flight: HashSet<u32> = HashSet::new();
    let mut max_in_flight: u32 = 0;
    let mut current_cycle: u64 = 0;

    for line in reader.lines() {
        let line = line?;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(cmd) = parse_line(&line) {
            match cmd {
                KonataCmd::CycleAbsolute(c) => current_cycle = c,
                KonataCmd::CycleDelta(d) => current_cycle += d,
                KonataCmd::Instruction { id, tid, .. } => {
                    in_flight.insert(id);
                    max_in_flight = max_in_flight.max(in_flight.len() as u32);
                    thread_ids.insert(tid);
                }
                KonataCmd::StageStart { stage, lane, .. } if lane == 0 => {
                    if !stage_set.contains(&stage) {
                        stage_set.insert(stage.clone());
                        stage_names.push(stage);
                    }
                }
                KonataCmd::Retire { id, .. } => {
                    in_flight.remove(&id);
                }
                _ => {}
            }
        }
    }

    Ok(ScanResult {
        stage_names,
        max_in_flight: max_in_flight.max(1),
        thread_ids,
        total_cycles: current_cycle,
    })
}

/// Extract PC from a disassembly label if it starts with a hex address.
fn extract_pc(text: &str) -> u64 {
    let trimmed = text.trim();
    // Try to parse the first whitespace-delimited token as a hex address
    if let Some(first_word) = trimmed.split_whitespace().next() {
        // Strip optional "0x" prefix
        let hex_str = first_word.strip_prefix("0x").unwrap_or(first_word);
        if let Ok(pc) = u64::from_str_radix(hex_str, 16) {
            // Sanity check: the hex string should be at least 4 chars
            if hex_str.len() >= 4 {
                return pc;
            }
        }
    }
    0
}

/// Pass 2: Emit uscope trace.
fn emit_pass(
    input_path: &str,
    output_path: &str,
    scan: &ScanResult,
    clock_period_ps: u64,
    dut_name: &str,
) -> io::Result<()> {
    // Build stage name references
    let stage_refs: Vec<&str> = scan.stage_names.iter().map(|s| s.as_str()).collect();

    // Build schema
    let entity_slots = scan.max_in_flight.next_power_of_two().max(16);
    let (dut_builder, mut sb, ids) = CpuSchemaBuilder::new(dut_name)
        .pipeline_stages(&stage_refs)
        .entity_slots(entity_slots as u16)
        .counter("committed_insns")
        .build();

    let dut = dut_builder.build(sb.strings_mut());
    let schema = sb.build();

    // Build stage name → index map
    let stage_map: HashMap<&str, u8> = scan
        .stage_names
        .iter()
        .enumerate()
        .map(|(i, s)| (s.as_str(), i as u8))
        .collect();

    // Open writer
    let checkpoint_interval = clock_period_ps * 1000; // every 1000 cycles
    let file = File::create(output_path)?;
    let mut w = Writer::create(file, &dut, &schema, checkpoint_interval)?;
    let cpu = CpuWriter::new(ids.clone());

    // Read input and emit
    let reader = open_input(input_path)?;
    let mut current_cycle: u64 = 0;
    let mut cycle_started = false;
    let mut last_time_ps: u64 = 0;

    // Track entity state: id → (pc, has been fetched)
    let mut entity_pc: HashMap<u32, u64> = HashMap::new();

    for line in reader.lines() {
        let line = line?;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some(cmd) = parse_line(&line) {
            match cmd {
                KonataCmd::CycleAbsolute(c) => {
                    if cycle_started {
                        w.end_cycle()?;
                        cycle_started = false;
                    }
                    current_cycle = c;
                }
                KonataCmd::CycleDelta(d) => {
                    if cycle_started {
                        w.end_cycle()?;
                        cycle_started = false;
                    }
                    current_cycle += d;
                }
                KonataCmd::Instruction { id, .. } => {
                    let time_ps = current_cycle * clock_period_ps;
                    if !cycle_started || time_ps != last_time_ps {
                        if cycle_started {
                            w.end_cycle()?;
                        }
                        w.begin_cycle(time_ps);
                        cycle_started = true;
                        last_time_ps = time_ps;
                    }
                    // Fetch: allocate entity
                    cpu.fetch(&mut w, id, 0, 0);
                    entity_pc.insert(id, 0);
                }
                KonataCmd::Label { id, label_type, text } => {
                    let time_ps = current_cycle * clock_period_ps;
                    if !cycle_started || time_ps != last_time_ps {
                        if cycle_started {
                            w.end_cycle()?;
                        }
                        w.begin_cycle(time_ps);
                        cycle_started = true;
                        last_time_ps = time_ps;
                    }

                    if label_type == 0 {
                        // Disassembly: extract PC and set it
                        let pc = extract_pc(&text);
                        if pc != 0 {
                            w.slot_set(
                                ids.entities_storage_id,
                                id as u16,
                                ids.field_pc,
                                pc,
                            );
                            entity_pc.insert(id, pc);
                        }
                    }
                    // All label types become annotations
                    cpu.annotate(&mut w, id, &text);
                }
                KonataCmd::StageStart { id, lane, stage } => {
                    let time_ps = current_cycle * clock_period_ps;
                    if !cycle_started || time_ps != last_time_ps {
                        if cycle_started {
                            w.end_cycle()?;
                        }
                        w.begin_cycle(time_ps);
                        cycle_started = true;
                        last_time_ps = time_ps;
                    }

                    if lane == 0 {
                        if let Some(&stage_idx) = stage_map.get(stage.as_str()) {
                            cpu.stage_transition(&mut w, id, stage_idx);
                        }
                    } else {
                        // Lane 1+ → annotate with stall/overlay info
                        cpu.annotate(&mut w, id, &format!("stall:{}", stage));
                    }
                }
                KonataCmd::StageEnd { .. } => {
                    // Stage ends don't need explicit tracking in uscope
                    // (next stage_transition or retire implicitly ends the previous)
                }
                KonataCmd::Retire { id, retire_type, .. } => {
                    let time_ps = current_cycle * clock_period_ps;
                    if !cycle_started || time_ps != last_time_ps {
                        if cycle_started {
                            w.end_cycle()?;
                        }
                        w.begin_cycle(time_ps);
                        cycle_started = true;
                        last_time_ps = time_ps;
                    }

                    if retire_type == 0 {
                        // Normal retirement
                        cpu.retire(&mut w, id);
                        cpu.counter_add(&mut w, "committed_insns", 1);
                    } else {
                        // Flush
                        cpu.flush(&mut w, id, 0); // mispredict
                    }
                    entity_pc.remove(&id);
                }
                KonataCmd::Dependency { consumer, producer, .. } => {
                    let time_ps = current_cycle * clock_period_ps;
                    if !cycle_started || time_ps != last_time_ps {
                        if cycle_started {
                            w.end_cycle()?;
                        }
                        w.begin_cycle(time_ps);
                        cycle_started = true;
                        last_time_ps = time_ps;
                    }

                    // dep_type in konata isn't standardized; map to raw(0)
                    cpu.dependency(&mut w, producer, consumer, 0);
                }
            }
        }
    }

    if cycle_started {
        w.end_cycle()?;
    }

    w.close()?;
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: konata2uscope <input.log[.gz]> -o <output.uscope> [--clock-period-ps <ps>] [--dut-name <name>]");
        std::process::exit(1);
    }

    let mut input_path = String::new();
    let mut output_path = String::from("output.uscope");
    let mut clock_period_ps: u64 = 1000;
    let mut dut_name = String::from("core0");

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-o" => {
                i += 1;
                output_path = args[i].clone();
            }
            "--clock-period-ps" => {
                i += 1;
                clock_period_ps = args[i].parse().expect("invalid clock period");
            }
            "--dut-name" => {
                i += 1;
                dut_name = args[i].clone();
            }
            _ => {
                if input_path.is_empty() {
                    input_path = args[i].clone();
                } else {
                    eprintln!("Unknown argument: {}", args[i]);
                    std::process::exit(1);
                }
            }
        }
        i += 1;
    }

    if input_path.is_empty() {
        eprintln!("Error: no input file specified");
        std::process::exit(1);
    }

    eprintln!("Pass 1: scanning {}...", input_path);
    let scan = scan_pass(&input_path).expect("failed to scan input");
    eprintln!(
        "  {} stages: [{}]",
        scan.stage_names.len(),
        scan.stage_names.join(", ")
    );
    eprintln!("  max in-flight: {}", scan.max_in_flight);
    eprintln!("  threads: {}", scan.thread_ids.len());
    eprintln!("  total cycles: {}", scan.total_cycles);

    eprintln!("Pass 2: emitting {}...", output_path);
    emit_pass(&input_path, &output_path, &scan, clock_period_ps, &dut_name)
        .expect("failed to emit uscope trace");

    eprintln!("Done.");
}
