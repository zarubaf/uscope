#[cfg(feature = "decode")]
use instruction_decoder::Decoder;

/// Build an RV64GC instruction decoder from bundled ISA TOML definitions.
#[cfg(feature = "decode")]
pub fn build_rv64gc_decoder() -> Option<Decoder> {
    Decoder::new(&[
        include_str!("../isa/RV64I.toml").to_string(),
        include_str!("../isa/RV64M.toml").to_string(),
        include_str!("../isa/RV64A.toml").to_string(),
        include_str!("../isa/RV32F.toml").to_string(),
        include_str!("../isa/RV64D.toml").to_string(),
        include_str!("../isa/RV64C.toml").to_string(),
        include_str!("../isa/RV64C-lower.toml").to_string(),
        include_str!("../isa/RV32_Zicsr.toml").to_string(),
        include_str!("../isa/RV_Zifencei.toml").to_string(),
    ])
    .ok()
}

/// Decode a single instruction using the decoder. Returns mnemonic or hex fallback.
#[cfg(feature = "decode")]
pub fn decode_instruction(decoder: &Decoder, inst_bits: u32) -> String {
    // Compressed instructions have the two LSBs != 0b11
    let bit_width = if inst_bits & 0x3 != 0x3 { 16 } else { 32 };
    decoder
        .decode_from_u32(inst_bits, bit_width)
        .unwrap_or_else(|_| format!("0x{:08x}", inst_bits))
}

/// No-op decoder builder when the decode feature is disabled.
#[cfg(not(feature = "decode"))]
pub fn build_rv64gc_decoder() -> Option<()> {
    None
}
