//! The SQLite database: library configuration and the file index.
//!
//! One connection behind a mutex, used from blocking tasks (`Db::run`). WAL mode keeps readers
//! quick while a scan writes, and a busy timeout covers the odd overlap.

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use rusqlite::Connection;

use crate::error::Error;

/// Each entry upgrades the schema by one version. Never edit one that has shipped; add a new one.
const MIGRATIONS: &[&str] = &[
    // 1: libraries, their paths, and the index of what's in them
    r#"
    CREATE TABLE libraries (
        id      INTEGER PRIMARY KEY,
        name    TEXT NOT NULL UNIQUE COLLATE NOCASE,
        kind    TEXT NOT NULL,
        created INTEGER NOT NULL
    );
    CREATE TABLE library_paths (
        id         INTEGER PRIMARY KEY,
        library_id INTEGER NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
        path       TEXT NOT NULL,
        label      TEXT NOT NULL DEFAULT '',
        writable   INTEGER NOT NULL DEFAULT 0,
        priority   INTEGER NOT NULL DEFAULT 0,
        last_scan  INTEGER,
        UNIQUE (library_id, path)
    );
    CREATE TABLE items (
        id         INTEGER PRIMARY KEY,
        library_id INTEGER NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
        path_id    INTEGER NOT NULL REFERENCES library_paths(id) ON DELETE CASCADE,
        relpath    TEXT NOT NULL,
        name       TEXT NOT NULL,
        kind       TEXT NOT NULL,
        size       INTEGER NOT NULL,
        mtime      INTEGER NOT NULL,
        title_id   TEXT,
        available  INTEGER NOT NULL DEFAULT 1,
        seen       INTEGER NOT NULL DEFAULT 0,
        UNIQUE (path_id, relpath)
    );
    CREATE INDEX items_library ON items(library_id, name COLLATE NOCASE);
    CREATE INDEX items_title ON items(title_id);
    "#,
    // 2: what each game file says about itself (title ID, media ID, disc, platform, name)
    r#"
    ALTER TABLE items ADD COLUMN media_id   TEXT;
    ALTER TABLE items ADD COLUMN disc       INTEGER;
    ALTER TABLE items ADD COLUMN discs      INTEGER;
    ALTER TABLE items ADD COLUMN platform   TEXT;
    ALTER TABLE items ADD COLUMN game_name  TEXT;
    ALTER TABLE items ADD COLUMN meta       TEXT;
    ALTER TABLE items ADD COLUMN meta_error TEXT;
    "#,
    // 3: game information from IGDB, kept so each game is only looked up once
    r#"
    CREATE TABLE game_meta (
        title_id   TEXT PRIMARY KEY,
        status     TEXT NOT NULL,
        igdb_id    INTEGER,
        name       TEXT,
        summary    TEXT,
        release    INTEGER,
        genres     TEXT,
        developers TEXT,
        publishers TEXT,
        rating     REAL,
        url        TEXT,
        cover      TEXT,
        fetched_at INTEGER NOT NULL
    );
    "#,
    // 4: what kind of content an item is (game, add-on, update) and whether its files look complete
    r#"
    ALTER TABLE items ADD COLUMN content_type TEXT;
    ALTER TABLE items ADD COLUMN health TEXT;
    "#,
    // 5: a library folder can live on another machine, behind a drive agent
    r#"
    ALTER TABLE library_paths ADD COLUMN remote_url TEXT;
    ALTER TABLE library_paths ADD COLUMN remote_token TEXT;
    "#,
    // 6: a remote folder can be a subfolder of the shared drive (for example just `Games`)
    r#"
    ALTER TABLE library_paths ADD COLUMN remote_subdir TEXT;
    "#,
    // 7: what a library folder holds: games (the default), add-ons (DLC) or title updates
    r#"
    ALTER TABLE library_paths ADD COLUMN role TEXT NOT NULL DEFAULT '';
    "#,
    // 8: games to find on Usenet (from IGDB), what was grabbed for them, and what not to grab again
    r#"
    CREATE TABLE wanted (
        id INTEGER PRIMARY KEY,
        igdb_id INTEGER UNIQUE,
        name TEXT NOT NULL,
        release INTEGER,
        summary TEXT,
        cover TEXT,
        title_id TEXT,
        monitored INTEGER NOT NULL DEFAULT 1,
        status TEXT NOT NULL DEFAULT 'wanted',
        added INTEGER NOT NULL,
        last_search INTEGER,
        last_result TEXT,
        note TEXT
    );
    CREATE TABLE grabs (
        id INTEGER PRIMARY KEY,
        wanted_id INTEGER NOT NULL,
        guid TEXT NOT NULL,
        title TEXT NOT NULL,
        indexer TEXT NOT NULL,
        size INTEGER NOT NULL DEFAULT 0,
        score INTEGER NOT NULL DEFAULT 0,
        sab_id TEXT,
        status TEXT NOT NULL,
        progress REAL NOT NULL DEFAULT 0,
        added INTEGER NOT NULL,
        updated INTEGER NOT NULL,
        path TEXT,
        error TEXT,
        import_job INTEGER
    );
    CREATE INDEX grabs_wanted ON grabs(wanted_id);
    CREATE TABLE blocklist (
        id INTEGER PRIMARY KEY,
        wanted_id INTEGER,
        guid TEXT,
        title TEXT NOT NULL,
        indexer TEXT,
        reason TEXT,
        added INTEGER NOT NULL
    );
    "#,
    // 9: which games a download held, and the job that sent them on to the console
    r#"
    ALTER TABLE grabs ADD COLUMN title_ids TEXT;
    ALTER TABLE grabs ADD COLUMN extra_job INTEGER;
    "#,
    // 10: a record of jobs, so their history survives a restart and a job number is never reused
    r#"
    CREATE TABLE job_history (
        id INTEGER PRIMARY KEY,
        kind TEXT NOT NULL,
        title TEXT NOT NULL,
        resources TEXT NOT NULL DEFAULT '[]',
        status TEXT NOT NULL,
        pct REAL NOT NULL DEFAULT 0,
        step TEXT NOT NULL DEFAULT '',
        error TEXT,
        started INTEGER NOT NULL,
        finished INTEGER,
        events TEXT NOT NULL DEFAULT '[]'
    );
    "#,
    // 11: downloads can come from a torrent client as well as SABnzbd. `sab_id` holds the
    // client's own id for the download (SABnzbd's nzo id, or the torrent's info hash).
    r#"
    ALTER TABLE grabs ADD COLUMN protocol TEXT NOT NULL DEFAULT 'usenet';
    "#,
    // 12: Xbox 360 Arcade games packaged like original Xbox ones were labelled "Original Xbox";
    // forget what was read for those so the next scan reads them again.
    r#"
    UPDATE items SET meta = NULL WHERE platform = 'xbox';
    "#,
];

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

