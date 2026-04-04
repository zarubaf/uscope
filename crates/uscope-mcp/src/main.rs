//! MCP (Model Context Protocol) server for uScope CPU pipeline traces.
//!
//! Exposes trace inspection tools over JSON-RPC 2.0 via stdio so that
//! Claude can query pipeline traces interactively.
//!
//! Usage:
//!     uscope-mcp --trace /path/to/file.uscope

use std::io::{self, BufRead, Write};

use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uscope::types::FieldType;
use uscope_cpu::types::RetireStatus;
use uscope_cpu::CpuTrace;

// ── CLI ────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "uscope-mcp",
    version,
    about = "MCP server for uScope trace inspection"
)]
struct Cli {
    /// Path to a .uscope trace file
    #[arg(long)]
    trace: String,
}

// ── JSON-RPC types ─────────────────────────────────────────────────

#[derive(Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Serialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

impl JsonRpcResponse {
    fn success(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: Option<Value>, code: i64, message: String) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError { code, message }),
        }
    }
}

// ── Tool definitions ───────────────────────────────────────────────

fn tool_definitions() -> Value {
    json!([
        {
            "name": "file_info",
            "description": "Get trace file header, schema, segments, counters, and buffer metadata. Call this first to understand the trace structure.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "required": []
            }
        },
        {
            "name": "state_at_cycle",
            "description": "Get the state of all buffers at a specific cycle. Shows slot contents, pointer positions, and fill levels.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "cycle": {
                        "type": "number",
                        "description": "The cycle number to query"
                    }
                },
                "required": ["cycle"]
            }
        },
        {
            "name": "entity_timeline",
            "description": "Get the full lifecycle of an instruction entity: pipeline stages, disassembly, retire/flush status. Loads all segments to find the entity.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "entity_id": {
                        "type": "number",
                        "description": "The entity ID to look up"
                    }
                },
                "required": ["entity_id"]
            }
        },
        {
            "name": "counter_values",
            "description": "Get performance counter values over a cycle range. Returns cumulative values at sampled points within the range.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "counter": {
                        "type": "string",
                        "description": "Counter name (or substring to match)"
                    },
                    "start_cycle": {
                        "type": "number",
                        "description": "Start of the cycle range"
                    },
                    "end_cycle": {
                        "type": "number",
                        "description": "End of the cycle range"
                    }
                },
                "required": ["counter", "start_cycle", "end_cycle"]
            }
        },
        {
            "name": "buffer_occupancy",
            "description": "Get buffer state at a specific cycle: occupied slots, pointer positions, fill level. Use file_info first to discover buffer names.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "buffer": {
                        "type": "string",
                        "description": "Buffer name (or substring to match)"
                    },
                    "cycle": {
                        "type": "number",
                        "description": "The cycle number to query"
                    }
                },
                "required": ["buffer", "cycle"]
            }
        },
        {
            "name": "analyze_performance",
            "description": "Compute performance summary for a cycle range: instruction count, IPC, per-counter totals, and buffer occupancy snapshots.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "start_cycle": {
                        "type": "number",
                        "description": "Start of the cycle range"
                    },
                    "end_cycle": {
                        "type": "number",
                        "description": "End of the cycle range"
                    }
                },
                "required": ["start_cycle", "end_cycle"]
            }
        }
    ])
}

// ── Tool implementations ───────────────────────────────────────────

