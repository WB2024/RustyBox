//! XDVDFS: the file system on Xbox and Xbox 360 game discs.
//!
//! A 360 disc image has the game partition at one of a few known offsets (depending on the
//! disc's layout generation). We find it by looking for the volume descriptor's magic, then
//! walk the root directory, which is a binary tree of entries.

use std::io;

use super::{ReadAt, le32};

pub const SECTOR: u64 = 2048;
const MAGIC: &[u8; 20] = b"MICROSOFT*XBOX*MEDIA";
/// Where the game partition starts: raw/rebuilt, XGD1, XGD2, XGD3.
const PARTITION_OFFSETS: [u64; 4] = [0, 0x1830_0000, 0x0FD9_0000, 0x0208_0000];
/// The volume descriptor sits 32 sectors into the partition.
const DESCRIPTOR_AT: u64 = 32 * SECTOR;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Entry {
    pub sector: u32,
    pub size: u32,
    pub is_dir: bool,
}

pub struct Volume<'a, R: ReadAt> {
    r: &'a R,
    base: u64,
    root_sector: u32,
    root_size: u32,
}

impl<'a, R: ReadAt> Volume<'a, R> {
    /// Find the game partition, or `None` if this isn't an Xbox disc image.
    pub fn open(r: &'a R) -> io::Result<Option<Volume<'a, R>>> {
        for base in PARTITION_OFFSETS {
            let mut head = [0u8; 28];
            match r.read_exact_at(&mut head, base + DESCRIPTOR_AT) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => continue,
                Err(e) => return Err(e),
            }
            if &head[..20] == MAGIC {
                let root_sector = le32(&head, 20).unwrap_or(0);
                let root_size = le32(&head, 24).unwrap_or(0);
                if root_size > 0 && root_size <= 16 * 1024 * 1024 {
                    return Ok(Some(Volume {
                        r,
                        base,
                        root_sector,
                        root_size,
                    }));
                }
            }
        }
        Ok(None)
    }

    pub fn partition_offset(&self) -> u64 {
        self.base
    }

    fn read_at_sector(&self, sector: u32, len: usize) -> io::Result<Vec<u8>> {
        let mut buf = vec![0u8; len];
        self.r
            .read_exact_at(&mut buf, self.base + sector as u64 * SECTOR)?;
        Ok(buf)
    }

    /// Look a name up in the root folder (case-insensitive).
    pub fn find_in_root(&self, name: &str) -> io::Result<Option<Entry>> {
        let dir = self.read_at_sector(self.root_sector, self.root_size as usize)?;
        Ok(find_entry(&dir, name))
    }

    /// Up to `max` bytes from the start of a file.
    pub fn read_file_prefix(&self, e: &Entry, max: usize) -> io::Result<Vec<u8>> {
        self.read_at_sector(e.sector, (e.size as usize).min(max))
    }
}

/// Search a directory table. Entries form a binary tree: each has a left and right child as a
/// dword offset into the table; `0xFFFF` in the first entry means an empty folder.
fn find_entry(dir: &[u8], name: &str) -> Option<Entry> {
    if dir.len() < 14 || dir[0] == 0xFF && dir[1] == 0xFF {
        return None;
    }
    let mut stack = vec![0usize];
    let mut seen = std::collections::HashSet::new();
    while let Some(off) = stack.pop() {
        if !seen.insert(off) || off + 14 > dir.len() {
            continue;
        }
        let left = u16::from_le_bytes([dir[off], dir[off + 1]]) as usize;
        let right = u16::from_le_bytes([dir[off + 2], dir[off + 3]]) as usize;
        let sector = le32(dir, off + 4)?;
        let size = le32(dir, off + 8)?;
        let attr = dir[off + 12];
        let nlen = dir[off + 13] as usize;
        let n = dir.get(off + 14..off + 14 + nlen)?;
        if n.eq_ignore_ascii_case(name.as_bytes()) {
            return Some(Entry {
                sector,
                size,
                is_dir: attr & 0x10 != 0,
            });
        }
        for child in [left, right] {
            if child != 0 && child != 0xFFFF {
                stack.push(child * 4);
            }
        }
    }
    None
}

/// A small, valid disc image whose partition starts at 0, with `xex` as `default.xex` (for mock data).
pub fn build_simple_disc(xex: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; 50 * SECTOR as usize + xex.len()];
    let mut root = Vec::new();
    root.extend(0u16.to_le_bytes());
    root.extend(0u16.to_le_bytes());
    root.extend(50u32.to_le_bytes());
    root.extend((xex.len() as u32).to_le_bytes());
    root.push(0);
    root.push(11);
    root.extend(b"default.xex");
    while root.len() % 4 != 0 {
        root.push(0xFF);
    }
    let desc_at = DESCRIPTOR_AT as usize;
    out[desc_at..desc_at + 20].copy_from_slice(MAGIC);
    out[desc_at + 20..desc_at + 24].copy_from_slice(&40u32.to_le_bytes());
    out[desc_at + 24..desc_at + 28].copy_from_slice(&(root.len() as u32).to_le_bytes());
    // A real descriptor repeats the magic at its end.
    out[desc_at + 0x7EC..desc_at + 0x7EC + 20].copy_from_slice(MAGIC);
    out[40 * SECTOR as usize..40 * SECTOR as usize + root.len()].copy_from_slice(&root);
    out[50 * SECTOR as usize..].copy_from_slice(xex);
    out
}

