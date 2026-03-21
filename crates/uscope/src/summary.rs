/// Summary (mipmap pyramid) section read/write.
/// Summary data is written at finalization for fast multi-resolution rendering.
use crate::types::*;
use std::io::{self, Read, Write};

/// Read summary header and level descriptors.
pub fn read_summary<R: Read>(r: &mut R) -> io::Result<(SummaryHeader, Vec<LevelDesc>)> {
    let header = SummaryHeader::read_from(r)?;
    let mut levels = Vec::with_capacity(header.num_levels as usize);
    for _ in 0..header.num_levels {
        levels.push(LevelDesc::read_from(r)?);
    }
    Ok((header, levels))
}

/// Write summary header and level descriptors.
pub fn write_summary<W: Write>(
    w: &mut W,
    header: &SummaryHeader,
    levels: &[LevelDesc],
) -> io::Result<()> {
    header.write_to(w)?;
    for l in levels {
        l.write_to(w)?;
    }
    Ok(())
}
