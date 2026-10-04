//! Working out what a conversion would do, before anything is written.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use super::{god, image};
use crate::{
    error::Error,
    library::{self, Item, LibPath, health},
    xbox::{catalog, meta},
};

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    IsoToGod,
    GodToIso,
    /// Unpack an ISO to a game folder.
    Extract,
    /// Pack a game folder into an ISO.
    Create,
}

impl Op {
    pub fn label(self) -> &'static str {
        match self {
            Op::IsoToGod => "Convert to GOD",
            Op::GodToIso => "Convert to ISO",
            Op::Extract => "Unpack to folder",
            Op::Create => "Create ISO",
        }
    }

    /// The kind of library item this operation reads (`None` when it reads a folder).
    pub fn source_kind(self) -> Option<&'static str> {
        match self {
            Op::IsoToGod | Op::Extract => Some("iso"),
            Op::GodToIso => Some("god"),
            Op::Create => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ItemRef {
    pub library_id: i64,
    pub item_id: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Request {
    pub op: Op,
    #[serde(default)]
    pub items: Vec<ItemRef>,
    /// Create only: the game folder to pack (an absolute path inside the allowed roots).
    pub source_folder: Option<String>,
    pub dest_library_id: i64,
    pub dest_path_id: i64,
    /// `name_titleid` or `titleid`; defaults to the setting.
    pub layout: Option<String>,
    #[serde(default)]
    pub overwrite: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub name: String,
    pub title_id: Option<String>,
    pub media_id: Option<String>,
    pub disc: Option<i64>,
    pub discs: Option<i64>,
    /// The ISO, the GOD container file, or the game folder being read.
    pub source: PathBuf,
    /// Where the result ends up: the GOD container file, the ISO, or the unpacked folder.
    pub output: PathBuf,
    /// For GOD output, the `.data` folder next to the container.
    pub output_data: Option<PathBuf>,
    pub bytes_in: u64,
    /// An estimate of what will be written.
    pub bytes_out: u64,
    pub exists: bool,
    pub problems: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub op: Op,
    pub op_label: &'static str,
    pub dest_root: PathBuf,
    pub dest_label: String,
    pub free: u64,
    pub total_out: u64,
    pub overwrite: bool,
    pub entries: Vec<Entry>,
    /// Problems that apply to the whole plan.
    pub problems: Vec<String>,
    /// True when nothing stands in the way.
    pub ok: bool,
}

/// What the plan needs from the database, read in one go so no file is touched while it is held.
pub struct Gathered {
    pub dest: LibPath,
    pub dest_library: String,
    pub sources: Vec<(Item, LibPath)>,
    /// (title ID, media ID) of GOD games already in a library, with the library's name.
    pub god_copies: Vec<(String, Option<String>, String)>,
    pub iso_copies: Vec<(String, Option<String>, String)>,
}

pub fn gather(conn: &Connection, req: &Request) -> Result<Gathered, Error> {
    let dest_lib = library::get_library(conn, req.dest_library_id)?;
    let dest = library::get_path(conn, req.dest_library_id, req.dest_path_id)?;
    let mut sources = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for r in &req.items {
        if seen.insert((r.library_id, r.item_id)) {
            sources.push(library::get_item(conn, r.library_id, r.item_id)?);
        }
    }
    let copies = |kind: &str| -> Result<Vec<(String, Option<String>, String)>, Error> {
        let mut stmt = conn.prepare("SELECT i.title_id, i.media_id, l.name FROM items i JOIN libraries l ON l.id = i.library_id WHERE i.kind = ?1 AND i.title_id IS NOT NULL")?;
        let rows = stmt.query_map([kind], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    };
    Ok(Gathered {
        dest,
        dest_library: dest_lib.name,
        sources,
        god_copies: copies("god")?,
        iso_copies: copies("iso")?,
    })
}

/// A name that is safe to use as a file or folder name on any filesystem the console might see.
pub fn safe_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                ' '
            } else {
                c
            }
        })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim_matches(|c: char| c == '.' || c.is_whitespace());
    trimmed
        .chars()
        .take(120)
        .collect::<String>()
        .trim_end()
        .to_string()
}

fn disc_suffix(disc: Option<i64>, discs: Option<i64>) -> String {
    match (disc, discs) {
        (Some(d), Some(n)) if n > 1 => format!(" (Disc {d})"),
        _ => String::new(),
    }
}

fn display_name(item: &Item) -> String {
    let n = safe_name(item.game_name.as_deref().unwrap_or(&item.name));
    if n.is_empty() {
        safe_name(&item.name)
    } else {
        n
    }
}

/// Slack kept free on the destination, so a conversion never fills a disk to the brim.
const FREE_MARGIN: u64 = 256 * 1024 * 1024;

