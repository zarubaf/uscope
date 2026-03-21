/// Segment reading utilities: chain walking, binary search in segment table.

use crate::types::*;
use std::io::{self, Read, Seek, SeekFrom};

/// Read a segment header at a given file offset.
pub fn read_segment_at<R: Read + Seek>(r: &mut R, offset: u64) -> io::Result<SegmentHeader> {
    r.seek(SeekFrom::Start(offset))?;
    SegmentHeader::read_from(r)
}

/// Walk the segment chain backwards from tail_offset, collecting all segment headers.
pub fn walk_chain<R: Read + Seek>(r: &mut R, tail_offset: u64) -> io::Result<Vec<(u64, SegmentHeader)>> {
    let mut segments = Vec::new();
    let mut offset = tail_offset;

    while offset != 0 {
        let header = read_segment_at(r, offset)?;
        let prev = header.prev_segment_offset;
        segments.push((offset, header));
        offset = prev;
    }

    segments.reverse();
    Ok(segments)
}

/// Binary search the segment table for the segment containing the given time.
pub fn find_segment_for_time(
    table: &[SegmentIndexEntry],
    time_ps: u64,
) -> Option<usize> {
    if table.is_empty() {
        return None;
    }

    // Binary search: find the last segment where time_start_ps <= time_ps
    let mut lo = 0;
    let mut hi = table.len();
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if table[mid].time_start_ps <= time_ps {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    if lo == 0 {
        None
    } else {
        Some(lo - 1)
    }
}

/// Read the segment table from a finalized file.
pub fn read_segment_table<R: Read + Seek>(
    r: &mut R,
    offset: u64,
    size: u64,
) -> io::Result<Vec<SegmentIndexEntry>> {
    r.seek(SeekFrom::Start(offset))?;
    let count = size as usize / SegmentIndexEntry::SIZE;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        entries.push(SegmentIndexEntry::read_from(r)?);
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_search_segments() {
        let table = vec![
            SegmentIndexEntry { offset: 0, time_start_ps: 0, time_end_ps: 1000 },
            SegmentIndexEntry { offset: 100, time_start_ps: 1000, time_end_ps: 2000 },
            SegmentIndexEntry { offset: 200, time_start_ps: 2000, time_end_ps: 3000 },
        ];

        assert_eq!(find_segment_for_time(&table, 0), Some(0));
        assert_eq!(find_segment_for_time(&table, 500), Some(0));
        assert_eq!(find_segment_for_time(&table, 1000), Some(1));
        assert_eq!(find_segment_for_time(&table, 1500), Some(1));
        assert_eq!(find_segment_for_time(&table, 2500), Some(2));
        assert_eq!(find_segment_for_time(&table, 5000), Some(2));
    }

    #[test]
    fn binary_search_empty() {
        assert_eq!(find_segment_for_time(&[], 100), None);
    }
}
