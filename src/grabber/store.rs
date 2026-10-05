//! The wanted list, what was grabbed for each game, and the blocklist (in the database).

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::{error::Error, jobs::now};

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Wanted {
    pub id: i64,
    pub igdb_id: Option<i64>,
    pub name: String,
    pub release: Option<i64>,
    pub summary: Option<String>,
    pub cover: Option<String>,
    pub title_id: Option<String>,
    pub monitored: bool,
    /// `wanted`, `downloading` or `downloaded`.
    pub status: String,
    pub added: i64,
    pub last_search: Option<i64>,
    pub last_result: Option<String>,
    pub note: Option<String>,
}

const WCOLS: &str = "id, igdb_id, name, release, summary, cover, title_id, monitored, status, added, last_search, last_result, note";

fn wmap(r: &rusqlite::Row) -> rusqlite::Result<Wanted> {
    Ok(Wanted {
        id: r.get(0)?,
        igdb_id: r.get(1)?,
        name: r.get(2)?,
        release: r.get(3)?,
        summary: r.get(4)?,
        cover: r.get(5)?,
        title_id: r.get(6)?,
        monitored: r.get::<_, i64>(7)? != 0,
        status: r.get(8)?,
        added: r.get(9)?,
        last_search: r.get(10)?,
        last_result: r.get(11)?,
        note: r.get(12)?,
    })
}

pub struct NewWanted<'a> {
    pub igdb_id: Option<i64>,
    pub name: &'a str,
    pub release: Option<i64>,
    pub summary: Option<&'a str>,
    pub cover: Option<&'a str>,
    pub title_id: Option<&'a str>,
}

pub fn add_wanted(c: &Connection, n: &NewWanted) -> Result<i64, Error> {
    if n.name.trim().is_empty() {
        return Err(Error::validation("A game needs a name"));
    }
    c.execute(
        "INSERT INTO wanted(igdb_id, name, release, summary, cover, title_id, added) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![n.igdb_id, n.name.trim(), n.release, n.summary, n.cover, n.title_id, now() as i64],
    )
    .map_err(|e| match e {
        rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation => Error::conflict(format!("{} is already on the wanted list", n.name)),
        e => e.into(),
    })?;
    Ok(c.last_insert_rowid())
}

pub fn list_wanted(c: &Connection) -> Result<Vec<Wanted>, Error> {
    let mut s = c.prepare(&format!(
        "SELECT {WCOLS} FROM wanted ORDER BY name COLLATE NOCASE"
    ))?;
    Ok(s.query_map([], wmap)?.collect::<Result<Vec<_>, _>>()?)
}

pub fn get_wanted(c: &Connection, id: i64) -> Result<Wanted, Error> {
    c.query_row(
        &format!("SELECT {WCOLS} FROM wanted WHERE id = ?1"),
        [id],
        wmap,
    )
    .optional()?
    .ok_or_else(|| Error::not_found(format!("No wanted game #{id}")))
}

pub fn set_monitored(
    c: &Connection,
    id: i64,
    monitored: bool,
    note: Option<&str>,
) -> Result<(), Error> {
    get_wanted(c, id)?;
    c.execute(
        "UPDATE wanted SET monitored = ?1, note = COALESCE(?2, note) WHERE id = ?3",
        params![monitored, note, id],
    )?;
    Ok(())
}

pub fn set_wanted_status(c: &Connection, id: i64, status: &str) -> Result<(), Error> {
    c.execute(
        "UPDATE wanted SET status = ?1 WHERE id = ?2",
        params![status, id],
    )?;
    Ok(())
}

pub fn touch_search(c: &Connection, id: i64, result: &str) -> Result<(), Error> {
    c.execute(
        "UPDATE wanted SET last_search = ?1, last_result = ?2 WHERE id = ?3",
        params![now() as i64, result, id],
    )?;
    Ok(())
}

pub fn delete_wanted(c: &Connection, id: i64) -> Result<(), Error> {
    get_wanted(c, id)?;
    c.execute("DELETE FROM grabs WHERE wanted_id = ?1", [id])?;
    c.execute("DELETE FROM wanted WHERE id = ?1", [id])?;
    Ok(())
}

