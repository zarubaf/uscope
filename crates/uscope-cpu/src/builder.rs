use crate::types::{RetireStatus, StageSpan};

/// Transient builder for an instruction being reconstructed from segment replay.
pub struct InstrBuilder {
    pub entity_id: u32,
    pub reflex_id: u32,
    pub pc: u64,
    pub inst_bits: Option<u32>,
    pub rbid: Option<u32>,
    pub iq_id: Option<u32>,
    pub dq_id: Option<u32>,
    pub ready_time_ps: Option<u64>,
    pub has_disasm_annotation: bool,
    pub disasm: String,
    pub tooltip: String,
    pub stages: Vec<StageSpan>,
    /// (stage_name_idx, start_cycle) for the currently open stage.
    pub current_stage: Option<(u16, u32)>,
    pub first_cycle: u32,
    pub last_cycle: u32,
    pub retire_status: RetireStatus,
}

impl InstrBuilder {
    pub fn new(entity_id: u32, reflex_id: u32, pc: u64, cycle: u32) -> Self {
        Self {
            entity_id,
            reflex_id,
            pc,
            inst_bits: None,
            rbid: None,
            iq_id: None,
            dq_id: None,
            ready_time_ps: None,
            has_disasm_annotation: false,
            disasm: format!("0x{:08x}", pc),
            tooltip: String::new(),
            stages: Vec::new(),
            current_stage: None,
            first_cycle: cycle,
            last_cycle: cycle,
            retire_status: RetireStatus::InFlight,
        }
    }

    pub fn close_current_stage(&mut self, end_cycle: u32) {
        if let Some((stage_idx, start)) = self.current_stage.take() {
            self.stages.push(StageSpan {
                stage_name_idx: stage_idx,
                lane: 0,
                _pad: 0,
                start_cycle: start,
                end_cycle,
            });
            if end_cycle > self.last_cycle {
                self.last_cycle = end_cycle;
            }
        }
    }

    pub fn open_stage(&mut self, stage_name_idx: u16, cycle: u32) {
        self.close_current_stage(cycle);
        self.current_stage = Some((stage_name_idx, cycle));
    }
}

/// Detect whether an annotation looks like a disassembly line by checking if
/// it starts with a hex address that matches the entity's known PC.
/// Handles formats like "00001000: jal zero, 0x10" and "0x80000000 addi x1, x0, 1".
pub fn is_disasm_line(text: &str, pc: u64) -> bool {
    let trimmed = text.trim();
    if let Some(first_word) = trimmed.split_whitespace().next() {
        let word = first_word.strip_suffix(':').unwrap_or(first_word);
        let hex_str = word.strip_prefix("0x").unwrap_or(word);
        if hex_str.len() >= 4 {
            if let Ok(addr) = u64::from_str_radix(hex_str, 16) {
                return pc != 0 && addr == pc;
            }
        }
    }
    false
}
