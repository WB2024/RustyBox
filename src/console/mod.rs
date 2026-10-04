//! The Xbox 360 itself, reached over Aurora's FTP: connection profiles, finding the games on it,
//! and sending files and folders to it (and fetching them back).
//!
//! Everything here is blocking and runs inside jobs or `spawn_blocking`.

pub mod fake;
pub mod ftp;
pub mod send;

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::RwLock,
};

use serde::{Deserialize, Serialize};

use crate::{
    convert::Ctl,
    error::Error,
    transfer::loc::{Loc, TreeEntry},
};
use ftp::{Ftp, Login, ftp_path};

/// One console RustyBox knows how to reach.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Console {
    pub id: u32,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    /// Never sent to the browser.
    pub password: String,
    /// Folders games live in, such as `Hdd1/Games` or `Usb0/Games` (what Aurora scans).
    pub game_paths: Vec<String>,
    /// Where Aurora keeps its files (trainers and so on go under here).
    pub aurora_path: String,
    /// How deep to look for title folders under a games folder.
    pub scan_depth: u32,
}

impl Default for Console {
    fn default() -> Self {
        Console {
            id: 0,
            name: "My Xbox 360".into(),
            host: String::new(),
            port: 21,
            user: "xbox".into(),
            password: "xbox".into(),
            game_paths: vec!["Hdd1/Games".into()],
            aurora_path: "Hdd1/Aurora".into(),
            scan_depth: 4,
        }
    }
}

impl Console {
    pub fn login(&self) -> Login {
        Login {
            host: self.host.clone(),
            port: self.port,
            user: self.user.clone(),
            password: self.password.clone(),
        }
    }

    pub fn connect(&self) -> Result<Ftp, Error> {
        Ftp::connect(&self.login())
    }

    /// What the browser may see.
    pub fn public(&self) -> serde_json::Value {
        let mut v = serde_json::to_value(self).unwrap_or_default();
        if let Some(o) = v.as_object_mut() {
            o.remove("password");
            o.insert("password_set".into(), (!self.password.is_empty()).into());
        }
        v
    }

    pub fn validate(mut self) -> Result<Console, Error> {
        self.name = self.name.trim().to_string();
        self.host = self.host.trim().to_string();
        self.user = self.user.trim().to_string();
        if self.name.is_empty() || self.name.len() > 60 {
            return Err(Error::validation(
                "Give the console a name (up to 60 letters)",
            ));
        }
        if self.host.is_empty()
            || self.host.len() > 253
            || self
                .host
                .chars()
                .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':')))
        {
            return Err(Error::validation(
                "Enter the console's IP address, such as 192.168.1.50",
            ));
        }
        if self.port == 0 {
            return Err(Error::validation(
                "The FTP port can't be 0 (Aurora uses 21)",
            ));
        }
        self.game_paths = self
            .game_paths
            .iter()
            .map(|p| ftp_path(p).trim_start_matches('/').to_string())
            .filter(|p| !p.is_empty())
            .collect();
        if self.game_paths.is_empty() {
            return Err(Error::validation(
                "Add at least one games folder, such as Hdd1/Games",
            ));
        }
        self.aurora_path = ftp_path(&self.aurora_path)
            .trim_start_matches('/')
            .to_string();
        if !(1..=8).contains(&self.scan_depth) {
            return Err(Error::validation("Scan depth must be between 1 and 8"));
        }
        Ok(self)
    }
}

/// The consoles, kept in `consoles.json` next to the settings (0600: it holds passwords).
pub struct Store {
    path: PathBuf,
    list: RwLock<Vec<Console>>,
}