// ── Grabs ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Grab {
    pub id: i64,
    pub wanted_id: i64,
    pub guid: String,
    pub title: String,
    pub indexer: String,
    pub size: i64,
    pub score: i64,
    pub sab_id: Option<String>,
    /// `queued`, `downloading`, `completed` (waiting to be imported), `unpacking` (a zip is being
    /// opened), `importing`, `imported`, `failed` or `import_failed`.
    pub status: String,
    pub progress: f64,
    pub added: i64,
    pub updated: i64,
    pub path: Option<String>,
    pub error: Option<String>,
    pub import_job: Option<i64>,
    /// Title IDs of the games the download held (comma separated), once it has been looked at.
    pub title_ids: Option<String>,
    /// The job that sent the imported games on to the console.
    pub extra_job: Option<i64>,
    /// `usenet` or `torrent`.
    pub protocol: String,
}

const GCOLS: &str = "id, wanted_id, guid, title, indexer, size, score, sab_id, status, progress, added, updated, path, error, import_job, title_ids, extra_job, protocol";

fn gmap(r: &rusqlite::Row) -> rusqlite::Result<Grab> {
    Ok(Grab {
        id: r.get(0)?,
        wanted_id: r.get(1)?,
        guid: r.get(2)?,
        title: r.get(3)?,
        indexer: r.get(4)?,
        size: r.get(5)?,
        score: r.get(6)?,
        sab_id: r.get(7)?,
        status: r.get(8)?,
        progress: r.get(9)?,
        added: r.get(10)?,
        updated: r.get(11)?,
        path: r.get(12)?,
        error: r.get(13)?,
        import_job: r.get(14)?,
        title_ids: r.get(15)?,
        extra_job: r.get(16)?,
        protocol: r.get(17)?,
    })
}

pub struct NewGrab<'a> {
    pub wanted_id: i64,
    pub guid: &'a str,
    pub title: &'a str,
    pub indexer: &'a str,
    pub size: u64,
    pub score: i32,
    /// The download client's id for it: SABnzbd's job id, or a torrent's info hash.
    pub sab_id: &'a str,
    pub protocol: &'a str,
}

pub fn add_grab(c: &Connection, g: &NewGrab) -> Result<i64, Error> {
    let t = now() as i64;
    c.execute(
        "INSERT INTO grabs(wanted_id, guid, title, indexer, size, score, sab_id, status, added, updated, protocol) VALUES (?1,?2,?3,?4,?5,?6,?7,'queued',?8,?8,?9)",
        params![g.wanted_id, g.guid, g.title, g.indexer, g.size as i64, g.score, g.sab_id, t, g.protocol],
    )?;
    set_wanted_status(c, g.wanted_id, "downloading")?;
    Ok(c.last_insert_rowid())
}

pub fn get_grab(c: &Connection, id: i64) -> Result<Grab, Error> {
    c.query_row(
        &format!("SELECT {GCOLS} FROM grabs WHERE id = ?1"),
        [id],
        gmap,
    )
    .optional()?
    .ok_or_else(|| Error::not_found(format!("No download #{id}")))
}

pub fn list_grabs(c: &Connection, limit: i64) -> Result<Vec<Grab>, Error> {
    let mut s = c.prepare(&format!(
        "SELECT {GCOLS} FROM grabs ORDER BY id DESC LIMIT ?1"
    ))?;
    Ok(s.query_map([limit], gmap)?.collect::<Result<Vec<_>, _>>()?)
}

/// Downloads still going: being fetched, or finished and waiting for their import.
pub fn open_grabs(c: &Connection) -> Result<Vec<Grab>, Error> {
    let mut s = c.prepare(&format!("SELECT {GCOLS} FROM grabs WHERE status IN ('queued','downloading','completed','unpacking','importing') ORDER BY id"))?;
    Ok(s.query_map([], gmap)?.collect::<Result<Vec<_>, _>>()?)
}

/// Torrent downloads that were marked failed because qBittorrent said "error", in the last few
/// days: if the torrent turns out to be finished after all, they are picked up again.
pub fn recent_failed_torrents(c: &Connection) -> Result<Vec<Grab>, Error> {
    let mut s = c.prepare(&format!(
        "SELECT {GCOLS} FROM grabs WHERE protocol = 'torrent' AND status = 'failed' AND error LIKE 'qBittorrent reports%' AND updated > ?1 ORDER BY id"
    ))?;
    Ok(s.query_map([now() as i64 - 3 * 86_400], gmap)?
        .collect::<Result<Vec<_>, _>>()?)
}

