//! What a game file says about itself: title ID, media ID, disc number and a name.

use std::{fs::File, path::Path};

use super::{ReadAt, catalog, stfs, xex, xiso};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Meta {
    pub title_id: String,
    pub media_id: Option<String>,
    pub disc: Option<u8>,
    pub discs: Option<u8>,
    /// "xbox360" or "xbox" (original Xbox, recognised but not read).
    pub platform: String,
    /// The best name we have: from the title list, else from the file itself.
    pub game_name: Option<String>,
    /// The STFS content type (`0x7000` Games on Demand, `0xD0000` Xbox Live Arcade, `0x2` add-on content, …).
    pub content_type: Option<u32>,
    /// What is wrong with the files, in plain words, if anything (for example an incomplete copy).
    pub health: Option<String>,
}

/// What a content type means for the games list.
pub fn content_kind(content_type: Option<u32>) -> &'static str {
    match content_type {
        None | Some(0x0000_7000) | Some(0x0000_5000) | Some(0x000D_0000) | Some(0x0000_4000) => {
            "game"
        }
        Some(0x0000_0002) => "dlc",
        Some(0x000B_0000) => "update",
        Some(_) => "other",
    }
}

fn nonzero(v: u8) -> Option<u8> {
    (v > 0).then_some(v)
}

fn finish(
    title_id: String,
    media_id: Option<String>,
    disc: Option<u8>,
    discs: Option<u8>,
    own_name: Option<String>,
) -> Meta {
    let media = media_id.filter(|m| m != "00000000");
    let name = catalog::get()
        .name(&title_id, media.as_deref())
        .map(str::to_string)
        .or(own_name.filter(|n| !n.is_empty()));
    Meta {
        title_id,
        media_id: media,
        disc,
        discs,
        platform: "xbox360".into(),
        game_name: name,
        content_type: None,
        health: None,
    }
}

/// Read an ISO image: find the game partition, then the title info in `default.xex`.
pub fn read_iso<R: ReadAt>(r: &R) -> Result<Meta, String> {
    let vol = xiso::Volume::open(r)
        .map_err(|e| format!("Could not read the image: {e}"))?
        .ok_or("Not an Xbox disc image (no Xbox file system found)")?;
    let Some(entry) = vol
        .find_in_root("default.xex")
        .map_err(|e| format!("Could not read the disc's files: {e}"))?
    else {
        if vol.find_in_root("default.xbe").ok().flatten().is_some() {
            return Err("Original Xbox disc image (not an Xbox 360 game)".into());
        }
        return Err("The disc has no default.xex".into());
    };
    let bytes = vol
        .read_file_prefix(&entry, 64 * 1024)
        .map_err(|e| format!("Could not read default.xex: {e}"))?;
    let info = xex::parse(&bytes)?;
    Ok(finish(
        info.title_id,
        Some(info.media_id),
        nonzero(info.disc_number),
        nonzero(info.disc_count),
        None,
    ))
}

pub fn read_iso_file(path: &Path) -> Result<Meta, String> {
    read_iso(&File::open(path).map_err(|e| format!("Could not open it: {e}"))?)
}

/// How much a content-type folder name is preferred as the game's container (lower is better).
pub fn god_rank(ct: &str) -> u8 {
    match u32::from_str_radix(ct, 16).ok() {
        Some(0x7000) => 0,
        Some(0x5000) => 1,
        Some(0xD0000) => 2,
        Some(0x4000) => 3,
        _ => 9,
    }
}

/// The container file of a GOD title folder (`<TitleID>/<content type>/<container>`). The playable
/// game content types are preferred when several exist (a title can also hold add-on content).
pub fn find_god_container(title_dir: &Path) -> Result<std::path::PathBuf, String> {
    let rank = god_rank;
    let mut candidates: Vec<(u8, std::path::PathBuf)> = Vec::new();
    for ct in std::fs::read_dir(title_dir)
        .map_err(|e| e.to_string())?
        .flatten()
    {
        let name = ct.file_name().to_string_lossy().to_string();
        if !ct.file_type().is_ok_and(|t| t.is_dir()) || name.len() != 8 {
            continue;
        }
        for f in std::fs::read_dir(ct.path())
            .map_err(|e| e.to_string())?
            .flatten()
        {
            let fname = f.file_name().to_string_lossy().to_string();
            if f.file_type().is_ok_and(|t| t.is_file()) && !fname.contains('.') {
                candidates.push((rank(&name), f.path()));
            }
        }
    }
    candidates.sort();
    candidates
        .into_iter()
        .next()
        .map(|(_, p)| p)
        .ok_or_else(|| "No container file found (expected <content type>/<container>)".to_string())
}

