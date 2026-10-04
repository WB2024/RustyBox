//! Reading Xbox 360 formats natively: disc images (XDVDFS), executables (XEX2), and the STFS
//! containers used by Games on Demand. Nothing here shells out to another program.

pub mod catalog;
pub mod meta;
pub mod stfs;
pub mod xex;
pub mod xiso;

/// Positioned reads, so the same parsers work on files and on in-memory test images.
pub trait ReadAt {
    /// Fill `buf` from `offset`. Reading past the end is an error.
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<()>;
}

impl ReadAt for &[u8] {
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
        let start = usize::try_from(offset).map_err(|_| std::io::ErrorKind::UnexpectedEof)?;
        let end = start
            .checked_add(buf.len())
            .ok_or(std::io::ErrorKind::UnexpectedEof)?;
        buf.copy_from_slice(
            self.get(start..end)
                .ok_or(std::io::ErrorKind::UnexpectedEof)?,
        );
        Ok(())
    }
}

impl ReadAt for std::fs::File {
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
        std::os::unix::fs::FileExt::read_exact_at(self, buf, offset)
    }
}

pub fn be32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

pub fn le32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

pub fn hex8(v: u32) -> String {
    format!("{v:08X}")
}

/// A UTF-16 big-endian, NUL-terminated string.
pub fn utf16be(b: &[u8]) -> String {
    let units: Vec<u16> = b
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .take_while(|u| *u != 0)
        .collect();
    String::from_utf16_lossy(&units).trim().to_string()
}
