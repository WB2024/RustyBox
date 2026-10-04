//! Finding games that are in a library more than once, and choosing which copy to keep.
//!
//! Two items are the same game when they have the same title ID, release (media ID) and disc, in
//! whatever folders. The copy to keep is chosen by clear rules, in order, and each removal says
//! which rule decided it, so nothing is a surprise:
//!
//! 1. a copy that can be reached beats one on an offline disk;
//! 2. a copy that isn't damaged beats a damaged one;
//! 3. the larger copy beats the smaller (an interrupted copy is smaller);
//! 4. one already in the tidy place (`Game Name/TitleID`) beats one that isn't;
//! 5. the folder higher in the library's list, then the newer files, then the older entry.

use std::collections::BTreeMap;

use rusqlite::Connection;
use serde::Serialize;

use super::tidy::expected;
use crate::error::Error;

#[derive(Debug, Clone, Serialize)]
pub struct DupItem {
    pub item_id: i64,
    pub path_id: i64,
    pub path_label: String,
    pub relpath: String,
    pub size: i64,
    pub health: Option<String>,
    pub available: bool,
    pub writable: bool,
    pub keep: bool,
    /// Why this copy is kept, or why it loses to the kept one.
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DupGroup {
    pub title_id: String,
    pub name: String,
    pub disc: Option<i64>,
    pub media_id: Option<String>,
    pub items: Vec<DupItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Duplicates {
    pub groups: Vec<DupGroup>,
    /// The copies that would be removed (those in a writable, reachable folder).
    pub remove: Vec<i64>,
    pub bytes: i64,
}

struct Row {
    item_id: i64,
    path_id: i64,
    path_label: String,
    relpath: String,
    kind: String,
    size: i64,
    mtime: i64,
    title_id: String,
    media_id: Option<String>,
    game_name: Option<String>,
    disc: Option<i64>,
    health: Option<String>,
    available: bool,
    writable: bool,
    priority: i64,
}

pub fn find(conn: &Connection, library_id: i64, layout: &str) -> Result<Duplicates, Error> {
    let mut stmt = conn.prepare(
        "SELECT i.id, i.path_id, p.label, p.path, i.relpath, i.kind, i.size, i.mtime, i.title_id, i.media_id,
                i.game_name, i.disc, i.health, i.available, p.writable, p.priority
         FROM items i JOIN library_paths p ON p.id = i.path_id
         WHERE i.library_id = ?1 AND i.title_id IS NOT NULL AND i.kind IN ('iso', 'god')
           AND (i.content_type IS NULL OR i.content_type IN ('00007000', '00005000', '000D0000', '00004000'))
         ORDER BY i.title_id, i.disc, i.relpath",
    )?;
    let rows = stmt
        .query_map([library_id], |r| {
            let label: String = r.get(2)?;
            let path: String = r.get(3)?;
            Ok(Row {
                item_id: r.get(0)?,
                path_id: r.get(1)?,
                path_label: if label.is_empty() { path } else { label },
                relpath: r.get(4)?,
                kind: r.get(5)?,
                size: r.get(6)?,
                mtime: r.get(7)?,
                title_id: r.get(8)?,
                media_id: r.get(9)?,
                game_name: r.get(10)?,
                disc: r.get(11)?,
                health: r.get(12)?,
                available: r.get::<_, i64>(13)? != 0,
                writable: r.get::<_, i64>(14)? != 0,
                priority: r.get(15)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut by: BTreeMap<(String, String, i64, String), Vec<Row>> = BTreeMap::new();
    for r in rows {
        by.entry((
            r.title_id.to_uppercase(),
            r.media_id.clone().unwrap_or_default().to_uppercase(),
            r.disc.unwrap_or(0),
            r.kind.clone(),
        ))
        .or_default()
        .push(r);
    }

    let mut out = Duplicates {
        groups: vec![],
        remove: vec![],
        bytes: 0,
    };
    for (_, mut rows) in by {
        if rows.len() < 2 {
            continue;
        }
        let tidy = |r: &Row| r.relpath == expected(layout, &r.title_id, r.game_name.as_deref());
        // Best first.
        rows.sort_by(|a, b| {
            let key = |r: &Row| {
                (
                    r.available,
                    r.health.is_none(),
                    r.size,
                    tidy(r),
                    std::cmp::Reverse(r.priority),
                    r.mtime,
                    std::cmp::Reverse(r.item_id),
                )
            };
            key(b).cmp(&key(a))
        });
        let best = &rows[0];
        let items: Vec<DupItem> = rows
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let reason = if i == 0 {
                    "Kept: the best copy".to_string()
                } else if r.available != best.available {
                    "Its disk isn't reachable right now".to_string()
                } else if r.health.is_some() != best.health.is_some() {
                    format!("Damaged: {}", r.health.clone().unwrap_or_default())
                } else if r.size != best.size {
                    format!("Smaller ({} bytes against {})", r.size, best.size)
                } else if tidy(r) != tidy(best) {
                    "Not in the tidy place".to_string()
                } else if r.priority != best.priority {
                    "In a lower-priority folder".to_string()
                } else if r.mtime != best.mtime {
                    "An older copy".to_string()
                } else {
                    "An identical copy".to_string()
                };
                DupItem {
                    item_id: r.item_id,
                    path_id: r.path_id,
                    path_label: r.path_label.clone(),
                    relpath: r.relpath.clone(),
                    size: r.size,
                    health: r.health.clone(),
                    available: r.available,
                    writable: r.writable,
                    keep: i == 0,
                    reason,
                }
            })
            .collect();
        for it in items
            .iter()
            .filter(|i| !i.keep && i.available && i.writable)
        {
            out.remove.push(it.item_id);
            out.bytes += it.size;
        }
        out.groups.push(DupGroup {
            title_id: best.title_id.to_uppercase(),
            name: best
                .game_name
                .clone()
                .unwrap_or_else(|| best.title_id.clone()),
            disc: best.disc,
            media_id: best.media_id.clone(),
            items,
        });
    }
    Ok(out)
}