fn tool_file_info(trace: &CpuTrace) -> Value {
    let info = trace.file_info();
    let schema = trace.schema();

    let storages: Vec<Value> = schema
        .storages
        .iter()
        .map(|s| {
            let name = schema.get_string(s.name).unwrap_or("?");
            let fields: Vec<Value> = s
                .fields
                .iter()
                .map(|f| {
                    json!({
                        "name": schema.get_string(f.name).unwrap_or("?"),
                        "type": field_type_name(f.field_type)
                    })
                })
                .collect();
            let properties: Vec<Value> = s
                .properties
                .iter()
                .map(|p| {
                    json!({
                        "name": schema.get_string(p.name).unwrap_or("?"),
                        "type": field_type_name(p.field_type),
                        "role": role_name(p.role),
                        "pair_id": p.pair_id
                    })
                })
                .collect();
            let mut flags = Vec::new();
            if s.is_sparse() {
                flags.push("sparse");
            }
            if s.is_buffer() {
                flags.push("buffer");
            }
            json!({
                "name": name,
                "storage_id": s.storage_id,
                "num_slots": s.num_slots,
                "fields": fields,
                "properties": properties,
                "flags": flags
            })
        })
        .collect();

    let events: Vec<Value> = schema
        .events
        .iter()
        .map(|e| {
            let name = schema.get_string(e.name).unwrap_or("?");
            let fields: Vec<Value> = e
                .fields
                .iter()
                .map(|f| {
                    json!({
                        "name": schema.get_string(f.name).unwrap_or("?"),
                        "type": field_type_name(f.field_type)
                    })
                })
                .collect();
            json!({
                "name": name,
                "event_type_id": e.event_type_id,
                "fields": fields
            })
        })
        .collect();

    let counter_names: Vec<&str> = trace
        .counter_names()
        .iter()
        .map(|(_, name)| name.as_str())
        .collect();

    let buffer_names: Vec<Value> = trace
        .buffer_infos()
        .iter()
        .map(|b| {
            json!({
                "name": b.name,
                "capacity": b.capacity,
                "storage_id": b.storage_id,
                "fields": b.fields.iter().map(|(n, t)| json!({"name": n, "type": field_type_name(*t)})).collect::<Vec<_>>()
            })
        })
        .collect();

    let segment_ranges: Vec<Value> = trace
        .segment_index()
        .segments
        .iter()
        .enumerate()
        .map(|(i, (start, end))| json!({"index": i, "start_cycle": start, "end_cycle": end}))
        .collect();

    json!({
        "version": info.version,
        "segment_count": info.segment_count,
        "total_instructions": info.total_instructions,
        "max_cycle": info.max_cycle,
        "period_ps": info.period_ps,
        "metadata": info.metadata.iter().map(|(k, v)| json!({k: v})).collect::<Vec<_>>(),
        "stage_names": trace.stage_names(),
        "counter_names": counter_names,
        "buffers": buffer_names,
        "segments": segment_ranges,
        "schema": {
            "storages": storages,
            "events": events
        }
    })
}

fn tool_state_at_cycle(trace: &mut CpuTrace, params: &Value) -> Result<Value, String> {
    let cycle = params
        .get("cycle")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "missing or invalid 'cycle' parameter".to_string())? as u32;

    let buffer_infos: Vec<(String, usize)> = trace
        .buffer_infos()
        .iter()
        .enumerate()
        .map(|(i, b)| (b.name.clone(), i))
        .collect();

    let mut buffers = Vec::new();
    for (name, idx) in &buffer_infos {
        let state = trace.buffer_state_at(*idx, cycle);
        let slots: Vec<Value> = state
            .slots
            .iter()
            .map(|(slot, fields, entity_fields)| {
                let ef: Vec<Value> = entity_fields
                    .iter()
                    .map(|(k, v)| json!({"name": k, "value": v}))
                    .collect();
                json!({
                    "slot": slot,
                    "fields": fields,
                    "entity_fields": ef
                })
            })
            .collect();
        let properties: Vec<Value> = state
            .properties
            .iter()
            .map(|p| {
                json!({
                    "name": p.name,
                    "value": p.value,
                    "role": role_name(p.role)
                })
            })
            .collect();
        buffers.push(json!({
            "name": name,
            "capacity": state.capacity,
            "occupied": state.slots.len(),
            "fill_percent": if state.capacity > 0 {
                (state.slots.len() as f64 / state.capacity as f64) * 100.0
            } else {
                0.0
            },
            "slots": slots,
            "properties": properties
        }));
    }

    Ok(json!({
        "cycle": cycle,
        "buffers": buffers
    }))
}