#[cfg(test)]
pub mod testimg {
    use super::*;

    /// A sparse in-memory disc image: reads outside the written pieces return zeros.
    #[derive(Default)]
    pub struct Img {
        pieces: Vec<(u64, Vec<u8>)>,
        pub len: u64,
    }

    impl Img {
        pub fn put(&mut self, at: u64, data: &[u8]) {
            self.len = self.len.max(at + data.len() as u64);
            self.pieces.push((at, data.to_vec()));
        }
    }

    impl Img {
        pub fn to_bytes(&self) -> Vec<u8> {
            let mut b = vec![0u8; self.len as usize];
            ReadAt::read_exact_at(self, &mut b, 0).unwrap();
            b
        }
    }

    impl ReadAt for Img {
        fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
            if offset + buf.len() as u64 > self.len {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "past end"));
            }
            buf.fill(0);
            for (at, data) in &self.pieces {
                let (s, e) = (
                    offset.max(*at),
                    (offset + buf.len() as u64).min(*at + data.len() as u64),
                );
                if s < e {
                    buf[(s - offset) as usize..(e - offset) as usize]
                        .copy_from_slice(&data[(s - at) as usize..(e - at) as usize]);
                }
            }
            Ok(())
        }
    }

    fn entry(left: u16, right: u16, sector: u32, size: u32, attr: u8, name: &str) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend(left.to_le_bytes());
        v.extend(right.to_le_bytes());
        v.extend(sector.to_le_bytes());
        v.extend(size.to_le_bytes());
        v.push(attr);
        v.push(name.len() as u8);
        v.extend(name.as_bytes());
        while v.len() % 4 != 0 {
            v.push(0xFF);
        }
        v
    }

    /// A tiny disc with `default.xex` (the given bytes) and a couple of other files in the root.
    pub fn disc(partition: u64, xex: &[u8]) -> Img {
        let mut img = Img::default();
        // Root directory at sector 40: Default.xex, whose right child is readme.txt.
        let first_len = entry(0, 0, 0, 0, 0, "Default.xex").len();
        let root = [
            entry(
                0,
                (first_len / 4) as u16,
                50,
                xex.len() as u32,
                0,
                "Default.xex",
            ),
            entry(0, 0, 99, 10, 0, "readme.txt"),
        ]
        .concat();
        let mut desc = Vec::new();
        desc.extend(MAGIC);
        desc.extend(40u32.to_le_bytes());
        desc.extend((root.len() as u32).to_le_bytes());
        img.put(partition + DESCRIPTOR_AT, &desc);
        img.put(partition + DESCRIPTOR_AT + 0x7EC, MAGIC);
        img.put(partition + 40 * SECTOR, &root);
        img.put(partition + 50 * SECTOR, xex);
        img
    }
}

#[cfg(test)]
mod tests {
    use super::{testimg::*, *};

    #[test]
    fn finds_the_partition_at_every_known_offset_and_reads_a_file() {
        for base in PARTITION_OFFSETS {
            let img = disc(base, b"XEX2-contents");
            let v = Volume::open(&img)
                .unwrap()
                .unwrap_or_else(|| panic!("no volume at {base:#x}"));
            assert_eq!(v.partition_offset(), base);
            let e = v.find_in_root("default.xex").unwrap().unwrap();
            assert_eq!(v.read_file_prefix(&e, 1000).unwrap(), b"XEX2-contents");
            assert!(v.find_in_root("readme.txt").unwrap().is_some());
            assert!(v.find_in_root("nope.xex").unwrap().is_none());
        }
    }

    #[test]
    fn the_simple_builder_makes_a_readable_disc() {
        let bytes = build_simple_disc(b"XEX2....");
        let slice = bytes.as_slice();
        let v = Volume::open(&slice).unwrap().unwrap();
        let e = v.find_in_root("DEFAULT.XEX").unwrap().unwrap();
        assert_eq!(v.read_file_prefix(&e, 100).unwrap(), b"XEX2....");
    }

    #[test]
    fn not_a_disc_is_none_and_garbage_directories_dont_loop() {
        let mut img = Img::default();
        img.put(0, &vec![1u8; 0x20000]);
        assert!(Volume::open(&img).unwrap().is_none());

        // An entry whose children point back at itself must terminate.
        let mut loopy = vec![0u8; 64];
        loopy[0..2].copy_from_slice(&0u16.to_le_bytes());
        loopy[2..4].copy_from_slice(&1u16.to_le_bytes());
        assert!(find_entry(&loopy, "x").is_none());
        assert!(find_entry(&[0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], "x").is_none());
    }
}