/// Take a download's entries off the blocklist (it was blocked by mistake).
pub fn unblock_download(c: &Connection, guid: &str, title: &str) -> Result<(), Error> {
    c.execute(
        "DELETE FROM blocklist WHERE guid = ?1 OR lower(title) = lower(?2)",
        params![guid, title],
    )?;
    Ok(())
}

pub fn has_open_grab(c: &Connection, wanted_id: i64) -> Result<bool, Error> {
    Ok(c.query_row(
        "SELECT count(*) FROM grabs WHERE wanted_id = ?1 AND status IN ('queued','downloading','completed','unpacking','importing')",
        [wanted_id],
        |r| r.get::<_, i64>(0),
    )? > 0)
}

pub fn update_grab(
    c: &Connection,
    id: i64,
    status: &str,
    progress: Option<f64>,
    path: Option<&str>,
    error: Option<&str>,
    import_job: Option<i64>,
) -> Result<(), Error> {
    c.execute(
        "UPDATE grabs SET status = ?1, progress = COALESCE(?2, progress), path = COALESCE(?3, path), error = ?4, import_job = COALESCE(?5, import_job), updated = ?6 WHERE id = ?7",
        params![status, progress, path, error, import_job, now() as i64, id],
    )?;
    Ok(())
}

pub fn set_title_ids(c: &Connection, id: i64, ids: &str) -> Result<(), Error> {
    c.execute(
        "UPDATE grabs SET title_ids = ?1 WHERE id = ?2",
        params![ids, id],
    )?;
    Ok(())
}

pub fn set_extra_job(c: &Connection, id: i64, job: i64) -> Result<(), Error> {
    c.execute(
        "UPDATE grabs SET extra_job = ?1 WHERE id = ?2",
        params![job, id],
    )?;
    Ok(())
}

/// The ids of games in a library with these title IDs (what an import just put there).
pub fn items_with_titles(
    c: &Connection,
    library_id: i64,
    title_ids: &[String],
) -> Result<Vec<i64>, Error> {
    let mut out = Vec::new();
    let mut s = c.prepare(
        "SELECT id FROM items WHERE library_id = ?1 AND upper(title_id) = upper(?2) AND available = 1 AND kind IN ('iso', 'god')
           AND (content_type IS NULL OR content_type IN ('00007000', '00005000', '000D0000', '00004000'))",
    )?;
    for t in title_ids {
        for r in s.query_map(params![library_id, t], |r| r.get::<_, i64>(0))? {
            out.push(r?);
        }
    }
    Ok(out)
}

pub fn delete_grab(c: &Connection, id: i64) -> Result<(), Error> {
    get_grab(c, id)?;
    c.execute("DELETE FROM grabs WHERE id = ?1", [id])?;
    Ok(())
}

// ── Blocklist ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct Blocked {
    pub id: i64,
    pub wanted_id: Option<i64>,
    pub title: String,
    pub indexer: Option<String>,
    pub reason: Option<String>,
    pub added: i64,
}

pub fn block(
    c: &Connection,
    wanted_id: Option<i64>,
    guid: &str,
    title: &str,
    indexer: &str,
    reason: &str,
) -> Result<(), Error> {
    c.execute(
        "INSERT INTO blocklist(wanted_id, guid, title, indexer, reason, added) VALUES (?1,?2,?3,?4,?5,?6)",
        params![wanted_id, guid, title, indexer, reason, now() as i64],
    )?;
    Ok(())
}

pub fn blocklist(c: &Connection) -> Result<Vec<Blocked>, Error> {
    let mut s = c.prepare(
        "SELECT id, wanted_id, title, indexer, reason, added FROM blocklist ORDER BY id DESC",
    )?;
    Ok(s.query_map([], |r| {
        Ok(Blocked {
            id: r.get(0)?,
            wanted_id: r.get(1)?,
            title: r.get(2)?,
            indexer: r.get(3)?,
            reason: r.get(4)?,
            added: r.get(5)?,
        })
    })?
    .collect::<Result<Vec<_>, _>>()?)
}