/// Is this a content type whose game lives in a `.data` folder next to the container (a Games on
/// Demand package)? Arcade games and add-ons are a single file.
pub fn god_has_data_folder(content_type: Option<u32>) -> bool {
    matches!(content_type, Some(0x7000) | Some(0x5000) | None)
}

/// Problems with a GOD package, from what was found: the container's size, its content type, the
/// sizes of `Data0000`, `Data0001`... (None when they weren't looked at) and the number of data
/// files its header says there are. Empty means it looks complete.
pub fn god_problem_list(
    size: u64,
    content_type: Option<u32>,
    data_sizes: Option<&[u64]>,
    parts: Option<usize>,
) -> Vec<String> {
    let mut out = Vec::new();
    if size == 0 {
        out.push(
            "The container file is empty (0 bytes): this game was copied incompletely".to_string(),
        );
    } else if size < 0x3A8 {
        out.push(format!(
            "The container file is only {size} bytes: this game was copied incompletely"
        ));
    }
    // Only Games on Demand packages keep their game in a `.data` folder; Xbox Live Arcade games and
    // add-on content are a single file, so there is nothing more to check for them.
    if !god_has_data_folder(content_type) {
        return out;
    }
    let sizes = data_sizes.unwrap_or(&[]);
    let empty = sizes.iter().filter(|s| **s == 0).count();
    if sizes.is_empty() {
        out.push("There are no data files".to_string());
    } else if empty > 0 {
        out.push(format!(
            "{empty} of {} data files are empty: this game was copied incompletely",
            sizes.len()
        ));
    }
    // What the header says there should be.
    if let Some(parts) = parts
        && parts > 0
        && parts < 10_000
        && sizes.len() < parts
    {
        out.push(format!(
            "{} of {parts} data files are missing",
            parts - sizes.len()
        ));
    }
    out
}

/// The number of data files a container's header names (at 0x3A0), if the header is there.
pub fn god_parts<R: ReadAt>(r: &R, size: u64) -> Option<usize> {
    if size < 0x3A8 {
        return None;
    }
    let mut h = [0u8; 8];
    r.read_exact_at(&mut h, 0x3A0).ok()?;
    Some(u32::from_le_bytes([h[0], h[1], h[2], h[3]]) as usize)
}

/// Problems with a GOD package's files: an empty or short container, missing data files, empty
/// data files, or data that doesn't add up to what the container says. Empty means it looks complete.
pub fn god_problems(container: &Path) -> Vec<String> {
    let size = std::fs::metadata(container).map(|m| m.len()).unwrap_or(0);
    let content_type = container
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| u32::from_str_radix(&n.to_string_lossy(), 16).ok());
    if !god_has_data_folder(content_type) {
        return god_problem_list(size, content_type, None, None);
    }
    let mut data = container.as_os_str().to_owned();
    data.push(".data");
    let data = std::path::PathBuf::from(data);
    let mut sizes: Vec<u64> = Vec::new();
    while let Ok(m) = std::fs::metadata(data.join(format!("Data{:04}", sizes.len()))) {
        sizes.push(m.len());
    }
    let parts = File::open(container).ok().and_then(|f| god_parts(&f, size));
    god_problem_list(size, content_type, Some(&sizes), parts)
}

/// Read a GOD title folder: the container is an STFS package whose header names the game.
pub fn read_god_dir(title_dir: &Path) -> Result<Meta, String> {
    let container = &find_god_container(title_dir)?;
    let problems = god_problems(container);
    let size = std::fs::metadata(container).map(|m| m.len()).unwrap_or(0);
    let file = File::open(container).map_err(|e| format!("Could not open the container: {e}"))?;
    god_meta(&file, size, problems)
}