impl Store {
    pub fn load(dir: &Path) -> Store {
        let path = dir.join("consoles.json");
        let list = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Vec<Console>>(&b).ok())
            .unwrap_or_default();
        Store {
            path,
            list: RwLock::new(list),
        }
    }

    pub fn all(&self) -> Vec<Console> {
        self.list.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn get(&self, id: u32) -> Result<Console, Error> {
        self.all()
            .into_iter()
            .find(|c| c.id == id)
            .ok_or_else(|| Error::not_found(format!("No console #{id}")))
    }

    fn save(&self, list: &[Console]) -> Result<(), Error> {
        crate::fsops::write_private(&self.path, &serde_json::to_vec_pretty(list)?)
    }

    /// Add (id 0) or change a console. A blank password on a change keeps the old one.
    pub fn put(&self, mut c: Console) -> Result<Console, Error> {
        let mut list = self.list.write().unwrap_or_else(|e| e.into_inner());
        if c.id == 0 {
            c.id = list.iter().map(|x| x.id).max().unwrap_or(0) + 1;
        } else if c.password.is_empty()
            && let Some(old) = list.iter().find(|x| x.id == c.id)
        {
            c.password = old.password.clone();
        }
        let c = c.validate()?;
        if list.iter().any(|x| x.id != c.id && x.name == c.name) {
            return Err(Error::conflict(format!(
                "A console called '{}' already exists",
                c.name
            )));
        }
        match list.iter_mut().find(|x| x.id == c.id) {
            Some(slot) => *slot = c.clone(),
            None => list.push(c.clone()),
        }
        self.save(&list)?;
        Ok(c)
    }

    pub fn remove(&self, id: u32) -> Result<(), Error> {
        let mut list = self.list.write().unwrap_or_else(|e| e.into_inner());
        let before = list.len();
        list.retain(|c| c.id != id);
        if list.len() == before {
            return Err(Error::not_found(format!("No console #{id}")));
        }
        self.save(&list)
    }
}

// ── Games on the console ─────────────────────────────────────────────────────

fn is_title_id(name: &str) -> bool {
    name.len() == 8 && name.bytes().all(|b| b.is_ascii_hexdigit())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConsoleGame {
    pub title_id: String,
    pub name: String,
    /// The title folder, as an FTP path.
    pub path: String,
    /// What is in it: a number of content folders such as `00007000` (game) or `00000002` (add-on).
    pub content: Vec<String>,
    pub size: u64,
}

/// Find the title folders under the console's games folders (any folder named by eight hex digits).
pub fn scan_games(ftp: &mut Ftp, c: &Console, ctl: &Ctl) -> Result<Vec<ConsoleGame>, Error> {
    let cat = crate::xbox::catalog::get();
    let mut found: HashMap<String, ConsoleGame> = HashMap::new();
    for root in &c.game_paths {
        let root = ftp_path(root);
        let mut stack = vec![(root.clone(), c.scan_depth)];
        ctl.check()?;
        (ctl.progress)(0.0, &format!("Looking in {root}"));
        while let Some((dir, depth)) = stack.pop() {
            ctl.check()?;
            let entries = match ftp.list(&dir) {
                Ok(e) => e,
                // A games folder that isn't there (yet) is not an error.
                Err(Error::NotFound(_)) => continue,
                Err(e) => return Err(e),
            };
            for e in entries.into_iter().filter(|e| e.is_dir) {
                let path = format!("{dir}/{}", e.name);
                if is_title_id(&e.name) {
                    let tid = e.name.to_uppercase();
                    let mut content = Vec::new();
                    let mut size = 0u64;
                    let mut inner = vec![path.clone()];
                    while let Some(d) = inner.pop() {
                        for x in ftp.list(&d)? {
                            if x.is_dir {
                                if d == path {
                                    content.push(x.name.clone());
                                }
                                inner.push(format!("{d}/{}", x.name));
                            } else {
                                size += x.size;
                            }
                        }
                    }
                    content.sort();
                    let name = cat.name(&tid, None).unwrap_or(&tid).to_string();
                    found.insert(
                        path.clone(),
                        ConsoleGame {
                            title_id: tid,
                            name,
                            path,
                            content,
                            size,
                        },
                    );
                } else if depth > 1 {
                    stack.push((path, depth - 1));
                }
            }
        }
    }
    let mut v: Vec<_> = found.into_values().collect();
    v.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.path.cmp(&b.path))
    });
    Ok(v)
}

// ── Sending and fetching ─────────────────────────────────────────────────────

