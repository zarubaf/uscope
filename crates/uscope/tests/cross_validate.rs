/// Cross-validation test: reads a trace file written by the C DPI library
/// and verifies its contents match expectations.

use uscope::reader::Reader;
use uscope::types::*;

/// This test reads the file produced by `dpi/test_write`.
/// Run `make -C dpi test` before running this test.
#[test]
fn read_c_writer_output() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../dpi/test_output.uscope");

    // Skip if the file doesn't exist (C test not run yet)
    if !std::path::Path::new(path).exists() {
        eprintln!("Skipping cross_validate: run `make -C dpi test` first");
        return;
    }

    let mut reader = Reader::open(path).unwrap();

    // Header checks
    let header = reader.header();
    assert!(header.flags & F_COMPLETE != 0, "F_COMPLETE not set");
    assert_eq!(header.total_time_ps, 4000);
    assert!(header.num_segments >= 1);

    // Schema
    let schema = reader.schema();
    assert_eq!(schema.clock_domains.len(), 1);
    assert_eq!(schema.clock_domains[0].period_ps, 1000);
    assert_eq!(schema.scopes.len(), 2); // root + core0
    assert_eq!(schema.storages.len(), 2); // entities + committed_insns
    assert_eq!(schema.events.len(), 2); // stage_transition + annotate

    // Verify entities storage
    assert_eq!(schema.storages[0].num_slots, 16);
    assert!(schema.storages[0].flags & SF_SPARSE != 0);
    assert_eq!(schema.storages[0].fields.len(), 3);

    // DUT properties
    assert_eq!(reader.dut_property("dut_name"), Some("test_core_c"));
    assert_eq!(reader.dut_property("cpu.isa"), Some("RV64GC"));

    // State at t=1500: inst 0 should be valid (in decode), inst 1 should be valid (fetched)
    let state = reader.state_at(1500).unwrap();
    let offsets = &reader.field_offsets()[0]; // entities storage

    assert!(state.slot_valid(0, 0), "inst 0 should be valid at t=1500");
    let pc0 = state.slot_field(0, 0, 1, offsets); // field 1 = pc
    assert_eq!(pc0, 0x80000000, "inst 0 pc should be 0x80000000");

    assert!(state.slot_valid(0, 1), "inst 1 should be valid at t=1500");
    let pc1 = state.slot_field(0, 1, 1, offsets);
    assert_eq!(pc1, 0x80000004, "inst 1 pc should be 0x80000004");

    // State at t=4000: both retired
    let state2 = reader.state_at(4000).unwrap();
    assert!(!state2.slot_valid(0, 0), "inst 0 should be retired at t=4000");
    assert!(!state2.slot_valid(0, 1), "inst 1 should be retired at t=4000");

    // Events: should have stage transitions
    let events = reader.events_in_range(0, 4000).unwrap();
    let stage_transitions: Vec<_> = events
        .iter()
        .filter(|e| e.event_type_id == 0) // stage_transition
        .collect();
    // 2 instructions * 4 stages = 8
    assert_eq!(stage_transitions.len(), 8, "expected 8 stage transitions");

    // String table should have annotation
    let st = reader.string_table().unwrap();
    assert_eq!(st.get(0), Some("addi x0, x0, 0"));

    // Konata converter output test
    let konata_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../test_data/sample.uscope");
    if std::path::Path::new(konata_path).exists() {
        let mut kr = Reader::open(konata_path).unwrap();
        let kh = kr.header();
        assert!(kh.flags & F_COMPLETE != 0, "konata output not finalized");
        assert!(kh.num_segments >= 1);

        let ks = kr.schema();
        // Should have pipeline stages from the sample log
        assert!(!ks.enums.is_empty(), "should have enums");

        // Check events exist
        let events = kr.events_in_range(0, kh.total_time_ps).unwrap();
        assert!(!events.is_empty(), "konata output should have events");
    }

    eprintln!("Cross-validation passed!");
}