fn tool_entity_timeline(trace: &mut CpuTrace, params: &Value) -> Result<Value, String> {
    let entity_id = params
        .get("entity_id")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "missing or invalid 'entity_id' parameter".to_string())?
        as u32;

    let seg_count = trace.segment_count();
    let all_indices: Vec<usize> = (0..seg_count).collect();

    let result = trace
        .load_segments(&all_indices)
        .map_err(|e| format!("failed to load segments: {}", e))?;

    let stage_names = trace.stage_names().to_vec();

    let matches: Vec<_> = result
        .instructions
        .iter()
        .filter(|inst| inst.id == entity_id)
        .collect();

    if matches.is_empty() {
        return Err(format!("entity {} not found in trace", entity_id));
    }

    let mut entities = Vec::new();
    for inst in &matches {
        let stages: Vec<Value> = (inst.stage_range.start..inst.stage_range.end)
            .filter_map(|si| {
                result.stages.get(si as usize).map(|s| {
                    let name = stage_names
                        .get(s.stage_name_idx as usize)
                        .cloned()
                        .unwrap_or_else(|| format!("stage_{}", s.stage_name_idx));
                    json!({
                        "name": name,
                        "start_cycle": s.start_cycle,
                        "end_cycle": s.end_cycle,
                        "duration": s.end_cycle.saturating_sub(s.start_cycle)
                    })
                })
            })
            .collect();

        entities.push(json!({
            "entity_id": inst.id,
            "disasm": inst.disasm,
            "first_cycle": inst.first_cycle,
            "last_cycle": inst.last_cycle,
            "total_latency": inst.last_cycle.saturating_sub(inst.first_cycle),
            "retire_status": retire_status_str(&inst.retire_status),
            "stages": stages,
            "tooltip": inst.tooltip
        }));
    }

    Ok(json!({
        "entity_id": entity_id,
        "results": entities
    }))
}

fn tool_counter_values(trace: &CpuTrace, params: &Value) -> Result<Value, String> {
    let counter_name = params
        .get("counter")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing or invalid 'counter' parameter".to_string())?;
    let start_cycle = params
        .get("start_cycle")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "missing or invalid 'start_cycle' parameter".to_string())?
        as u32;
    let end_cycle = params
        .get("end_cycle")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "missing or invalid 'end_cycle' parameter".to_string())?
        as u32;

    // Find matching counters
    let matching: Vec<(usize, &str)> = trace
        .counter_names()
        .iter()
        .enumerate()
        .filter(|(_, (_, name))| name.contains(counter_name))
        .map(|(i, (_, name))| (i, name.as_str()))
        .collect();

    if matching.is_empty() {
        return Err(format!("no counters matching '{}'", counter_name));
    }

    let mut counters = Vec::new();
    for (idx, name) in &matching {
        // Sample at reasonable intervals to avoid huge responses
        let range = end_cycle.saturating_sub(start_cycle);
        let step = if range > 1000 { range / 1000 } else { 1 };

        let mut values = Vec::new();
        let mut cycle = start_cycle;
        while cycle <= end_cycle {
            let val = trace.counter_value_at(*idx, cycle);
            values.push(json!({"cycle": cycle, "value": val}));
            cycle = cycle.saturating_add(step);
        }
        // Ensure we include the end cycle
        if values.last().map(|v| v["cycle"].as_u64().unwrap_or(0)) != Some(end_cycle as u64) {
            let val = trace.counter_value_at(*idx, end_cycle);
            values.push(json!({"cycle": end_cycle, "value": val}));
        }

        let start_val = trace.counter_value_at(*idx, start_cycle);
        let end_val = trace.counter_value_at(*idx, end_cycle);
        let delta = end_val.wrapping_sub(start_val);

        counters.push(json!({
            "name": name,
            "start_value": start_val,
            "end_value": end_val,
            "delta": delta,
            "rate": if range > 0 { delta as f64 / range as f64 } else { 0.0 },
            "values": values
        }));
    }

    Ok(json!({
        "counter": counter_name,
        "start_cycle": start_cycle,
        "end_cycle": end_cycle,
        "counters": counters
    }))
}