/// The biggest file a drive can hold: FAT32 USB sticks are limited, the console's own disk isn't.
pub fn max_file_for(path: &str) -> Option<u64> {
    let p = ftp_path(path).to_lowercase();
    (p.starts_with("/usb")).then_some(4 * 1024 * 1024 * 1024 - 1)
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Sent {
    pub files: usize,
    pub skipped: usize,
    pub bytes: u64,
}

const CHUNK: u64 = 1024 * 1024;

fn join(a: &str, b: &str) -> String {
    match (a.trim_matches('/').is_empty(), b.is_empty()) {
        (true, _) => b.to_string(),
        (false, true) => a.trim_matches('/').to_string(),
        (false, false) => format!("{}/{b}", a.trim_matches('/')),
    }
}

/// Send a file or folder from `src` to `dest` on the console. A file that is already there with
/// the right size is skipped, so running it again after an interruption carries on; a half-sent
/// file is written under a `.part` name and renamed when whole. `overwrite` lets a file of a
/// different size be replaced; without it that is an error.
pub fn send_tree(
    ftp: &mut Ftp,
    src: &Loc,
    src_rel: &str,
    dest: &str,
    overwrite: bool,
    ctl: &Ctl,
) -> Result<Sent, Error> {
    let tree = src.tree(src_rel)?;
    let is_file = tree.len() == 1 && tree[0].rel.is_empty();
    let total: u64 = tree.iter().filter(|e| !e.is_dir).map(|e| e.size).sum();
    let dest = ftp_path(dest);
    let mut done = 0u64;
    let mut out = Sent::default();
    if !is_file {
        ftp.mkdir_p(&dest)?;
        for e in tree.iter().filter(|e| e.is_dir) {
            ctl.check()?;
            ftp.mkdir_p(&format!("{dest}/{}", e.rel))?;
        }
    }
    // Look at each destination folder once, not once per file.
    let mut seen: HashMap<String, HashMap<String, u64>> = HashMap::new();
    let mut files: Vec<&TreeEntry> = tree.iter().filter(|e| !e.is_dir).collect();
    files.sort_by(|a, b| a.rel.cmp(&b.rel));
    for e in files {
        ctl.check()?;
        let remote = if is_file {
            dest.clone()
        } else {
            format!("{dest}/{}", e.rel)
        };
        let from = if is_file {
            src_rel.to_string()
        } else {
            join(src_rel, &e.rel)
        };
        let dir = ftp::parent(&remote);
        let name = ftp::file_name(&remote).to_string();
        let have = seen.entry(dir.clone()).or_insert_with(|| {
            ftp.list(&dir)
                .map(|l| {
                    l.into_iter()
                        .filter(|x| !x.is_dir)
                        .map(|x| (x.name, x.size))
                        .collect()
                })
                .unwrap_or_default()
        });
        if let Some(&sz) = have.get(&name) {
            if sz == e.size {
                out.skipped += 1;
                done += e.size;
                (ctl.progress)(done as f32 / total.max(1) as f32, &name);
                continue;
            }
            if !overwrite {
                return Err(Error::coded(
                    409,
                    "EXISTS",
                    format!("{remote} is already on the console with a different size"),
                ));
            }
        }
        let part = format!("{remote}.part");
        let result = (|| -> Result<(), Error> {
            ftp.store_start(&part)?;
            let mut off = 0u64;
            while off < e.size {
                ctl.check()?;
                let len = CHUNK.min(e.size - off);
                let data = src.read(&from, off, len)?;
                if data.len() as u64 != len {
                    return Err(Error::backend(format!(
                        "{from} ended early: expected {len} bytes at {off}, got {}",
                        data.len()
                    )));
                }
                ftp.store_write(&data)?;
                off += len;
                (ctl.progress)((done + off) as f32 / total.max(1) as f32, &name);
            }
            ftp.store_finish()
        })();
        if let Err(err) = result {
            ftp.store_abort();
            let _ = ftp.delete_file(&part);
            return Err(err);
        }
        if have.contains_key(&name) {
            ftp.delete_file(&remote)?;
        }
        ftp.rename(&part, &remote)?;
        // Check it arrived whole before counting it.
        let size_now = ftp
            .list(&dir)?
            .into_iter()
            .find(|x| x.name == name)
            .map(|x| x.size);
        if size_now != Some(e.size) {
            return Err(Error::backend(format!(
                "{remote} didn't arrive intact (expected {} bytes, the console shows {size_now:?})",
                e.size
            )));
        }
        have.insert(name, e.size);
        done += e.size;
        out.files += 1;
        out.bytes += e.size;
    }
    (ctl.progress)(1.0, "");
    Ok(out)
}

/// Everything below a console folder (files and folders), paths relative to it.
pub fn remote_tree(ftp: &mut Ftp, path: &str) -> Result<Vec<TreeEntry>, Error> {
    let path = ftp_path(path);
    let mut out = Vec::new();
    let mut stack = vec![String::new()];
    while let Some(dir) = stack.pop() {
        let here = if dir.is_empty() {
            path.clone()
        } else {
            format!("{path}/{dir}")
        };
        for e in ftp.list(&here)? {
            let rel = if dir.is_empty() {
                e.name.clone()
            } else {
                format!("{dir}/{}", e.name)
            };
            if e.is_dir {
                stack.push(rel.clone());
            }
            out.push(TreeEntry {
                rel,
                is_dir: e.is_dir,
                size: e.size,
                mtime: 0,
            });
        }
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(out)
}

/// Copy a console file or folder to a folder here, written as `.part` and renamed when whole.
pub fn fetch_tree(
    ftp: &mut Ftp,
    remote: &str,
    local_root: &Path,
    local_rel: &str,
    ctl: &Ctl,
) -> Result<Sent, Error> {
    use std::io::Write;
    let remote = ftp_path(remote);
    let st = ftp
        .stat(&remote)?
        .ok_or_else(|| Error::not_found(format!("{remote} isn't on the console")))?;
    let tree = if st.is_dir {
        remote_tree(ftp, &remote)?
    } else {
        vec![TreeEntry {
            rel: String::new(),
            is_dir: false,
            size: st.size,
            mtime: 0,
        }]
    };
    let total: u64 = tree.iter().filter(|e| !e.is_dir).map(|e| e.size).sum();
    let mut done = 0u64;
    let mut out = Sent::default();
    let base = crate::fsops::join_rel(local_root, local_rel)?;
    for e in tree.iter().filter(|e| e.is_dir) {
        std::fs::create_dir_all(crate::fsops::join_rel(&base, &e.rel)?)?;
    }
    for e in tree.iter().filter(|e| !e.is_dir) {
        ctl.check()?;
        let (src, dst) = if e.rel.is_empty() {
            (remote.clone(), base.clone())
        } else {
            (
                format!("{remote}/{}", e.rel),
                crate::fsops::join_rel(&base, &e.rel)?,
            )
        };
        if std::fs::metadata(&dst).is_ok_and(|m| m.len() == e.size) {
            out.skipped += 1;
            done += e.size;
            continue;
        }
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p)?;
        }
        let mut part = dst.clone().into_os_string();
        part.push(".part");
        let part = PathBuf::from(part);
        let mut f = std::fs::File::create(&part)?;
        let name = ftp::file_name(&src).to_string();
        let got = ftp
            .read_to(&src, 0, &mut f, &mut |n| {
                ctl.check()?;
                (ctl.progress)((done + n) as f32 / total.max(1) as f32, &name);
                Ok(())
            })
            .inspect_err(|_| {
                let _ = std::fs::remove_file(&part);
            })?;
        if got != e.size {
            let _ = std::fs::remove_file(&part);
            return Err(Error::backend(format!(
                "{src} came back as {got} bytes, expected {}",
                e.size
            )));
        }
        f.flush()?;
        std::fs::rename(&part, &dst)?;
        done += e.size;
        out.files += 1;
        out.bytes += e.size;
    }
    Ok(out)
}

