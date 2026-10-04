//! XEX2: the Xbox 360 executable. Only the header matters here, for the "execution info"
//! block that names the title and the disc it came from.

use super::{be32, hex8};

const EXECUTION_INFO: u32 = 0x0004_0006;

#[derive(Debug, Clone, PartialEq)]
pub struct ExecInfo {
    pub media_id: String,
    pub title_id: String,
    pub version: u32,
    pub base_version: u32,
    pub platform: u8,
    pub executable_type: u8,
    pub disc_number: u8,
    pub disc_count: u8,
}

/// Parse the start of an XEX2 file (the first 64 KB is plenty).
pub fn parse(b: &[u8]) -> Result<ExecInfo, String> {
    if b.get(..4) != Some(b"XEX2") {
        return Err("Not an XEX2 executable".into());
    }
    let count = be32(b, 0x14).ok_or("Truncated XEX header")? as usize;
    if count > 128 {
        return Err("Corrupt XEX header".into());
    }
    for i in 0..count {
        let at = 0x18 + i * 8;
        let (Some(id), Some(val)) = (be32(b, at), be32(b, at + 4)) else {
            break;
        };
        if id != EXECUTION_INFO {
            continue;
        }
        let o = val as usize;
        let info = b.get(o..o + 20).ok_or("Truncated XEX execution info")?;
        return Ok(ExecInfo {
            media_id: hex8(be32(info, 0).unwrap_or(0)),
            version: be32(info, 4).unwrap_or(0),
            base_version: be32(info, 8).unwrap_or(0),
            title_id: hex8(be32(info, 12).unwrap_or(0)),
            platform: info[16],
            executable_type: info[17],
            disc_number: info[18],
            disc_count: info[19],
        });
    }
    Err("The executable has no title information".into())
}

/// A minimal XEX header carrying execution info (for mock data and tests).
pub fn build(media_id: u32, title_id: u32, disc: u8, discs: u8) -> Vec<u8> {
    let mut b = vec![0u8; 0x200];
    b[..4].copy_from_slice(b"XEX2");
    b[0x14..0x18].copy_from_slice(&2u32.to_be_bytes());
    // an unrelated header first, then execution info at 0x100
    b[0x18..0x1C].copy_from_slice(&0x0001_0100u32.to_be_bytes());
    b[0x1C..0x20].copy_from_slice(&0x8280_50E0u32.to_be_bytes());
    b[0x20..0x24].copy_from_slice(&EXECUTION_INFO.to_be_bytes());
    b[0x24..0x28].copy_from_slice(&0x100u32.to_be_bytes());
    b[0x100..0x104].copy_from_slice(&media_id.to_be_bytes());
    b[0x104..0x108].copy_from_slice(&1u32.to_be_bytes());
    b[0x10C..0x110].copy_from_slice(&title_id.to_be_bytes());
    b[0x110] = 2;
    b[0x112] = disc;
    b[0x113] = discs;
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_execution_info() {
        let i = parse(&build(0x08FE_F3C1, 0x4D53_0805, 1, 2)).unwrap();
        assert_eq!(
            (
                i.title_id.as_str(),
                i.media_id.as_str(),
                i.disc_number,
                i.disc_count
            ),
            ("4D530805", "08FEF3C1", 1, 2)
        );
    }

    #[test]
    fn rejects_other_files_and_truncation() {
        assert!(parse(b"MZ not an xex").is_err());
        assert!(parse(&build(1, 2, 1, 1)[..0x108]).is_err());
        let mut many = build(1, 2, 1, 1);
        many[0x14..0x18].copy_from_slice(&9999u32.to_be_bytes());
        assert!(parse(&many).is_err());
    }

    /// Check real files: `RUSTYBOX_TEST_XEX=/path/to/file-or-folder cargo test real_xex -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_xex() {
        fn walk(p: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            if p.is_dir() {
                for e in std::fs::read_dir(p).unwrap().flatten() {
                    walk(&e.path(), out);
                }
            } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("xex")) {
                out.push(p.to_path_buf());
            }
        }
        let mut files = Vec::new();
        walk(
            std::path::Path::new(
                &std::env::var("RUSTYBOX_TEST_XEX").expect("set RUSTYBOX_TEST_XEX"),
            ),
            &mut files,
        );
        let (mut ok, mut matches_folder, mut none) = (0, 0, 0);
        for f in &files {
            let bytes = std::fs::read(f).unwrap();
            match parse(&bytes[..bytes.len().min(65536)]) {
                Ok(i) => {
                    ok += 1;
                    let folder = f
                        .parent()
                        .and_then(|p| p.file_name())
                        .map(|n| n.to_string_lossy().to_uppercase())
                        .unwrap_or_default();
                    if folder == i.title_id {
                        matches_folder += 1;
                    } else {
                        eprintln!("{}: title {} but folder {folder}", f.display(), i.title_id);
                    }
                }
                Err(e) => {
                    none += 1;
                    eprintln!("{}: {e}", f.display());
                }
            }
        }
        eprintln!(
            "{n} files: {ok} parsed, {matches_folder} match their folder's title ID, {none} without title info",
            n = files.len()
        );
    }
}