fn migrate(conn: &mut Connection) -> Result<(), Error> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", i as i64 + 1)?;
        tx.commit()?;
    }
    Ok(())
}

impl Db {
    pub fn open(path: &Path) -> Result<Db, Error> {
        // The database holds drive agent tokens, so only its owner may read it: new files are made
        // that way, and an existing one (from before this) is tightened.
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            if !path.exists() {
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(path)?;
            }
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        let mut conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::finish(&mut conn)?;
        Ok(Db {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn open_memory() -> Result<Db, Error> {
        let mut conn = Connection::open_in_memory()?;
        Self::finish(&mut conn)?;
        Ok(Db {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn finish(conn: &mut Connection) -> Result<(), Error> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        migrate(conn)
    }

    /// Run `f` on a blocking thread with the connection.
    pub async fn run<T, F>(&self, f: F) -> Result<T, Error>
    where
        F: FnOnce(&mut Connection) -> Result<T, Error> + Send + 'static,
        T: Send + 'static,
    {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut g = conn.lock().unwrap_or_else(|e| e.into_inner());
            f(&mut g)
        })
        .await
        .map_err(|e| Error::backend(e.to_string()))?
    }

    /// Same as `run`, for code that is already on a blocking thread.
    pub fn with<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T, Error>) -> Result<T, Error> {
        let mut g = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut g)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_database_file_is_private_even_if_it_was_not_before() {
        use std::os::unix::fs::PermissionsExt;
        let p = std::env::temp_dir().join(format!("rustybox_dbperm_{}.db", std::process::id()));
        let _ = std::fs::remove_file(&p);
        std::fs::write(&p, b"").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        Db::open(&p).unwrap();
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(p.with_extension("db-wal"));
        let _ = std::fs::remove_file(p.with_extension("db-shm"));
    }

    #[test]
    fn migrations_apply_once_and_set_the_version() {
        let db = Db::open_memory().unwrap();
        let v: i64 = db
            .with(|c| Ok(c.query_row("PRAGMA user_version", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(v as usize, MIGRATIONS.len());
        // Running them again on an up-to-date database changes nothing.
        db.with(migrate).unwrap();
    }

    #[test]
    fn deleting_a_library_removes_its_paths_and_items() {
        let db = Db::open_memory().unwrap();
        db.with(|c| {
            c.execute("INSERT INTO libraries(id,name,kind,created) VALUES (1,'a','iso',0)", [])?;
            c.execute("INSERT INTO library_paths(id,library_id,path) VALUES (1,1,'/x')", [])?;
            c.execute("INSERT INTO items(library_id,path_id,relpath,name,kind,size,mtime) VALUES (1,1,'a.iso','a','iso',1,0)", [])?;
            c.execute("DELETE FROM libraries WHERE id=1", [])?;
            let n: i64 = c.query_row("SELECT count(*) FROM items", [], |r| r.get(0))?;
            assert_eq!(n, 0);
            Ok(())
        })
        .unwrap();
    }
}