/// Blocked release names and guids, for filtering search results.
pub fn blocked_keys(c: &Connection) -> Result<HashSet<String>, Error> {
    let mut s = c.prepare("SELECT guid, title FROM blocklist")?;
    let mut out = HashSet::new();
    for r in s.query_map([], |r| {
        Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?))
    })? {
        let (g, t) = r?;
        out.extend(g);
        out.insert(t.to_lowercase());
    }
    Ok(out)
}

pub fn unblock(c: &Connection, id: i64) -> Result<(), Error> {
    c.execute("DELETE FROM blocklist WHERE id = ?1", [id])?;
    Ok(())
}

// ── What the libraries have ──────────────────────────────────────────────────

/// IGDB ids and title IDs of the games that are in a library now.
pub fn owned(c: &Connection) -> Result<(HashSet<i64>, HashSet<String>), Error> {
    let mut tids = HashSet::new();
    let mut s = c.prepare(
        "SELECT DISTINCT upper(title_id) FROM items WHERE title_id IS NOT NULL AND available = 1 AND kind IN ('iso', 'god')
           AND (content_type IS NULL OR content_type IN ('00007000', '00005000', '000D0000', '00004000'))",
    )?;
    for r in s.query_map([], |r| r.get::<_, String>(0))? {
        tids.insert(r?);
    }
    let mut igdb = HashSet::new();
    let mut s = c.prepare("SELECT title_id, igdb_id FROM game_meta WHERE igdb_id IS NOT NULL")?;
    for r in s.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (t, i) = r?;
        if tids.contains(&t.to_uppercase()) {
            igdb.insert(i);
        }
    }
    Ok((igdb, tids))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    #[test]
    fn wanted_games_grabs_and_the_blocklist_round_trip() {
        let d = std::env::temp_dir().join(format!("rustybox_grabdb_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let db = Db::open(&d.join("t.db")).unwrap();
        db.with(|c| {
            let id = add_wanted(
                c,
                &NewWanted {
                    igdb_id: Some(42),
                    name: "Halo 3",
                    release: Some(1),
                    summary: None,
                    cover: Some("c.jpg"),
                    title_id: Some("4D5307E6"),
                },
            )?;
            assert!(matches!(
                add_wanted(
                    c,
                    &NewWanted {
                        igdb_id: Some(42),
                        name: "Halo 3 again",
                        release: None,
                        summary: None,
                        cover: None,
                        title_id: None
                    }
                ),
                Err(Error::Conflict(_))
            ));
            assert!(!has_open_grab(c, id)?);
            let g = add_grab(
                c,
                &NewGrab {
                    wanted_id: id,
                    guid: "g1",
                    title: "Halo.3.XBOX360-X",
                    indexer: "Geek",
                    size: 7 << 30,
                    score: 110,
                    sab_id: "SABnzbd_nzo_1",
                    protocol: "usenet",
                },
            )?;
            assert_eq!(get_wanted(c, id)?.status, "downloading");
            assert!(has_open_grab(c, id)? && open_grabs(c)?.len() == 1);
            update_grab(c, g, "completed", Some(100.0), Some("/data/x"), None, None)?;
            assert_eq!(get_grab(c, g)?.path.as_deref(), Some("/data/x"));
            update_grab(c, g, "failed", None, None, Some("missing articles"), None)?;
            assert!(!has_open_grab(c, id)?);
            assert_eq!(
                get_grab(c, g)?.path.as_deref(),
                Some("/data/x"),
                "an update without a path keeps the old one"
            );
            block(
                c,
                Some(id),
                "g1",
                "Halo.3.XBOX360-X",
                "Geek",
                "missing articles",
            )?;
            let keys = blocked_keys(c)?;
            assert!(keys.contains("g1") && keys.contains("halo.3.xbox360-x"));
            unblock(c, blocklist(c)?[0].id)?;
            assert!(blocked_keys(c)?.is_empty());
            set_monitored(c, id, false, Some("later"))?;
            assert!(!get_wanted(c, id)?.monitored);
            delete_wanted(c, id)?;
            assert!(list_grabs(c, 10)?.is_empty());
            Ok(())
        })
        .unwrap();
        let _ = std::fs::remove_dir_all(&d);
    }
}
