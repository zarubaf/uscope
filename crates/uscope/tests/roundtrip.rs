/// Integration test: write a trace file, read it back, verify round-trip.

use uscope::protocols::cpu::{CpuSchemaBuilder, CpuWriter};
use uscope::reader::Reader;
use uscope::types::*;
use uscope::writer::Writer;

#[test]
fn write_then_read_roundtrip() {
    // Build CPU schema
    let (dut_builder, mut sb, ids) = CpuSchemaBuilder::new("core0")
        .isa("RV64GC")
        .pipeline_stages(&["fetch", "decode", "execute", "writeback"])
        .fetch_width(2)
        .commit_width(2)
        .entity_slots(16)
        .counter("committed_insns")
        .build();

    let dut = dut_builder.build(sb.strings_mut());
    let schema = sb.build();

    // Write trace to a temp file
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.uscope");
    let path_str = path.to_str().unwrap();

    {
        let file = std::fs::File::create(&path).unwrap();
        let mut w = Writer::create(file, &dut, &schema, 5_000).unwrap();
        let cpu = CpuWriter::new(ids.clone());

        // Simulate 3 instructions pipelined: each fetched 1 cycle apart
        // Cycle 0: fetch inst 0
        w.begin_cycle(0);
        cpu.fetch(&mut w, 0, 0x8000_0000, 0x13);
        cpu.stage_transition(&mut w, 0, 0);
        cpu.annotate(&mut w, 0, "nop #0");
        w.end_cycle().unwrap();

        // Cycle 1: decode inst 0, fetch inst 1
        w.begin_cycle(1000);
        cpu.stage_transition(&mut w, 0, 1);
        cpu.fetch(&mut w, 1, 0x8000_0004, 0x13);
        cpu.stage_transition(&mut w, 1, 0);
        cpu.annotate(&mut w, 1, "nop #1");
        cpu.dependency(&mut w, 0, 1, 0); // RAW dependency
        w.end_cycle().unwrap();

        // Cycle 2: execute inst 0, decode inst 1, fetch inst 2
        w.begin_cycle(2000);
        cpu.stage_transition(&mut w, 0, 2);
        cpu.stage_transition(&mut w, 1, 1);
        cpu.fetch(&mut w, 2, 0x8000_0008, 0x13);
        cpu.stage_transition(&mut w, 2, 0);
        cpu.annotate(&mut w, 2, "nop #2");
        w.end_cycle().unwrap();

        // Cycle 3: writeback+retire inst 0, execute inst 1, decode inst 2
        w.begin_cycle(3000);
        cpu.stage_transition(&mut w, 0, 3);
        cpu.retire(&mut w, 0);
        cpu.counter_add(&mut w, "committed_insns", 1);
        cpu.stage_transition(&mut w, 1, 2);
        cpu.stage_transition(&mut w, 2, 1);
        w.end_cycle().unwrap();

        // Cycle 4: writeback+retire inst 1, execute inst 2
        w.begin_cycle(4000);
        cpu.stage_transition(&mut w, 1, 3);
        cpu.retire(&mut w, 1);
        cpu.counter_add(&mut w, "committed_insns", 1);
        cpu.stage_transition(&mut w, 2, 2);
        w.end_cycle().unwrap();

        // Cycle 5: writeback+retire inst 2
        w.begin_cycle(5000);
        cpu.stage_transition(&mut w, 2, 3);
        cpu.retire(&mut w, 2);
        cpu.counter_add(&mut w, "committed_insns", 1);
        w.end_cycle().unwrap();

        w.close().unwrap();
    }

    // Read back and verify
    let mut reader = Reader::open(path_str).unwrap();

    // Header checks
    let header = reader.header();
    assert!(header.flags & F_COMPLETE != 0);
    assert_eq!(header.total_time_ps, 5000);
    assert!(header.num_segments >= 1);

    // Schema checks
    let schema = reader.schema();
    assert_eq!(schema.clock_domains.len(), 1);
    assert_eq!(schema.scopes.len(), 2);
    assert_eq!(schema.storages.len(), 2); // entities + committed_insns

    // DUT properties
    assert_eq!(reader.dut_property("cpu.isa"), Some("RV64GC"));
    assert_eq!(reader.dut_property("cpu.fetch_width"), Some("2"));

    // State at time 1500: inst 0 in decode, inst 1 fetched, inst 2 not yet
    let state = reader.state_at(1500).unwrap();
    let offsets = &reader.field_offsets()[ids.entities_storage_id as usize];

    // inst 0 should be valid
    assert!(state.slot_valid(ids.entities_storage_id, 0));
    let pc = state.slot_field(ids.entities_storage_id, 0, ids.field_pc, offsets);
    assert_eq!(pc, 0x8000_0000);

    // inst 1 should be valid (fetched at 1000)
    assert!(state.slot_valid(ids.entities_storage_id, 1));

    // inst 2 not yet fetched at 1500
    assert!(!state.slot_valid(ids.entities_storage_id, 2));

    // Events in range 0..5000
    let events = reader.events_in_range(0, 5000).unwrap();
    assert!(!events.is_empty());

    // Count stage transitions: 3 instructions * 4 stages = 12
    let stage_transitions: Vec<_> = events
        .iter()
        .filter(|e| e.event_type_id == ids.stage_transition_event_id)
        .collect();
    assert_eq!(stage_transitions.len(), 12);

    // Count annotations: 3
    let annotations: Vec<_> = events
        .iter()
        .filter(|e| e.event_type_id == ids.annotate_event_id)
        .collect();
    assert_eq!(annotations.len(), 3);

    // Check string table
    let st = reader.string_table().unwrap();
    assert!(st.get(0).unwrap().starts_with("nop #"));

    // Verify dependency event
    let deps: Vec<_> = events
        .iter()
        .filter(|e| e.event_type_id == ids.dependency_event_id)
        .collect();
    assert_eq!(deps.len(), 1);

    // State at time 5000: all instructions retired
    let state2 = reader.state_at(5000).unwrap();
    for i in 0..3u16 {
        assert!(
            !state2.slot_valid(ids.entities_storage_id, i),
            "inst {} should be retired at t=5000",
            i
        );
    }
}