fn tool_buffer_occupancy(trace: &mut CpuTrace, params: &Value) -> Result<Value, String> {
    let buffer_name = params
        .get("buffer")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing or invalid 'buffer' parameter".to_string())?;
    let cycle = params
        .get("cycle")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "missing or invalid 'cycle' parameter".to_string())? as u32;

    let matching: Vec<(String, usize)> = trace
        .buffer_infos()
        .iter()
        .enumerate()
        .filter(|(_, b)| b.name.contains(buffer_name))
        .map(|(i, b)| (b.name.clone(), i))
        .collect();

    if matching.is_empty() {
        let available: Vec<&str> = trace
            .buffer_infos()
            .iter()
            .map(|b| b.name.as_str())
            .collect();
        return Err(format!(
            "no buffers matching '{}'. Available: {:?}",
            buffer_name, available
        ));
    }

    let mut buffers = Vec::new();
    for (name, idx) in &matching {
        let state = trace.buffer_state_at(*idx, cycle);
        let slots: Vec<Value> = state
            .slots
            .iter()
            .map(|(slot, fields, entity_fields)| {
                let ef: Vec<Value> = entity_fields
                    .iter()
                    .map(|(k, v)| json!({"name": k, "value": v}))
                    .collect();
                json!({
                    "slot": slot,
                    "fields": fields,
                    "entity_fields": ef
                })
            })
            .collect();
        let properties: Vec<Value> = state
            .properties
            .iter()
            .map(|p| {
                json!({
                    "name": p.name,
                    "value": p.value,
                    "role": role_name(p.role)
                })
            })
            .collect();

        // Compute pointer pair info
        let mut pointer_pairs = Vec::new();
        let mut heads: std::collections::HashMap<u8, (&str, u64)> =
            std::collections::HashMap::new();
        let mut tails: std::collections::HashMap<u8, (&str, u64)> =
            std::collections::HashMap::new();
        for p in &state.properties {
            match p.role {
                1 => {
                    heads.insert(p.pair_id, (&p.name, p.value));
                }
                2 => {
                    tails.insert(p.pair_id, (&p.name, p.value));
                }
                _ => {}
            }
        }
        for (pair_id, (head_name, head_val)) in &heads {
            if let Some((tail_name, tail_val)) = tails.get(pair_id) {
                pointer_pairs.push(json!({
                    "pair_id": pair_id,
                    "head": {"name": head_name, "value": head_val},
                    "tail": {"name": tail_name, "value": tail_val}
                }));
            }
        }

        buffers.push(json!({
            "name": name,
            "capacity": state.capacity,
            "occupied": state.slots.len(),
            "fill_percent": if state.capacity > 0 {
                (state.slots.len() as f64 / state.capacity as f64) * 100.0
            } else {
                0.0
            },
            "slots": slots,
            "properties": properties,
            "pointer_pairs": pointer_pairs
        }));
    }

    Ok(json!({
        "buffer": buffer_name,
        "cycle": cycle,
        "buffers": buffers
    }))
}

