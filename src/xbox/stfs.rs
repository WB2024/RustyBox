//! STFS packages (`CON `, `LIVE`, `PIRS`): the container for Games on Demand, title updates,
//! saves and downloads. The header says what is inside.

use super::{be32, hex8, utf16be};

/// How much of the start of the file to read.
pub const HEADER_LEN: usize = 0x1800;

#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    pub magic: String,
    pub content_type: u32,
    pub media_id: String,
    pub version: u32,
    pub base_version: u32,
    pub title_id: String,
    pub disc_number: u8,
    pub disc_count: u8,
    /// The package's own name (often the game's name for a GOD container).
    pub display_name: String,
    /// The game's name from the title-name field.
    pub title_name: String,
}

pub fn parse(b: &[u8]) -> Result<Header, String> {
    let magic = b.get(..4).ok_or("File too small to be a package")?;
    if ![b"CON ".as_slice(), b"LIVE", b"PIRS"].contains(&magic) {
        return Err("Not an STFS package".into());
    }
    if b.len() < 0x368 {
        return Err("Truncated package header".into());
    }
    Ok(Header {
        magic: String::from_utf8_lossy(magic).trim().to_string(),
        content_type: be32(b, 0x344).unwrap_or(0),
        media_id: hex8(be32(b, 0x354).unwrap_or(0)),
        version: be32(b, 0x358).unwrap_or(0),
        base_version: be32(b, 0x35C).unwrap_or(0),
        title_id: hex8(be32(b, 0x360).unwrap_or(0)),
        disc_number: b[0x366],
        disc_count: b[0x367],
        display_name: b.get(0x411..0x411 + 0x80).map(utf16be).unwrap_or_default(),
        title_name: b
            .get(0x1691..0x1691 + 0x80)
            .map(utf16be)
            .unwrap_or_default(),
    })
}

/// A minimal STFS header (for mock data and tests).
pub fn build(
    magic: &[u8; 4],
    content_type: u32,
    title_id: u32,
    media_id: u32,
    display: &str,
    title: &str,
) -> Vec<u8> {
    let mut b = vec![0u8; HEADER_LEN];
    b[..4].copy_from_slice(magic);
    b[0x344..0x348].copy_from_slice(&content_type.to_be_bytes());
    b[0x354..0x358].copy_from_slice(&media_id.to_be_bytes());
    b[0x360..0x364].copy_from_slice(&title_id.to_be_bytes());
    b[0x366] = 1;
    b[0x367] = 1;
    let put = |b: &mut Vec<u8>, at: usize, s: &str| {
        for (i, u) in s.encode_utf16().enumerate() {
            b[at + i * 2..at + i * 2 + 2].copy_from_slice(&u.to_be_bytes());
        }
    };
    put(&mut b, 0x411, display);
    put(&mut b, 0x1691, title);
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_god_header() {
        let h = parse(&build(
            b"LIVE",
            0x7000,
            0x4D53_0805,
            0x1122_3344,
            "Alan Wake",
            "Alan Wake",
        ))
        .unwrap();
        assert_eq!(
            (
                h.title_id.as_str(),
                h.media_id.as_str(),
                h.content_type,
                h.magic.as_str()
            ),
            ("4D530805", "11223344", 0x7000, "LIVE")
        );
        assert_eq!(
            (h.display_name.as_str(), h.title_name.as_str()),
            ("Alan Wake", "Alan Wake")
        );
    }

    #[test]
    fn rejects_other_files() {
        assert!(parse(b"XEX2 and more bytes here").is_err());
        assert!(parse(b"CON ").is_err());
        assert!(parse(&[]).is_err());
    }

    /// Check real packages: `RUSTYBOX_TEST_STFS=/folder cargo test real_stfs -- --ignored --nocapture`.
    /// Each package's title ID is compared with the 8-hex-digit folder it sits in (any parent).
    #[test]
    #[ignore]
    fn real_stfs() {
        fn walk(p: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            if p.is_dir() {
                for e in std::fs::read_dir(p).unwrap().flatten() {
                    walk(&e.path(), out);
                }
            } else {
                out.push(p.to_path_buf());
            }
        }
        let mut files = Vec::new();
        walk(
            std::path::Path::new(
                &std::env::var("RUSTYBOX_TEST_STFS").expect("set RUSTYBOX_TEST_STFS"),
            ),
            &mut files,
        );
        let (mut parsed, mut matched, mut checked) = (0, 0, 0);
        for f in files {
            use std::io::Read;
            let mut buf = Vec::new();
            std::fs::File::open(&f)
                .unwrap()
                .take(HEADER_LEN as u64)
                .read_to_end(&mut buf)
                .unwrap();
            let Ok(h) = parse(&buf) else { continue };
            parsed += 1;
            eprintln!(
                "{:<5} type {:08X} title {} media {} v{} \"{}\" / \"{}\"  {}",
                h.magic,
                h.content_type,
                h.title_id,
                h.media_id,
                h.version,
                h.display_name,
                h.title_name,
                f.file_name().unwrap().to_string_lossy()
            );
            let hex_folders: Vec<String> = f
                .ancestors()
                .skip(1)
                .filter_map(|a| a.file_name())
                .map(|n| n.to_string_lossy().to_uppercase())
                .filter(|n| n.len() == 8 && n.bytes().all(|b| b.is_ascii_hexdigit()))
                .collect();
            if !hex_folders.is_empty() {
                checked += 1;
                if hex_folders.contains(&h.title_id) {
                    matched += 1;
                } else {
                    eprintln!("   MISMATCH: {} not in {hex_folders:?}", h.title_id);
                }
            }
        }
        eprintln!("{parsed} packages parsed, {matched}/{checked} title IDs match their folder");
    }
}