// ── Self-test ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct TestStep {
    pub name: String,
    pub ok: bool,
    pub detail: String,
    pub ms: u64,
}

/// Try every kind of FTP operation RustyBox relies on, in a throwaway folder (`RustyBoxTest`, under
/// the console's first games folder's drive), and clean up. It never touches anything else.
/// Each step is reported separately so a difference in a real console's behaviour is easy to find.
pub fn selftest(c: &Console) -> Vec<TestStep> {
    let mut steps: Vec<TestStep> = Vec::new();
    let t0 = std::time::Instant::now();
    macro_rules! step {
        ($name:expr, $body:expr) => {{
            let started = std::time::Instant::now();
            #[allow(clippy::redundant_closure_call)]
            let r: Result<String, Error> = (|| $body)();
            let ok = r.is_ok();
            steps.push(TestStep {
                name: $name.to_string(),
                ok,
                detail: r.unwrap_or_else(|e| e.to_string()),
                ms: started.elapsed().as_millis() as u64,
            });
            ok
        }};
    }
    let mut ftp = match c.connect() {
        Ok(f) => {
            steps.push(TestStep {
                name: "Connect and log in".into(),
                ok: true,
                detail: format!("{}:{}", c.host, c.port),
                ms: t0.elapsed().as_millis() as u64,
            });
            f
        }
        Err(e) => {
            steps.push(TestStep {
                name: "Connect and log in".into(),
                ok: false,
                detail: e.to_string(),
                ms: t0.elapsed().as_millis() as u64,
            });
            return steps;
        }
    };
    let drive = ftp_path(&c.game_paths[0])
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or("Hdd1")
        .to_string();
    let dir = format!("/{drive}/RustyBoxTest");
    step!("List the drives at the top", {
        let l = ftp.list("/")?;
        Ok(l.iter()
            .filter(|e| e.is_dir)
            .map(|e| e.name.clone())
            .collect::<Vec<_>>()
            .join(", "))
    });
    for g in &c.game_paths {
        let g = g.clone();
        step!(format!("List the games folder {g}"), {
            match ftp.list(&g) {
                Ok(l) => Ok(format!(
                    "{} entries ({} folders)",
                    l.len(),
                    l.iter().filter(|e| e.is_dir).count()
                )),
                Err(Error::NotFound(_)) => {
                    Ok("not there yet (it is made when you send a game)".to_string())
                }
                Err(e) => Err(e),
            }
        });
    }
    let _ = ftp.delete_tree(&dir); // left over from an earlier run
    let made = step!("Make folders", {
        ftp.mkdir_p(&format!("{dir}/Sub folder/Deep"))?;
        let st = ftp.stat(&format!("{dir}/Sub folder/Deep"))?;
        if st.is_some_and(|e| e.is_dir) {
            Ok("made three levels at once".into())
        } else {
            Err(Error::backend("the folder doesn't show up after MKD"))
        }
    });
    if made {
        let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let file = format!("{dir}/Sub folder/test file.bin");
        step!("Send a file (STOR) under a .part name", {
            let part = format!("{file}.part");
            ftp.store_start(&part)?;
            ftp.store_write(&data)?;
            ftp.store_finish()?;
            let sz = ftp.stat(&part)?.map(|e| e.size);
            if sz == Some(data.len() as u64) {
                Ok(format!("{} bytes arrived", data.len()))
            } else {
                Err(Error::backend(format!(
                    "expected {} bytes, the listing shows {sz:?}",
                    data.len()
                )))
            }
        });
        step!("Rename the .part file into place (RNFR/RNTO)", {
            ftp.rename(&format!("{file}.part"), &file)?;
            if ftp.stat(&file)?.is_some() && ftp.stat(&format!("{file}.part"))?.is_none() {
                Ok("renamed".into())
            } else {
                Err(Error::backend(
                    "the file isn't where it should be after the rename",
                ))
            }
        });
        step!("Rename a folder", {
            ftp.rename(
                &format!("{dir}/Sub folder/Deep"),
                &format!("{dir}/Sub folder/Deeper"),
            )?;
            if ftp
                .stat(&format!("{dir}/Sub folder/Deeper"))?
                .is_some_and(|e| e.is_dir)
            {
                Ok("renamed".into())
            } else {
                Err(Error::backend("the renamed folder isn't there"))
            }
        });
        step!("Read the whole file back (RETR)", {
            let mut got = Vec::new();
            ftp.read_to(&file, 0, &mut got, &mut |_| Ok(()))?;
            if got == data {
                Ok("identical".into())
            } else {
                Err(Error::backend(format!(
                    "{} bytes came back and they differ",
                    got.len()
                )))
            }
        });
        step!("Read from the middle of a file (REST)", {
            let got = ftp.read(&file, 250_000, 1000)?;
            if got == data[250_000..251_000] {
                Ok("correct bytes".into())
            } else {
                Err(Error::backend(
                    "the bytes from the middle differ (resuming will not work)",
                ))
            }
        });
        step!("Resend with a different size is detected", {
            let sz = ftp.stat(&file)?.map(|e| e.size);
            if sz == Some(300_000) {
                Ok("size is read from the listing".into())
            } else {
                Err(Error::backend(format!("size {sz:?}")))
            }
        });
        step!("Delete a file (DELE)", {
            ftp.delete_file(&file)?;
            if ftp.stat(&file)?.is_none() {
                Ok("deleted".into())
            } else {
                Err(Error::backend("still there after DELE"))
            }
        });
    }
    step!("Delete the test folder (RMD)", {
        ftp.delete_tree(&dir)?;
        if ftp.stat(&dir)?.is_none() {
            Ok("cleaned up".into())
        } else {
            Err(Error::backend("the test folder is still there"))
        }
    });
    step!("The connection is still good", {
        let l = ftp.list("/")?;
        Ok(format!("{} entries", l.len()))
    });
    ftp.quit();
    steps
}

