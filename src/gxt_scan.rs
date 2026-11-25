//! Bounded streaming discovery of typed RAGE9 localization tables.

use crate::model3d::is_zlib_head;
use crate::tex::{gxt_referenced_rows, gxt_row_descriptor, vptr};
use flate2::{Decompress, FlushDecompress, Status};
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;

const BLOCK_BYTES: usize = 8 * 1024 * 1024;
const MAX_STORED_BYTES: usize = 2 * 1024 * 1024;
const MAX_INFLATED_BYTES: usize = 16 * 1024 * 1024;
const ZLIB_PAGE_BYTES: usize = 64 * 1024;

pub(crate) struct Table {
    pub offset: u64,
    pub chunks: Vec<(u64, u64)>,
    pub raw_size: u64,
    pub rows: usize,
    pub nonblank_rows: usize,
}

fn refill(input: &mut impl Read, mut target: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while !target.is_empty() {
        match input.read(target) {
            Ok(0) => break,
            Ok(n) => {
                filled += n;
                target = &mut target[n..];
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

struct Probe {
    decoder: Decompress,
    raw: Vec<u8>,
    block: Vec<u8>,
}

impl Probe {
    fn new() -> Self {
        Self {
            decoder: Decompress::new(true),
            raw: Vec::new(),
            block: vec![0; 64 * 1024],
        }
    }

    fn append_stream(&mut self, input: &[u8]) -> Option<usize> {
        self.decoder.reset(true);
        loop {
            let consumed = self.decoder.total_in() as usize;
            let written = self.decoder.total_out();
            let status = self.decoder.decompress(
                input.get(consumed..)?,
                &mut self.block,
                FlushDecompress::None,
            ).ok()?;
            let produced = (self.decoder.total_out() - written) as usize;
            if self.raw.len() + produced > MAX_INFLATED_BYTES {
                return None;
            }
            self.raw.extend_from_slice(&self.block[..produced]);
            if status == Status::StreamEnd {
                return Some(self.decoder.total_in() as usize);
            }
            if self.decoder.total_in() as usize == consumed && produced == 0 {
                return None;
            }
        }
    }

    fn table(&mut self, input: &[u8], offset: u64) -> Option<Table> {
        self.decoder.reset(true);
        let mut header = [0; 0x60];
        self.decoder.decompress(&input[..input.len().min(4096)], &mut header, FlushDecompress::None).ok()?;
        if self.decoder.total_out() < header.len() as u64 || &header[0x10..0x15] != b"0DRPz" {
            return None;
        }
        let descriptor = vptr(u64::from_le_bytes(header[0x38..0x40].try_into().ok()?))?;
        if descriptor + 16 > MAX_INFLATED_BYTES {
            return None;
        }
        self.raw.clear();
        let mut chunks = Vec::new();
        let mut start = 0;
        loop {
            let before = self.raw.len();
            let length = self.append_stream(input.get(start..)?)?;
            if before != 0 && self.raw.get(before + 0x10..before + 0x15) == Some(b"0DRPz") {
                return None;
            }
            chunks.push((offset + start as u64, length as u64));
            if self.raw.len() >= descriptor + 16 {
                let typed = gxt_row_descriptor(&self.raw)?;
                if let Some(rows) = gxt_referenced_rows(&self.raw, typed) {
                    let row_count = rows.len();
                    let nonblank_rows = rows.iter().filter(|row| !row.trim().is_empty()).count();
                    // A page can already contain every referenced string while an
                    // immediately adjacent continuation holds the end of the resource.
                    let mut before_page = before;
                    let mut end = start + length;
                    while (self.raw.len() - before_page).is_multiple_of(ZLIB_PAGE_BYTES) {
                        let next = (end + 15) & !15;
                        let Some(potential) = input.get(next..).filter(|bytes| is_zlib_head(bytes)) else {
                            break;
                        };
                        before_page = self.raw.len();
                        let Some(length) = self.append_stream(potential) else {
                            self.raw.truncate(before_page);
                            break;
                        };
                        if self.raw.get(before_page + 0x10..before_page + 0x15) == Some(b"0DRPz") {
                            self.raw.truncate(before_page);
                            break;
                        }
                        chunks.push((offset + next as u64, length as u64));
                        end = next + length;
                    }
                    return Some(Table {
                        offset,
                        chunks,
                        raw_size: self.raw.len() as u64,
                        rows: row_count,
                        nonblank_rows,
                    });
                }
            }
            // Continuations are separate zlib streams at nearby aligned offsets.
            let end = start + length;
            start = (((end + 15) & !15)..=end + 64)
                .step_by(16)
                .find(|&at| input.get(at..).is_some_and(is_zlib_head))?;
        }
    }
}

fn scan(
    mut input: impl Read,
    mut progress: impl FnMut(u64),
    mut visit: impl FnMut(Table) -> io::Result<()>,
) -> io::Result<u64> {
    let mut probe = Probe::new();
    let mut window = vec![0; BLOCK_BYTES + MAX_STORED_BYTES];
    let mut filled = refill(&mut input, &mut window)?;
    if window.get(..filled.min(4)) != Some(b"9FPR") {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Expected an RPF9 archive"));
    }
    let mut block_offset = 0u64;
    let mut next = 0u64;
    while filled != 0 {
        let limit = filled.min(BLOCK_BYTES);
        let first = (next.saturating_sub(block_offset) as usize + 15) & !15;
        let mut cursor = first;
        while cursor < limit {
            let input = &window[cursor..filled.min(cursor + MAX_STORED_BYTES)];
            if is_zlib_head(input) {
                if let Some(table) = probe.table(input, block_offset + cursor as u64) {
                    let (at, length) = table.chunks.last().copied().unwrap();
                    next = (at + length + 15) & !15;
                    cursor = (next - block_offset) as usize;
                    visit(table)?;
                    continue;
                }
            }
            cursor += 16;
        }
        block_offset += limit as u64;
        progress(block_offset);
        window.copy_within(limit..filled, 0);
        filled -= limit;
        filled += refill(&mut input, &mut window[filled..])?;
    }
    Ok(block_offset)
}

pub(crate) fn scan_archive(
    path: &Path,
    progress: impl FnMut(u64),
    visit: impl FnMut(Table) -> io::Result<()>,
) -> io::Result<u64> {
    let file = File::open(path)?;
    let expected = file.metadata()?.len();
    let scanned = scan(file, progress, visit)?;
    if scanned != expected {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "RPF9 size changed during GXT scan"));
    }
    Ok(scanned)
}

pub(crate) fn report(path: &Path) -> io::Result<()> {
    let mut output = io::BufWriter::new(io::stdout().lock());
    writeln!(output, "offset\tchunks\traw_bytes\trows\tnonblank_rows")?;
    let mut tables = 0usize;
    let mut rows = 0u64;
    let mut nonblank_rows = 0u64;
    let mut last = 0u64;
    let bytes = scan_archive(path, |bytes| {
        if bytes - last >= 1024 * 1024 * 1024 {
            eprintln!("GXT scan: {bytes} bytes");
            last = bytes;
        }
    }, |table| {
        tables += 1;
        rows += table.rows as u64;
        nonblank_rows += table.nonblank_rows as u64;
        writeln!(output, "{}\t{}\t{}\t{}\t{}", table.offset, table.chunks.len(), table.raw_size, table.rows, table.nonblank_rows)
    })?;
    output.flush()?;
    eprintln!("GXT done: bytes={bytes} tables={tables} rows={rows} nonblank_rows={nonblank_rows}");
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use flate2::{write::ZlibEncoder, Compression};

    fn compressed(bytes: &[u8]) -> Vec<u8> {
        let mut writer = ZlibEncoder::new(Vec::new(), Compression::default());
        writer.write_all(bytes).unwrap();
        writer.finish().unwrap()
    }

    pub(crate) fn resource() -> Vec<u8> {
        let mut raw = vec![0; 0xc3];
        raw[0x10..0x15].copy_from_slice(b"0DRPz");
        raw[0x38..0x40].copy_from_slice(&0x5000_0060u64.to_le_bytes());
        raw[0x60..0x64].copy_from_slice(&0xa847_8889u32.to_le_bytes());
        raw[0x64..0x68].copy_from_slice(&32u32.to_le_bytes());
        raw[0x68..0x70].copy_from_slice(&0x5000_00a0u64.to_le_bytes());
        raw[0x90..0x94].copy_from_slice(&0x10u32.to_le_bytes());
        raw[0x94..0x98].copy_from_slice(&3u32.to_le_bytes());
        raw[0x98..0xa0].copy_from_slice(&0x5000_00c0u64.to_le_bytes());
        raw[0xa8..0xb0].copy_from_slice(&4u64.to_le_bytes());
        raw[0xb8..0xc0].copy_from_slice(&((2u64 << 20) | 4).to_le_bytes());
        raw[0xc0..].copy_from_slice(b"!\0\0");
        raw
    }

    #[test]
    fn finds_split_gxt_once_across_read_boundary() {
        let mut archive = vec![0; BLOCK_BYTES - 16];
        archive[..4].copy_from_slice(b"9FPR");
        let raw = resource();
        let offset = archive.len() as u64;
        let first = compressed(&raw[..0x70]);
        archive.extend_from_slice(&first);
        archive.resize((archive.len() + 15) & !15, 0);
        let continuation = archive.len() as u64;
        let second = compressed(&raw[0x70..]);
        archive.extend_from_slice(&second);
        let mut tables = Vec::new();
        let bytes = scan(archive.as_slice(), |_| {}, |t| {
            tables.push(t);
            Ok(())
        }).unwrap();
        assert_eq!(bytes, archive.len() as u64);
        let [table] = tables.as_slice() else { panic!("expected one table") };
        assert_eq!(table.offset, offset);
        assert_eq!(table.chunks, [(offset, first.len() as u64), (continuation, second.len() as u64)]);
        assert_eq!(table.raw_size, raw.len() as u64);
        assert_eq!((table.rows, table.nonblank_rows), (2, 1));
    }

    #[test]
    fn retains_the_resource_tail_after_the_last_text_reference() {
        let mut archive = vec![0; 16];
        archive[..4].copy_from_slice(b"9FPR");
        let mut raw = resource();
        raw.resize(ZLIB_PAGE_BYTES, 0);
        archive.extend(compressed(&raw));
        archive.resize((archive.len() + 15) & !15, 0);
        archive.extend(compressed(b"native resource tail"));
        scan(archive.as_slice(), |_| {}, |table| {
            assert_eq!(table.raw_size, (ZLIB_PAGE_BYTES + 20) as u64);
            assert_eq!(table.chunks.len(), 2);
            assert_eq!(table.rows, 2);
            Ok(())
        }).unwrap();
    }

    #[test]
    fn skips_invalid_checksum_and_other_prd0_layouts() {
        let mut archive = vec![0; 16];
        archive[..4].copy_from_slice(b"9FPR");
        let mut invalid = compressed(&resource());
        *invalid.last_mut().unwrap() ^= 1;
        archive.extend(invalid);
        archive.resize((archive.len() + 15) & !15, 0);
        let mut unrelated = resource();
        unrelated[0x60..0x64].copy_from_slice(&0xc90d_1c34u32.to_le_bytes());
        archive.extend(compressed(&unrelated));
        archive.resize((archive.len() + 15) & !15, 0);
        let valid_offset = archive.len() as u64;
        archive.extend(compressed(&resource()));
        let mut offsets = Vec::new();
        scan(archive.as_slice(), |_| {}, |table| {
            offsets.push(table.offset);
            Ok(())
        }).unwrap();
        assert_eq!(offsets, [valid_offset]);
    }
}