fn tool_analyze_performance(trace: &mut CpuTrace, params: &Value) -> Result<Value, String> {
    let start_cycle = params
        .get("start_cycle")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "missing or invalid 'start_cycle' parameter".to_string())?
        as u32;
    let end_cycle = params
        .get("end_cycle")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "missing or invalid 'end_cycle' parameter".to_string())?
        as u32;

    if start_cycle >= end_cycle {
        return Err("start_cycle must be less than end_cycle".to_string());
    }

    let range = end_cycle - start_cycle;

    // Load all segments that overlap this range to count instructions
    let seg_indices = trace
        .segment_index()
        .segments_in_range(start_cycle, end_cycle);
    let result = trace
        .load_segments(&seg_indices)
        .map_err(|e| format!("failed to load segments: {}", e))?;

    // Count instructions in range
    let instructions_in_range: Vec<_> = result
        .instructions
        .iter()
        .filter(|inst| inst.first_cycle >= start_cycle && inst.first_cycle < end_cycle)
        .collect();

    let total_instructions = instructions_in_range.len() as u64;
    let retired = instructions_in_range
        .iter()
        .filter(|inst| inst.retire_status == RetireStatus::Retired)
        .count() as u64;
    let flushed = instructions_in_range
        .iter()
        .filter(|inst| inst.retire_status == RetireStatus::Flushed)
        .count() as u64;
    let in_flight = instructions_in_range
        .iter()
        .filter(|inst| inst.retire_status == RetireStatus::InFlight)
        .count() as u64;

    let ipc = if range > 0 {
        retired as f64 / range as f64
    } else {
        0.0
    };

    // Counter summaries
    let counter_summaries: Vec<Value> = trace
        .counter_names()
        .iter()
        .enumerate()
        .map(|(idx, (_, name))| {
            let start_val = trace.counter_value_at(idx, start_cycle);
            let end_val = trace.counter_value_at(idx, end_cycle);
            let delta = end_val.wrapping_sub(start_val);
            let rate = if range > 0 {
                delta as f64 / range as f64
            } else {
                0.0
            };
            json!({
                "name": name,
                "start_value": start_val,
                "end_value": end_val,
                "delta": delta,
                "rate_per_cycle": rate
            })
        })
        .collect();

    // Buffer occupancy snapshots at start, mid, end
    let mid_cycle = start_cycle + range / 2;
    let sample_cycles = [start_cycle, mid_cycle, end_cycle];
    let buffer_infos: Vec<(String, usize)> = trace
        .buffer_infos()
        .iter()
        .enumerate()
        .map(|(i, b)| (b.name.clone(), i))
        .collect();

    let mut buffer_snapshots = Vec::new();
    for (name, idx) in &buffer_infos {
        let mut samples = Vec::new();
        for &c in &sample_cycles {
            let state = trace.buffer_state_at(*idx, c);
            let fill_pct = if state.capacity > 0 {
                (state.slots.len() as f64 / state.capacity as f64) * 100.0
            } else {
                0.0
            };
            samples.push(json!({
                "cycle": c,
                "occupied": state.slots.len(),
                "capacity": state.capacity,
                "fill_percent": fill_pct
            }));
        }
        buffer_snapshots.push(json!({
            "name": name,
            "samples": samples
        }));
    }

    // Stage latency statistics
    let stage_names = trace.stage_names().to_vec();
    let mut stage_totals: std::collections::HashMap<u16, (u64, u64)> =
        std::collections::HashMap::new(); // stage_idx -> (total_cycles, count)
    for inst in &instructions_in_range {
        for si in inst.stage_range.start..inst.stage_range.end {
            if let Some(s) = result.stages.get(si as usize) {
                let dur = s.end_cycle.saturating_sub(s.start_cycle) as u64;
                let entry = stage_totals.entry(s.stage_name_idx).or_insert((0, 0));
                entry.0 += dur;
                entry.1 += 1;
            }
        }
    }
    let mut stage_stats: Vec<Value> = stage_totals
        .iter()
        .map(|(idx, (total, count))| {
            let name = stage_names
                .get(*idx as usize)
                .cloned()
                .unwrap_or_else(|| format!("stage_{}", idx));
            json!({
                "name": name,
                "total_cycles": total,
                "instruction_count": count,
                "avg_latency": if *count > 0 { *total as f64 / *count as f64 } else { 0.0 }
            })
        })
        .collect();
    stage_stats.sort_by(|a, b| {
        b["avg_latency"]
            .as_f64()
            .unwrap_or(0.0)
            .partial_cmp(&a["avg_latency"].as_f64().unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(json!({
        "start_cycle": start_cycle,
        "end_cycle": end_cycle,
        "range_cycles": range,
        "instructions": {
            "total": total_instructions,
            "retired": retired,
            "flushed": flushed,
            "in_flight": in_flight,
            "flush_rate": if total_instructions > 0 { flushed as f64 / total_instructions as f64 } else { 0.0 }
        },
        "ipc": ipc,
        "counter_summaries": counter_summaries,
        "buffer_snapshots": buffer_snapshots,
        "stage_latencies": stage_stats
    }))
}

// ── Helpers ────────────────────────────────────────────────────────

fn field_type_name(ft: u8) -> &'static str {
    match FieldType::from_u8(ft) {
        Some(FieldType::U8) => "u8",
        Some(FieldType::U16) => "u16",
        Some(FieldType::U32) => "u32",
        Some(FieldType::U64) => "u64",
        Some(FieldType::I8) => "i8",
        Some(FieldType::I16) => "i16",
        Some(FieldType::I32) => "i32",
        Some(FieldType::I64) => "i64",
        Some(FieldType::Bool) => "bool",
        Some(FieldType::StringRef) => "string_ref",
        Some(FieldType::Enum) => "enum",
        None => "unknown",
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

// ── Request dispatch ───────────────────────────────────────────────

fn handle_request(trace: &mut CpuTrace, req: &JsonRpcRequest) -> Option<JsonRpcResponse> {
    match req.method.as_str() {
        "initialize" => Some(JsonRpcResponse::success(
            req.id.clone(),
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": {
                    "name": "uscope-mcp",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        )),

        // initialized is a notification (no id) — no response needed
        "notifications/initialized" => None,

        "tools/list" => Some(JsonRpcResponse::success(
            req.id.clone(),
            json!({ "tools": tool_definitions() }),
        )),

        "tools/call" => {
            let tool_name = req
                .params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let arguments = req.params.get("arguments").cloned().unwrap_or(json!({}));

            let result = match tool_name {
                "file_info" => Ok(tool_file_info(trace)),
                "state_at_cycle" => tool_state_at_cycle(trace, &arguments),
                "entity_timeline" => tool_entity_timeline(trace, &arguments),
                "counter_values" => tool_counter_values(trace, &arguments),
                "buffer_occupancy" => tool_buffer_occupancy(trace, &arguments),
                "analyze_performance" => tool_analyze_performance(trace, &arguments),
                _ => Err(format!("unknown tool: {}", tool_name)),
            };

            match result {
                Ok(value) => {
                    let text = serde_json::to_string_pretty(&value).unwrap_or_default();
                    Some(JsonRpcResponse::success(
                        req.id.clone(),
                        json!({
                            "content": [{
                                "type": "text",
                                "text": text
                            }]
                        }),
                    ))
                }
                Err(msg) => Some(JsonRpcResponse::success(
                    req.id.clone(),
                    json!({
                        "content": [{
                            "type": "text",
                            "text": format!("Error: {}", msg)
                        }],
                        "isError": true
                    }),
                )),
            }
        }

        _ => {
            if req.id.is_some() {
                Some(JsonRpcResponse::error(
                    req.id.clone(),
                    -32601,
                    format!("method not found: {}", req.method),
                ))
            } else {
                // Unknown notification — ignore silently
                None
            }
        }
    }
}

// ── Main loop ──────────────────────────────────────────────────────

fn main() {
    let cli = Cli::parse();

    eprintln!("uscope-mcp: opening trace '{}'...", cli.trace);
    let mut trace = CpuTrace::open(&cli.trace).unwrap_or_else(|e| {
        eprintln!("error: failed to open '{}': {}", cli.trace, e);
        std::process::exit(1);
    });
    eprintln!(
        "uscope-mcp: trace loaded ({} segments, {} max cycle)",
        trace.segment_count(),
        trace.max_cycle()
    );

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("uscope-mcp: stdin read error: {}", e);
                break;
            }
        };

        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        eprintln!("uscope-mcp: << {}", line);

        let req: JsonRpcRequest = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("uscope-mcp: parse error: {}", e);
                let resp = JsonRpcResponse::error(None, -32700, format!("parse error: {}", e));
                let out = serde_json::to_string(&resp).unwrap();
                eprintln!("uscope-mcp: >> {}", out);
                let _ = writeln!(stdout, "{}", out);
                let _ = stdout.flush();
                continue;
            }
        };

        if req.jsonrpc != "2.0" {
            let resp = JsonRpcResponse::error(
                req.id.clone(),
                -32600,
                "invalid JSON-RPC version".to_string(),
            );
            let out = serde_json::to_string(&resp).unwrap();
            eprintln!("uscope-mcp: >> {}", out);
            let _ = writeln!(stdout, "{}", out);
            let _ = stdout.flush();
            continue;
        }

        if let Some(resp) = handle_request(&mut trace, &req) {
            let out = serde_json::to_string(&resp).unwrap();
            eprintln!("uscope-mcp: >> {}", out);
            let _ = writeln!(stdout, "{}", out);
            let _ = stdout.flush();
        }
    }

    eprintln!("uscope-mcp: stdin closed, exiting");
}