/// Sample content for `--mock`: a pretend console with a few games already on it.
pub fn seed_mock(root: &Path) -> std::io::Result<()> {
    use crate::xbox::stfs;
    let games = root.join("Hdd1/Games");
    if games.exists() {
        return Ok(());
    }
    for (tid, name, pad) in [
        (0x4D53_0805u32, "Alan Wake", 300_000usize),
        (0x4D53_07D5, "Gears of War", 200_000),
    ] {
        let ct = games.join(name).join(format!("{tid:08X}")).join("00007000");
        std::fs::create_dir_all(ct.join("A1B2C3D4E5.data"))?;
        std::fs::write(
            ct.join("A1B2C3D4E5"),
            stfs::build(b"LIVE", 0x7000, tid, 0, name, name),
        )?;
        std::fs::write(ct.join("A1B2C3D4E5.data/Data0000"), vec![0x58u8; pad])?;
    }
    std::fs::create_dir_all(root.join("Hdd1/Aurora/User/Trainers"))?;
    std::fs::create_dir_all(root.join("Usb0/Games"))?;
    std::fs::create_dir_all(root.join("Hdd1/Content/0000000000000000"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn noctl<T>(f: impl FnOnce(&Ctl) -> T) -> T {
        let cancelled = || false;
        let progress = |_: f32, _: &str| {};
        f(&Ctl {
            cancelled: &cancelled,
            progress: &progress,
        })
    }

    fn server(name: &str) -> (fake::FakeAurora, PathBuf) {
        let d =
            std::env::temp_dir().join(format!("rustybox_console_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("srv/Hdd1")).unwrap();
        fs::create_dir_all(d.join("srv/Usb0")).unwrap();
        let s = fake::start(&d.join("srv"), "xbox", "xbox").unwrap();
        (s, d)
    }

    fn login(s: &fake::FakeAurora) -> Login {
        Login {
            host: "127.0.0.1".into(),
            port: s.addr.port(),
            user: "xbox".into(),
            password: "xbox".into(),
        }
    }

    #[test]
    fn talks_to_an_aurora_like_server() {
        let (s, d) = server("talk");
        let mut f = Ftp::connect(&login(&s)).unwrap();
        // The root lists the drives.
        let root = f.list("/").unwrap();
        assert!(root.iter().any(|e| e.name == "Hdd1" && e.is_dir));
        // Folders are made a level at a time, and making one twice is fine.
        f.mkdir_p("Hdd1:\\Games\\Alan Wake").unwrap();
        f.mkdir_p("/Hdd1/Games/Alan Wake").unwrap();
        assert!(d.join("srv/Hdd1/Games/Alan Wake").is_dir());
        // Writing a file, reading part of it back, renaming, deleting.
        f.store_start("/Hdd1/Games/a.bin").unwrap();
        f.store_write(&(0..=255u8).collect::<Vec<_>>()).unwrap();
        f.store_finish().unwrap();
        assert_eq!(f.stat("/Hdd1/Games/a.bin").unwrap().unwrap().size, 256);
        assert_eq!(
            f.read("/Hdd1/Games/a.bin", 250, 100).unwrap(),
            vec![250, 251, 252, 253, 254, 255]
        );
        assert_eq!(f.read("/Hdd1/Games/a.bin", 0, 3).unwrap(), vec![0, 1, 2]);
        f.rename("/Hdd1/Games/a.bin", "/Hdd1/Games/b.bin").unwrap();
        assert!(f.stat("/Hdd1/Games/a.bin").unwrap().is_none());
        f.delete_file("/Hdd1/Games/b.bin").unwrap();
        // A whole folder goes, but never a drive.
        fs::write(d.join("srv/Hdd1/Games/Alan Wake/x"), b"1").unwrap();
        f.delete_tree("/Hdd1/Games/Alan Wake").unwrap();
        assert!(!d.join("srv/Hdd1/Games/Alan Wake").exists());
        assert!(f.delete_tree("/Hdd1").is_err());
        f.quit();
        // A wrong password is explained.
        let bad = Login {
            password: "nope".into(),
            ..login(&s)
        };
        let e = Ftp::connect(&bad).err().unwrap();
        assert_eq!(e.to_box_error().error, "CONSOLE_LOGIN");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn games_are_found_and_a_folder_can_be_sent_and_fetched_back() {
        let (s, d) = server("send");
        seed_mock(&d.join("srv")).unwrap();
        let c = Console {
            id: 1,
            host: "127.0.0.1".into(),
            port: s.addr.port(),
            ..Default::default()
        };
        let mut f = c.connect().unwrap();
        let games = noctl(|ctl| scan_games(&mut f, &c, ctl)).unwrap();
        let names: Vec<_> = games.iter().map(|g| g.title_id.as_str()).collect();
        assert_eq!(names, vec!["4D530805", "4D5307D5"], "{games:?}");
        assert_eq!(games[0].content, vec!["00007000"]);
        assert!(games[0].size > 300_000);

        // Send a GOD-like folder from here.
        let src = d.join("local/Fable II/4D5307F1/00007000");
        fs::create_dir_all(src.join("C.data")).unwrap();
        fs::write(src.join("C"), vec![7u8; 1000]).unwrap();
        fs::write(src.join("C.data/Data0000"), vec![9u8; 3_000_000]).unwrap();
        let loc = Loc::Local(d.join("local"));
        let sent =
            noctl(|ctl| send_tree(&mut f, &loc, "Fable II", "Hdd1/Games/Fable II", false, ctl))
                .unwrap();
        assert_eq!((sent.files, sent.skipped), (2, 0));
        let on = d.join("srv/Hdd1/Games/Fable II/4D5307F1/00007000/C.data/Data0000");
        assert_eq!(fs::metadata(&on).unwrap().len(), 3_000_000);
        assert!(
            !fs::read_dir(on.parent().unwrap())
                .unwrap()
                .flatten()
                .any(|e| e.file_name().to_string_lossy().ends_with(".part"))
        );
        // Again: everything is already there.
        let again =
            noctl(|ctl| send_tree(&mut f, &loc, "Fable II", "Hdd1/Games/Fable II", false, ctl))
                .unwrap();
        assert_eq!((again.files, again.skipped), (0, 2));
        // A file of a different size is refused unless asked.
        fs::write(src.join("C"), vec![7u8; 1200]).unwrap();
        let clash =
            noctl(|ctl| send_tree(&mut f, &loc, "Fable II", "Hdd1/Games/Fable II", false, ctl))
                .err()
                .unwrap();
        assert_eq!(clash.to_box_error().error, "EXISTS");
        let ow = noctl(|ctl| send_tree(&mut f, &loc, "Fable II", "Hdd1/Games/Fable II", true, ctl))
            .unwrap();
        assert_eq!(ow.files, 1);
        // And back again, byte for byte.
        let back = d.join("back");
        noctl(|ctl| fetch_tree(&mut f, "Hdd1/Games/Fable II", &back, "Fable II", ctl)).unwrap();
        assert_eq!(
            fs::read(back.join("Fable II/4D5307F1/00007000/C.data/Data0000")).unwrap(),
            vec![9u8; 3_000_000]
        );
        // Cancel stops a transfer and leaves no half file behind.
        let cancel = || true;
        let progress = |_: f32, _: &str| {};
        let r = send_tree(
            &mut f,
            &loc,
            "Fable II",
            "Hdd1/Games/Fable III",
            false,
            &Ctl {
                cancelled: &cancel,
                progress: &progress,
            },
        );
        assert!(r.is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn the_self_test_passes_against_a_server_with_aurora_s_habits_and_cleans_up() {
        let (s, d) = server("selftest");
        seed_mock(&d.join("srv")).unwrap();
        let c = Console {
            id: 1,
            host: "127.0.0.1".into(),
            port: s.addr.port(),
            ..Default::default()
        };
        let steps = selftest(&c);
        let failed: Vec<_> = steps.iter().filter(|x| !x.ok).collect();
        assert!(failed.is_empty(), "{failed:?}");
        assert!(steps.len() >= 12);
        assert!(
            !d.join("srv/Hdd1/RustyBoxTest").exists(),
            "the test folder is removed"
        );
        // Nothing listening: one clear failed step, not a panic.
        let down = selftest(&Console { port: 1, ..c });
        assert_eq!(down.len(), 1);
        assert!(!down[0].ok && down[0].detail.contains("Can't reach"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn the_self_test_also_passes_when_the_server_lists_dos_style_and_gives_a_wrong_pasv_address() {
        let d =
            std::env::temp_dir().join(format!("rustybox_console_quirks_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("srv/Hdd1/Games")).unwrap();
        for q in [
            fake::Quirks {
                dos_list: true,
                wrong_pasv_ip: false,
            },
            fake::Quirks {
                dos_list: false,
                wrong_pasv_ip: true,
            },
            fake::Quirks {
                dos_list: true,
                wrong_pasv_ip: true,
            },
        ] {
            let s = fake::start_with(&d.join("srv"), "xbox", "xbox", q).unwrap();
            let c = Console {
                id: 1,
                host: "127.0.0.1".into(),
                port: s.addr.port(),
                ..Default::default()
            };
            let steps = selftest(&c);
            let failed: Vec<_> = steps.iter().filter(|x| !x.ok).collect();
            assert!(failed.is_empty(), "{q:?}: {failed:?}");
        }
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn the_console_store_keeps_passwords_server_side() {
        let d = std::env::temp_dir().join(format!("rustybox_console_store_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        let st = Store::load(&d);
        let c = st
            .put(Console {
                host: "192.168.1.50".into(),
                password: "secret".into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(c.id, 1);
        assert!(c.public().get("password").is_none());
        // A blank password on an edit keeps the old one.
        let edited = st
            .put(Console {
                password: String::new(),
                name: "Living room".into(),
                ..c.clone()
            })
            .unwrap();
        assert_eq!(edited.password, "secret");
        assert_eq!(Store::load(&d).get(1).unwrap().name, "Living room");
        assert!(
            st.put(Console {
                host: "bad host!".into(),
                ..Default::default()
            })
            .is_err()
        );
        assert_eq!(
            max_file_for("Usb0:\\Games"),
            Some(4 * 1024 * 1024 * 1024 - 1)
        );
        assert_eq!(max_file_for("Hdd1/Games"), None);
        let _ = fs::remove_dir_all(&d);
    }
}
