//! Title updates (TUs): reading them from files, finding more on XboxUnity, checking they fit a
//! game, and where they go on the console.

pub mod unity;

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use serde::Serialize;

use crate::{
    console::{Console, ftp::ftp_path},
    xbox::{hex8, stfs},
};

pub const TU_CONTENT_TYPE: u32 = 0x000B_0000;

/// A title update found in a library folder.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LocalTu {
    /// Relative to the library folder.
    pub relpath: String,
    pub size: u64,
    pub title_id: String,
    pub media_id: String,
    pub name: String,
    /// The update's number (TU3 is 3), 0 if the header doesn't say.
    pub tu_version: u32,
    pub base_version: String,
}

/// The update's number (the 3 in "Title Update #3", "TU3" or `..._TU3_...`). Real packages don't
/// keep it in a header field, so it comes from their name, then from the file's name.
pub fn tu_number(display_name: &str, file_name: &str) -> u32 {
    fn after<'a>(s: &'a str, marker: &str) -> Option<&'a str> {
        let i = s.to_lowercase().find(&marker.to_lowercase())?;
        s.get(i + marker.len()..)
    }
    fn digits(s: &str) -> Option<u32> {
        let d: String = s
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        d.parse().ok().filter(|n| (1..=9999).contains(n))
    }
    for (src, marker) in [
        (display_name, "Update #"),
        (display_name, "TU"),
        (display_name, "Update "),
        (file_name, "_TU"),
        (file_name, "TU"),
    ] {
        if let Some(n) = after(src, marker).and_then(digits) {
            return n;
        }
    }
    0
}

/// Read a file as a title update, or `None` if it is anything else.
pub fn read_tu(path: &Path) -> Option<LocalTu> {
    let mut buf = vec![0u8; 0x3A4 + 0x1400];
    let n = fs::File::open(path).ok()?.read(&mut buf).ok()?;
    buf.truncate(n);
    let h = stfs::parse(&buf).ok()?;
    if h.content_type != TU_CONTENT_TYPE {
        return None;
    }
    let name = if h.title_name.is_empty() {
        h.display_name.clone()
    } else {
        h.title_name.clone()
    };
    Some(LocalTu {
        relpath: String::new(),
        size: fs::metadata(path).ok()?.len(),
        title_id: h.title_id,
        media_id: h.media_id,
        name: if name.is_empty() {
            path.file_stem()?.to_string_lossy().to_string()
        } else {
            name
        },
        tu_version: tu_number(
            &h.display_name,
            &path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
        ),
        base_version: hex8(h.base_version),
    })
}

/// Every title update under a folder, however deep.
pub fn scan_dir(root: &Path) -> Vec<LocalTu> {
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name.ends_with(".part") {
                continue;
            }
            let Ok(m) = fs::metadata(&p) else { continue };
            if m.is_dir() {
                stack.push(p);
            } else if m.len() > 0x3A4
                && let Some(mut tu) = read_tu(&p)
            {
                tu.relpath = p
                    .strip_prefix(root)
                    .map(|r| r.to_string_lossy().to_string())
                    .unwrap_or(name);
                out.push(tu);
            }
        }
    }
    out.sort_by(|a, b| {
        (&a.title_id, a.tu_version, &a.relpath).cmp(&(&b.title_id, b.tu_version, &b.relpath))
    });
    out
}