pub fn build(
    g: &Gathered,
    req: &Request,
    roots: &[PathBuf],
    default_layout: &str,
) -> Result<Plan, Error> {
    let dest_root = PathBuf::from(&g.dest.path);
    let mut problems = Vec::new();
    if g.dest.is_remote() {
        problems.push(
            "Conversions are written to a folder on this server. Convert into a local library, then send the result to the drive."
                .to_string(),
        );
    }
    let h = health::check_blocking(&dest_root);
    if !g.dest.writable {
        problems.push(
            "The destination folder is read-only. Turn on writing for it in its library first."
                .to_string(),
        );
    }
    if !h.online {
        problems.push(format!(
            "The destination folder is offline: {}",
            h.problem.clone().unwrap_or_default()
        ));
    }
    let layout = req
        .layout
        .clone()
        .unwrap_or_else(|| default_layout.to_string());
    if !crate::settings::LAYOUTS.contains(&layout.as_str()) {
        problems.push(format!("Unknown GOD layout '{layout}'"));
    }

    let mut entries = Vec::new();
    match req.op {
        Op::Create => {
            let Some(raw) = req.source_folder.as_deref() else {
                return Err(Error::validation("Choose the game folder to pack"));
            };
            let folder = library::validate_path(roots, raw)?;
            entries.push(entry_create(&folder, &dest_root));
        }
        _ => {
            if g.sources.is_empty() {
                return Err(Error::validation("Choose at least one game"));
            }
            for (item, lp) in &g.sources {
                entries.push(entry_for(g, req.op, item, lp, &dest_root, &layout));
            }
        }
    }

    // Per-entry checks that don't depend on the operation.
    let mut seen_outputs: HashMap<PathBuf, usize> = HashMap::new();
    for (i, e) in entries.iter_mut().enumerate() {
        e.exists = e.output.exists() || e.output_data.as_ref().is_some_and(|d| d.exists());
        if e.exists {
            if req.overwrite && req.op != Op::Extract {
                e.warnings
                    .push("Already exists: it will be replaced.".into());
            } else if req.op == Op::Extract {
                e.problems.push(
                    "That folder already exists. Remove it or choose another destination.".into(),
                );
            } else {
                e.problems
                    .push("Already exists. Tick \"replace existing\" to overwrite it.".into());
            }
        }
        if let Some(first) = seen_outputs.insert(e.output.clone(), i) {
            e.problems.push(format!(
                "Two of the chosen games would be written to the same place (see #{}).",
                first + 1
            ));
        }
    }
    let total_out: u64 = entries.iter().map(|e| e.bytes_out).sum();
    if h.online && total_out + FREE_MARGIN > h.free {
        problems.push(format!("Not enough free space: about {total_out} bytes are needed (plus a margin) and {} are free.", h.free));
    }
    let ok = problems.is_empty() && entries.iter().all(|e| e.problems.is_empty());
    Ok(Plan {
        op: req.op,
        op_label: req.op.label(),
        dest_root,
        dest_label: if g.dest.label.is_empty() {
            g.dest_library.clone()
        } else {
            format!("{} / {}", g.dest_library, g.dest.label)
        },
        free: h.free,
        total_out,
        overwrite: req.overwrite,
        entries,
        problems,
        ok,
    })
}

fn blank(name: String, source: PathBuf, output: PathBuf) -> Entry {
    Entry {
        name,
        title_id: None,
        media_id: None,
        disc: None,
        discs: None,
        source,
        output,
        output_data: None,
        bytes_in: 0,
        bytes_out: 0,
        exists: false,
        problems: vec![],
        warnings: vec![],
    }
}

fn entry_create(folder: &Path, dest_root: &Path) -> Entry {
    let name = safe_name(
        &folder
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
    );
    let name = if name.is_empty() {
        "game".to_string()
    } else {
        name
    };
    let mut e = blank(
        name.clone(),
        folder.to_path_buf(),
        dest_root.join(format!("{name}.iso")),
    );
    e.bytes_in = god::dir_size(folder);
    e.bytes_out = e.bytes_in + 4 * 1024 * 1024;
    let has_exe = std::fs::read_dir(folder).is_ok_and(|rd| {
        rd.flatten().any(|f| {
            ["default.xex", "default.xbe"]
                .contains(&f.file_name().to_string_lossy().to_lowercase().as_str())
        })
    });
    if !has_exe {
        e.warnings
            .push("There is no default.xex in this folder, so it may not be a game folder.".into());
    }
    if e.bytes_in == 0 {
        e.problems.push("The folder is empty.".into());
    }
    e
}