/// The game a GOD container's header describes. `problems` are noted on it (or, if the container
/// is too small to read, are the error).
pub fn god_meta<R: ReadAt>(file: &R, size: u64, problems: Vec<String>) -> Result<Meta, String> {
    if size < 0x1800 {
        return Err(problems
            .into_iter()
            .next()
            .unwrap_or_else(|| "The container file is too small to be a game".into()));
    }
    let mut buf = vec![0u8; stfs::HEADER_LEN];
    let mut got = 0;
    // The container may be shorter than the full header; read what is there.
    while got < buf.len() {
        let want = (buf.len() - got).min(4096);
        match file.read_exact_at(&mut buf[got..got + want], got as u64) {
            Ok(()) => got += want,
            Err(_) => break,
        }
    }
    let h = stfs::parse(&buf[..got])?;
    let own = [h.title_name.clone(), h.display_name.clone()]
        .into_iter()
        .find(|n| !n.is_empty());
    let mut m = finish(
        h.title_id,
        Some(h.media_id),
        nonzero(h.disc_number),
        nonzero(h.disc_count),
        own,
    );
    m.content_type = Some(h.content_type);
    // An original Xbox game on demand has this content type. But some Xbox 360 Arcade games are
    // packaged the same way, so a title that is in the Xbox 360 list stays an Xbox 360 game.
    if h.content_type == 0x0000_5000 && catalog::get().name(&m.title_id, None).is_none() {
        m.platform = "xbox".into();
    }
    if !problems.is_empty() {
        m.health = Some(problems.join("; "));
    }
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xbox::xiso::testimg::disc;

    #[test]
    fn reads_title_and_disc_from_an_iso_and_names_it_from_the_list() {
        // 465307D3 is in the embedded list.
        let img = disc(0x0FD9_0000, &xex::build(0x08FE_F3C1, 0x4653_07D3, 2, 3));
        let m = read_iso(&img).unwrap();
        assert_eq!(
            (m.title_id.as_str(), m.media_id.as_deref(), m.disc, m.discs),
            ("465307D3", Some("08FEF3C1"), Some(2), Some(3))
        );
        assert!(m.game_name.unwrap().contains("eNCHANT"));
    }

    #[test]
    fn clear_errors_for_things_that_are_not_games() {
        let mut img = crate::xbox::xiso::testimg::Img::default();
        img.put(0, &vec![0u8; 0x20000]);
        assert!(
            read_iso(&img)
                .unwrap_err()
                .contains("Not an Xbox disc image")
        );
        assert!(
            read_iso(&disc(0, b"MZ this is not an xex"))
                .unwrap_err()
                .contains("Not an XEX2")
        );
    }

    #[test]
    fn reads_a_god_folder_and_prefers_the_game_content_type() {
        let d = std::env::temp_dir().join(format!("rustybox_god_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let t = d.join("4D530805");
        std::fs::create_dir_all(t.join("00007000")).unwrap();
        std::fs::create_dir_all(t.join("00004000")).unwrap();
        std::fs::write(
            t.join("00007000/ABCDEF"),
            stfs::build(b"LIVE", 0x7000, 0x4D53_0805, 0, "Alan Wake", "Alan Wake"),
        )
        .unwrap();
        std::fs::write(
            t.join("00004000/AAAAAA"),
            stfs::build(b"LIVE", 0x4000, 0x4D53_0805, 0, "Other", "Other"),
        )
        .unwrap();
        let m = read_god_dir(&t).unwrap();
        assert_eq!(
            (m.title_id.as_str(), m.platform.as_str()),
            ("4D530805", "xbox360")
        );
        assert!(m.game_name.is_some());

        // Content type 5000 is an original Xbox game on demand, unless the title is a known 360 one.
        let x360 = d.join("584108DB"); // A Kingdom for Keflings (Arcade)
        std::fs::create_dir_all(x360.join("00005000")).unwrap();
        std::fs::write(
            x360.join("00005000/ABCDEF"),
            stfs::build(b"LIVE", 0x5000, 0x5841_08DB, 0, "Keflings", "Keflings"),
        )
        .unwrap();
        assert_eq!(read_god_dir(&x360).unwrap().platform, "xbox360");
        let old = d.join("4D530064");
        std::fs::create_dir_all(old.join("00005000")).unwrap();
        std::fs::write(
            old.join("00005000/ABCDEF"),
            stfs::build(b"LIVE", 0x5000, 0x4D53_0064, 0, "Halo CE", "Halo CE"),
        )
        .unwrap();
        assert_eq!(read_god_dir(&old).unwrap().platform, "xbox");

        let empty = d.join("DEADBEEF");
        std::fs::create_dir_all(empty.join("00007000")).unwrap();
        assert!(read_god_dir(&empty).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    fn god(dir: &std::path::Path, ct: &str, container: &[u8], data: &[&[u8]]) {
        let c = dir.join(ct);
        std::fs::create_dir_all(c.join("ABCDEF.data")).unwrap();
        std::fs::write(c.join("ABCDEF"), container).unwrap();
        for (i, d) in data.iter().enumerate() {
            std::fs::write(c.join(format!("ABCDEF.data/Data{i:04}")), d).unwrap();
        }
    }

    /// A container whose header says it has `parts` data files.
    fn container_with(parts: u32) -> Vec<u8> {
        let mut b = stfs::build(b"LIVE", 0x7000, 0x4D53_0805, 1, "Alan Wake", "Alan Wake");
        b[0x3A0..0x3A4].copy_from_slice(&parts.to_le_bytes());
        b
    }

    #[test]
    fn incomplete_copies_are_recognised_and_explained() {
        let d = std::env::temp_dir().join(format!("rustybox_health_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);

        // Healthy: every part present and non-empty.
        god(
            &d.join("ok/4D530805"),
            "00007000",
            &container_with(2),
            &[b"aaa", b"bbb"],
        );
        let m = read_god_dir(&d.join("ok/4D530805")).unwrap();
        assert!(m.health.is_none(), "{:?}", m.health);
        assert_eq!(
            (m.content_type, content_kind(m.content_type)),
            (Some(0x7000), "game")
        );

        // As seen on a real drive: an empty container file and empty data parts (an interrupted copy).
        god(
            &d.join("empty/534507D6"),
            "00007000",
            b"",
            &[b"", b"", b"x"],
        );
        let err = read_god_dir(&d.join("empty/534507D6")).unwrap_err();
        assert!(
            err.contains("empty (0 bytes)") && err.contains("incompletely"),
            "{err}"
        );

        // A fine header but some data parts empty or missing.
        god(
            &d.join("short/4D530805"),
            "00007000",
            &container_with(4),
            &[b"aaa", b"", b"ccc"],
        );
        let m = read_god_dir(&d.join("short/4D530805")).unwrap();
        let h = m.health.unwrap();
        assert!(
            h.contains("1 of 3 data files are empty")
                && h.contains("1 of 4 data files are missing"),
            "{h}"
        );

        // Add-on content is not a game.
        let mut dlc = stfs::build(
            b"LIVE",
            0x2,
            0x5841_09DB,
            0,
            "Curse of the Zombiesaurus",
            "A World of Keflings",
        );
        dlc[0x3A0..0x3A4].copy_from_slice(&0u32.to_le_bytes());
        god(&d.join("dlc/584109DB"), "00000002", &dlc, &[b"x"]);
        let m = read_god_dir(&d.join("dlc/584109DB")).unwrap();
        assert_eq!(content_kind(m.content_type), "dlc");
        // A single-file package (add-on or Xbox Live Arcade) has no .data folder, and that is normal.
        let single = d.join("arcade/58410B8D/000D0000");
        std::fs::create_dir_all(&single).unwrap();
        std::fs::write(
            single.join("PKG"),
            stfs::build(b"LIVE", 0xD0000, 0x5841_0B8D, 0, "Deadlight", "Deadlight"),
        )
        .unwrap();
        let m = read_god_dir(&d.join("arcade/58410B8D")).unwrap();
        assert_eq!((m.health, content_kind(m.content_type)), (None, "game"));
        assert_eq!(
            (
                content_kind(Some(0xD0000)),
                content_kind(Some(0xB0000)),
                content_kind(None),
                content_kind(Some(0x9999))
            ),
            ("game", "update", "game", "other")
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_game_content_is_preferred_when_a_title_also_holds_add_ons() {
        let d = std::env::temp_dir().join(format!("rustybox_pref_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let t = d.join("454108D4");
        god(&t, "00000002", &container_with(1), &[b"x"]);
        god(&t, "00007000", &container_with(1), &[b"x"]);
        let c = find_god_container(&t).unwrap();
        assert!(c.to_string_lossy().contains("00007000"), "{c:?}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