/// Where a title update goes on the console: in the games drive's Content folder.
pub fn install_path(c: &Console, title_id: &str, file_name: &str) -> String {
    let drive = ftp_path(&c.game_paths[0])
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or("Hdd1")
        .to_string();
    format!(
        "/{drive}/Content/0000000000000000/{}/000B0000/{file_name}",
        title_id.to_uppercase()
    )
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Compat {
    /// The update's media ID is one of the game's known discs.
    Compatible,
    /// Known discs exist and none matches: it is for a different release.
    Incompatible,
    /// There isn't enough information to say.
    Unknown,
}

/// Does an update (by its media ID) fit a game? `owned` are media IDs of copies you have.
pub fn compat(tu_media: &str, title_id: &str, owned: &[String]) -> Compat {
    let m = tu_media.trim().to_uppercase();
    if m.is_empty() || m == "00000000" {
        return Compat::Unknown;
    }
    let mut known = crate::xbox::catalog::get().media_ids(title_id);
    known.extend(owned.iter().map(|o| o.to_uppercase()));
    if known.is_empty() {
        Compat::Unknown
    } else if known.contains(&m) {
        Compat::Compatible
    } else {
        Compat::Incompatible
    }
}

/// A title update package for mock mode and tests.
pub fn build_tu(title_id: u32, media_id: u32, tu_version: u32, name: &str) -> Vec<u8> {
    let label = format!("{name} Title Update #{tu_version}");
    let mut b = stfs::build(b"LIVE", TU_CONTENT_TYPE, title_id, media_id, &label, name);
    b.extend((0..2048u32).map(|i| (i % 251) as u8));
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_updates_are_read_and_nothing_else() {
        let d = std::env::temp_dir().join(format!("rustybox_tu_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("4D530805 - Alan Wake")).unwrap();
        fs::write(
            d.join("4D530805 - Alan Wake/TU2"),
            build_tu(0x4D53_0805, 0x54E3_4DF4, 2, "Alan Wake"),
        )
        .unwrap();
        fs::write(
            d.join("flat_tu"),
            build_tu(0x4D53_07D5, 0x76E9_DF5B, 7, "Gears of War"),
        )
        .unwrap();
        // A game package and a text file are not updates.
        fs::write(
            d.join("game"),
            stfs::build(b"LIVE", 0x7000, 0x4D53_0805, 1, "x", "x"),
        )
        .unwrap();
        fs::write(d.join("notes.txt"), vec![b'a'; 5000]).unwrap();
        let found = scan_dir(&d);
        assert_eq!(found.len(), 2, "{found:?}");
        let aw = found.iter().find(|t| t.title_id == "4D530805").unwrap();
        assert_eq!((aw.tu_version, aw.media_id.as_str()), (2, "54E34DF4"));
        assert_eq!(aw.relpath, "4D530805 - Alan Wake/TU2");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn the_update_number_comes_from_the_name() {
        assert_eq!(tu_number("Halo 3 FINAL (German) Title Update #3", "x"), 3);
        assert_eq!(tu_number("Alan Wake TU12", "x"), 12);
        assert_eq!(tu_number("Something", "4D5307E6_TU7_21581"), 7);
        assert_eq!(tu_number("Something", "TU2"), 2);
        assert_eq!(tu_number("Update 5 for the game", "x"), 5);
        assert_eq!(tu_number("No number here", "file"), 0);
    }

    #[test]
    fn updates_go_to_the_content_folder_of_the_games_drive() {
        let mut c = Console {
            host: "x".into(),
            ..Default::default()
        };
        assert_eq!(
            install_path(&c, "4d530805", "TU2"),
            "/Hdd1/Content/0000000000000000/4D530805/000B0000/TU2"
        );
        c.game_paths = vec!["Usb0/Games".into()];
        assert!(install_path(&c, "4D530805", "TU2").starts_with("/Usb0/Content/"));
    }

    #[test]
    fn compatibility_uses_known_and_owned_discs() {
        // Alan Wake's known media IDs include the one used in the tests above.
        assert_eq!(compat("", "4D530805", &[]), Compat::Unknown);
        assert_eq!(
            compat("DEADBEEF", "4D530805", &["DEADBEEF".into()]),
            Compat::Compatible
        );
        assert_eq!(compat("DEADBEEF", "4D530805", &[]), Compat::Incompatible);
        assert_eq!(compat("DEADBEEF", "FFFFFFFF", &[]), Compat::Unknown);
    }
}