fn entry_for(
    g: &Gathered,
    op: Op,
    item: &Item,
    lp: &LibPath,
    dest_root: &Path,
    layout: &str,
) -> Entry {
    let name = display_name(item);
    let source = Path::new(&lp.path).join(&item.relpath);
    let mut e = blank(name.clone(), source.clone(), PathBuf::new());
    if lp.is_remote() {
        e.problems.push(
            "This game is on a remote drive. Import it into a local library first, then convert it."
                .into(),
        );
        e.output = dest_root.join(&name);
        return e;
    }
    e.title_id = item.title_id.clone();
    e.media_id = item.media_id.clone();
    e.disc = item.disc;
    e.discs = item.discs;

    if Some(item.kind.as_str()) != op.source_kind() {
        e.problems.push(format!(
            "This is a {} item; \"{}\" needs {}.",
            item.kind.to_uppercase(),
            op.label(),
            op.source_kind().unwrap_or("a folder").to_uppercase()
        ));
        e.output = dest_root.join(&name);
        return e;
    }
    if !item.available || !source.exists() {
        e.problems
            .push("The source isn't available right now (is its disk or share mounted?).".into());
        e.output = dest_root.join(&name);
        return e;
    }

    let suffix = disc_suffix(item.disc, item.discs);
    match op {
        Op::IsoToGod => match god::inspect_iso(&source) {
            Ok((title, media, ct, used)) => {
                let ct = format!("{:08X}", ct as u32);
                let base = if layout == "titleid" {
                    dest_root.to_path_buf()
                } else {
                    dest_root.join(&name)
                };
                let container = base.join(&title).join(&ct).join(&media);
                e.output_data = Some(container.with_file_name(format!("{media}.data")));
                e.output = container;
                e.title_id = Some(title.clone());
                e.media_id = Some(media.clone());
                e.bytes_in = used;
                e.bytes_out = used + used / 100 + 1024 * 1024;
                if let Some(lib) = g
                    .god_copies
                    .iter()
                    .find(|(t, m, _)| {
                        *t == title && (m.is_none() || m.as_deref() == Some(media.as_str()))
                    })
                    .map(|c| &c.2)
                {
                    e.warnings.push(format!(
                        "A GOD copy of this game already exists (in {lib})."
                    ));
                }
            }
            Err(err) => {
                e.problems.push(format!("Can't read this image: {err}"));
                e.output = dest_root.join(&name);
            }
        },
        Op::GodToIso => match meta::find_god_container(&source) {
            Ok(container) => {
                e.output = dest_root.join(format!("{name}{suffix}.iso"));
                let bytes: u64 = god::data_parts(&container)
                    .map(|p| {
                        p.iter()
                            .filter_map(|f| std::fs::metadata(f).ok())
                            .map(|m| m.len())
                            .sum()
                    })
                    .unwrap_or(0);
                if bytes == 0 {
                    e.problems.push("The package has no data files.".into());
                }
                e.bytes_in = bytes;
                e.bytes_out = bytes;
                e.source = container;
                if let Some(t) = &item.title_id
                    && let Some(lib) = g
                        .iso_copies
                        .iter()
                        .find(|(ti, m, _)| {
                            ti == t && (m.is_none() || m.as_deref() == item.media_id.as_deref())
                        })
                        .map(|c| &c.2)
                {
                    e.warnings
                        .push(format!("An ISO of this game already exists (in {lib})."));
                }
            }
            Err(err) => {
                e.problems
                    .push(format!("Can't find the game's container: {err}"));
                e.output = dest_root.join(format!("{name}{suffix}.iso"));
            }
        },
        Op::Extract => {
            e.output = dest_root.join(format!("{name}{suffix}"));
            match image::unpack_size(&source) {
                Ok((_files, bytes)) => {
                    e.bytes_in = bytes;
                    e.bytes_out = bytes;
                }
                Err(err) => e.problems.push(format!("Can't read this image: {err}")),
            }
        }
        Op::Create => unreachable!("handled by entry_create"),
    }
    // A game the title list doesn't know still converts; just say so.
    if item.title_id.is_some()
        && item.game_name.is_none()
        && catalog::get()
            .name(item.title_id.as_deref().unwrap_or(""), None)
            .is_none()
    {
        e.warnings
            .push("This title isn't in the title list, so it is named from its file name.".into());
    }
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_safe_for_any_filesystem() {
        assert_eq!(safe_name("Halo 3: ODST / Disc 1"), "Halo 3 ODST Disc 1");
        assert_eq!(safe_name("  ..hidden.. "), "hidden");
        assert_eq!(safe_name("a\\b|c?d*e\"f<g>h"), "a b c d e f g h");
        assert_eq!(safe_name("../../etc"), "etc");
        assert_eq!(safe_name("///"), "");
        assert_eq!(safe_name(&"x".repeat(300)).len(), 120);
        assert_eq!(disc_suffix(Some(2), Some(3)), " (Disc 2)");
        assert_eq!(disc_suffix(Some(1), Some(1)), "");
        assert_eq!(disc_suffix(None, None), "");
    }
}
