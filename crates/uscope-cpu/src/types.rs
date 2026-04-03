use std::ops::Range;

/// Interned stage name index.
pub type StageNameIdx = u16;

/// Information about a buffer storage detected from the uscope schema.
#[derive(Debug, Clone)]
pub struct BufferInfo {
    pub name: String,
    pub storage_id: u16,
    pub capacity: u16,
    /// Fields defined on this buffer: (name, field_type as u8).
    pub fields: Vec<(String, u8)>,
    /// Storage-level property definitions with pointer-pair metadata. v0.3.
    pub properties: Vec<BufferPropertyDef>,
}

/// A storage-level property definition with pointer-pair metadata.
#[derive(Debug, Clone)]
pub struct BufferPropertyDef {
    pub name: String,
    pub field_type: u8,
    /// 0=plain, 1=HEAD_PTR, 2=TAIL_PTR.
    pub role: u8,
    /// Pointer pair grouping (head/tail with same pair_id form a pair).
    pub pair_id: u8,
}

/// A single stage span within an instruction's pipeline execution.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct StageSpan {
    pub stage_name_idx: StageNameIdx,
    pub lane: u8,
    pub _pad: u8,
    pub start_cycle: u32,
    pub end_cycle: u32,
}

/// Dependency relationship between two instructions.
#[derive(Debug, Clone, Copy)]
pub struct Dependency {
    pub producer: u32,
    pub consumer: u32,
    pub kind: DepKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepKind {
    Data,
    Control,
    Memory,
}

/// Retirement status of an instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetireStatus {
    Retired,
    Flushed,
    InFlight,
}

/// Per-instruction data.
#[derive(Debug, Clone)]
pub struct InstructionData {
    pub id: u32,
    pub sim_id: u64,
    pub thread_id: u16,
    /// Retire buffer ID (slot in the retire queue). `None` if not yet allocated.
    pub rbid: Option<u32>,
    /// Issue queue ID (index into cpu.issue_queue_names). `None` if unknown.
    pub iq_id: Option<u32>,
    /// Dispatch queue ID (index into cpu.dispatch_queue_names). `None` if unknown.
    pub dq_id: Option<u32>,
    /// Cycle at which the instruction became ready in the issue queue. `None` if not yet ready.
    pub ready_cycle: Option<u32>,
    pub disasm: String,
    pub tooltip: String,
    pub stage_range: Range<u32>,
    pub retire_status: RetireStatus,
    pub first_cycle: u32,
    pub last_cycle: u32,
}

/// Display mode for a performance counter value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CounterDisplayMode {
    /// Raw cumulative value.
    Total,
    /// Delta / window_size (e.g., IPC).
    Rate,
    /// Single-cycle change.
    Delta,
}

/// A single performance counter time-series.
#[derive(Debug, Clone)]
pub struct CounterSeries {
    /// Display name (scope-qualified if multi-scope trace).
    pub name: String,
    /// Sparse counter samples: (cycle, cumulative_value) pairs.
    /// One entry per segment boundary (from checkpoint data).
    /// Sorted by cycle.
    pub samples: Vec<(u32, u64)>,
    /// Default display mode.
    pub default_mode: CounterDisplayMode,
}

/// Lightweight index mapping segment indices to their cycle ranges.
/// Built on load from uscope segment time bounds; enables binary search
/// for "which segments cover cycles N..M?" in future lazy-loading phases.
#[derive(Debug, Clone, Default)]
pub struct SegmentIndex {
    /// (start_cycle, end_cycle) per segment, ordered by segment index.
    pub segments: Vec<(u32, u32)>,
}

impl SegmentIndex {
    /// Find segment indices that overlap the given cycle range.
    pub fn segments_in_range(&self, start_cycle: u32, end_cycle: u32) -> Vec<usize> {
        self.segments
            .iter()
            .enumerate()
            .filter(|(_, (seg_start, seg_end))| *seg_start < end_cycle && *seg_end > start_cycle)
            .map(|(idx, _)| idx)
            .collect()
    }
}

/// File-level information about a uscope trace.
#[derive(Debug, Clone)]
pub struct FileInfo {
    /// Format version string (e.g. "0.3").
    pub version: String,
    /// Number of segments in the trace.
    pub segment_count: usize,
    /// Total number of instructions (from TraceSummary, if available).
    pub total_instructions: u64,
    /// Total trace duration in cycles.
    pub max_cycle: u32,
    /// Clock period in picoseconds.
    pub period_ps: u64,
    /// Key-value metadata from the trace source (DUT properties, format info, etc.).
    pub metadata: Vec<(String, String)>,
}

/// Occupied buffer slot: (slot_index, buffer_field_values, entity_field_name_value_pairs).
pub type BufferSlot = (u16, Vec<u64>, Vec<(String, u64)>);

/// Property value with pointer-pair metadata.
#[derive(Debug, Clone)]
pub struct PropertyValue {
    pub name: String,
    pub value: u64,
    /// 0=plain, 1=HEAD_PTR, 2=TAIL_PTR.
    pub role: u8,
    pub pair_id: u8,
}

/// Result of querying buffer state at a cycle.
#[derive(Debug, Clone, Default)]
pub struct BufferState {
    pub slots: Vec<BufferSlot>,
    pub properties: Vec<PropertyValue>,
    pub capacity: u16,
}
