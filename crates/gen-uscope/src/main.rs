/// gen-uscope: Generate synthetic µScope CPU pipeline traces for testing.
///
/// Usage: gen-uscope -o output.uscope [options]
use std::io::{self, Write as IoWrite};

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use uscope::protocols::cpu::{CpuSchemaBuilder, CpuWriter};
use uscope::summary::embed_counter_summary;
use uscope::writer::Writer;

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    let mut output = String::new();
    let mut num_instructions: u64 = 1_000_000;
    let mut num_counters: usize = 100;
    let mut num_stages: usize = 8;
    let mut fetch_width: u32 = 4;
    let mut clock_period: u32 = 1000;
    let mut seed: u64 = 42;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--output" => {
                i += 1;
                output = args.get(i).cloned().unwrap_or_default();
            }
            "--instructions" | "-n" => {
                i += 1;
                num_instructions = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(num_instructions);
            }
            "--counters" => {
                i += 1;
                num_counters = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(num_counters);
            }
            "--stages" => {
                i += 1;
                num_stages = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(num_stages);
            }
            "--fetch-width" => {
                i += 1;
                fetch_width = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(fetch_width);
            }
            "--clock-period" => {
                i += 1;
                clock_period = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(clock_period);
            }
            "--seed" => {
                i += 1;
                seed = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(seed);
            }
            "-h" | "--help" => {
                eprintln!("gen-uscope: Generate synthetic µScope CPU pipeline traces");
                eprintln!();
                eprintln!("Usage: gen-uscope -o output.uscope [options]");
                eprintln!("  -o, --output FILE       Output file (required)");
                eprintln!("  -n, --instructions N    Instructions (default: 1000000)");
                eprintln!("  --counters N            Counters (default: 100)");
                eprintln!("  --stages N              Pipeline stages (default: 8)");
                eprintln!("  --fetch-width N         Superscalar width (default: 4)");
                eprintln!("  --clock-period N        Clock period in ps (default: 1000)");
                eprintln!("  --seed N                RNG seed (default: 42)");
                return Ok(());
            }
            _ => {
                eprintln!("Unknown argument: {}", args[i]);
                std::process::exit(1);
            }
        }
        i += 1;
    }

    if output.is_empty() {
        eprintln!("Error: -o output.uscope is required");
        std::process::exit(1);
    }

    let mut rng = StdRng::seed_from_u64(seed);

    // Pipeline stage names.
    let short_names = ["Fe", "De", "Rn", "Ds", "Is", "Ex", "Cp", "Rt", "Wb", "Cm"];
    let stage_names: Vec<String> = (0..num_stages)
        .map(|i| {
            if i < short_names.len() {
                short_names[i].to_string()
            } else {
                format!("S{}", i)
            }
        })
        .collect();

    // Counter names.
    let predefined = [
        "committed_insns",
        "cycles",
        "retired_insns",
        "mispredicts",
        "dcache_misses",
        "icache_misses",
        "dtlb_misses",
        "itlb_misses",
        "rob_full_stalls",
        "iq_full_stalls",
        "dq_full_stalls",
        "lsq_full_stalls",
        "fetch_bubbles",
        "decode_stalls",
        "br_taken",
        "br_not_taken",
        "load_ops",
        "store_ops",
        "alu_ops",
        "fp_ops",
    ];
    let counter_names: Vec<String> = (0..num_counters)
        .map(|ci| {
            if ci < predefined.len() {
                predefined[ci].to_string()
            } else {
                format!("counter_{}", ci)
            }
        })
        .collect();

    // Build schema.
    let stage_refs: Vec<&str> = stage_names.iter().map(|s| s.as_str()).collect();
    let mut builder = CpuSchemaBuilder::new("cpu0")
        .pipeline_stages(&stage_refs)
        .fetch_width(fetch_width)
        .entity_slots(512);

    for name in &counter_names {
        builder = builder.counter(name);
    }

    let (dut_builder, mut sb, ids) = builder.build();
    let dut = dut_builder.build(sb.strings_mut());
    let schema = sb.build();

    // Create writer.
    let checkpoint_interval = clock_period as u64 * 10_000; // checkpoint every 10k cycles
    let file = std::fs::File::create(&output)?;
    let buf = std::io::BufWriter::new(file);
    let mut w = Writer::create(buf, &dut, &schema, checkpoint_interval)?;
    let cpu = CpuWriter::new(ids);

    eprintln!(
        "Generating {} instructions, {} counters, {} stages, width {}...",
        num_instructions, num_counters, num_stages, fetch_width
    );
    let start = std::time::Instant::now();

    // Per-counter rate parameters.
    let counter_rates: Vec<f64> = (0..num_counters).map(|_| rng.gen_range(0.1..5.0)).collect();
    let counter_burst: Vec<f64> = (0..num_counters)
        .map(|_| rng.gen_range(0.01..0.2))
        .collect();

    let segment_interval = 50_000u64;
    let mut instructions_in_segment = 0u64;
    let stall_prob = 0.10;
    let flush_prob = 0.01;
    let mut cycle: u64 = 0;
    let period = clock_period as u64;
    let mut next_entity_id: u32 = 0;
    let mut instr_idx = 0u64;

    // Helper: emit one cycle with all ops inside begin/end.
    macro_rules! emit_cycle {
        ($w:expr, $cycle:expr, $period:expr, $body:expr) => {{
            $w.begin_cycle($cycle * $period);
            $body;
            $w.end_cycle()?;
        }};
    }

    while instr_idx < num_instructions {
        let group_size = (num_instructions - instr_idx).min(fetch_width as u64);

        // Fetch cycle: create entities + counter updates.
        cycle += 1;
        w.begin_cycle(cycle * period);

        // Counter increments this cycle.
        for ci in 0..num_counters {
            let delta = if rng.gen_bool(counter_burst[ci].min(1.0)) {
                rng.gen_range(1..=(counter_rates[ci] * 10.0) as u64 + 1)
            } else if rng.gen_bool((counter_rates[ci] / 2.0).min(1.0)) {
                rng.gen_range(1..=(counter_rates[ci] * 2.0) as u64 + 1)
            } else {
                0
            };
            if delta > 0 {
                cpu.counter_add(&mut w, &counter_names[ci], delta);
            }
        }

        // Fetch group.
        let mut group_entities: Vec<(u32, bool, usize)> = Vec::new(); // (eid, is_flushed, flush_after)
        for g in 0..group_size {
            let eid = next_entity_id;
            next_entity_id = next_entity_id.wrapping_add(1);

            let pc = 0x80000000u64 + (instr_idx + g) * 4;
            let inst_bits = 0x00000013u32; // NOP
            let is_branch = rng.gen_bool(0.15);
            let is_flushed = is_branch && rng.gen_bool(flush_prob);
            let flush_after = if is_flushed {
                rng.gen_range(1..num_stages.max(2))
            } else {
                num_stages
            };

            cpu.fetch(&mut w, eid, pc, inst_bits);
            cpu.stage_transition(&mut w, eid, 0); // Enter first stage
            group_entities.push((eid, is_flushed, flush_after));
        }
        w.end_cycle()?;

        // Walk remaining pipeline stages for each entity.
        for (eid, is_flushed, flush_after) in &group_entities {
            for si in 1..num_stages {
                if si >= *flush_after {
                    break;
                }

                // Stage duration.
                let mut dur = 1u64;
                if rng.gen_bool(stall_prob) {
                    dur += rng.gen_range(1..=3);
                }
                if si == 5 {
                    dur += rng.gen_range(0..=2);
                }

                // Each duration tick is a cycle.
                for d in 0..dur {
                    cycle += 1;
                    w.begin_cycle(cycle * period);
                    if d == 0 {
                        cpu.stage_transition(&mut w, *eid, si as u8);
                    }
                    w.end_cycle()?;
                }
            }

            // Retire or flush (in its own cycle).
            cycle += 1;
            w.begin_cycle(cycle * period);
            if *is_flushed {
                cpu.flush(&mut w, *eid, 0);
            } else {
                cpu.retire(&mut w, *eid);
            }

            // Dependencies.
            if *eid > 0 && rng.gen_bool(0.3) {
                let producer = rng.gen_range(eid.saturating_sub(8)..*eid);
                cpu.dependency(&mut w, producer, *eid, 0);
            }
            w.end_cycle()?;
        }

        instr_idx += group_size;

        if instr_idx % 100_000 == 0 {
            eprint!(
                "\r  {:.0}% ({}/{})",
                instr_idx as f64 / num_instructions as f64 * 100.0,
                instr_idx,
                num_instructions
            );
            io::stderr().flush().ok();
        }
    }

    w.close()?;

    let elapsed = start.elapsed();
    let file_size = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0);
    eprintln!(
        "\rDone: {} instructions, {} cycles, {} counters in {:.2}s → {} ({:.1} MB)",
        num_instructions,
        cycle,
        num_counters,
        elapsed.as_secs_f64(),
        output,
        file_size as f64 / 1_048_576.0
    );

    // Compute and embed counter mipmaps inside the .uscope file.
    eprint!("Embedding counter mipmaps...");
    io::stderr().flush().ok();
    let mipmap_start = std::time::Instant::now();

    embed_counter_summary(&output, clock_period as u64)?;

    eprintln!(" done in {:.2}s", mipmap_start.elapsed().as_secs_f64());

    Ok(())
}
