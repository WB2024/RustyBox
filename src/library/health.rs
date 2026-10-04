//! Is a library path reachable, and how full is it?
//!
//! A hung NFS mount can block `stat` forever, so the check runs on a blocking thread with a
//! timeout and reports "not responding" rather than hanging the page.

use std::{
    collections::HashMap,
    ffi::CString,
    os::unix::ffi::OsStrExt,
    path::Path,
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Health {
    pub online: bool,
    pub total: u64,
    pub free: u64,
    /// Why it is offline, in plain words.
    pub problem: Option<String>,
    /// The filesystem (`vfat`, `ext4`, …) and the biggest single file it can hold, when known.
    pub fs_type: Option<String>,
    pub max_file: Option<u64>,
    /// A drive on another machine, and the name its agent gave it.
    pub remote_name: Option<String>,
    /// The agent shares it read-only.
    pub share_read_only: bool,
}

fn offline(why: impl Into<String>) -> Health {
    Health {
        online: false,
        total: 0,
        free: 0,
        problem: Some(why.into()),
        fs_type: None,
        max_file: None,
        remote_name: None,
        share_read_only: false,
    }
}

pub fn check_blocking(path: &Path) -> Health {
    match std::fs::metadata(path) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => return offline("Not a folder"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return offline("Folder not found (is the disk or share mounted?)");
        }
        Err(e) => return offline(e.to_string()),
    }
    if let Err(e) = std::fs::read_dir(path) {
        return offline(format!("Can't read it: {e}"));
    }
    let (total, free) = space(path).unwrap_or((0, 0));
    let fs_type = crate::fsops::fs_type_of(path);
    Health {
        online: true,
        total,
        free,
        problem: None,
        max_file: crate::fsops::max_file_for(&fs_type),
        fs_type: Some(fs_type),
        remote_name: None,
        share_read_only: false,
    }
}

/// (total, free-to-us) bytes of the filesystem holding `path`.
pub fn space(path: &Path) -> Option<(u64, u64)> {
    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid NUL-terminated path and `s` is a properly sized out-parameter.
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    let frsize = if s.f_frsize > 0 {
        s.f_frsize
    } else {
        s.f_bsize
    } as u64;
    Some((s.f_blocks as u64 * frsize, s.f_bavail as u64 * frsize))
}

/// Health of a library folder, whether it is here or on another machine.
pub async fn check_path(lp: &super::LibPath, timeout: Duration) -> Health {
    match lp.remote() {
        None => check(Path::new(&lp.path), timeout).await,
        Some(remote) => {
            let r = tokio::task::spawn_blocking(move || remote.info());
            match tokio::time::timeout(timeout.max(Duration::from_secs(8)), r).await {
                Ok(Ok(Ok(i))) => Health {
                    online: true,
                    total: i.fs.total,
                    free: i.fs.free,
                    problem: None,
                    fs_type: Some(i.fs.fs_type),
                    max_file: i.fs.max_file,
                    remote_name: Some(i.name),
                    share_read_only: i.read_only,
                },
                Ok(Ok(Err(e))) => offline(e.to_string()),
                Ok(Err(e)) => offline(e.to_string()),
                Err(_) => offline("The drive agent isn't answering"),
            }
        }
    }
}

/// How long a result is reused. Pages ask about every folder each time they load, and a dead
/// network share leaves a blocked thread behind per question, so asking less often matters.
const FRESH_FOR: Duration = Duration::from_secs(8);
const OFFLINE_FRESH_FOR: Duration = Duration::from_secs(20);

static RECENT: LazyLock<Mutex<HashMap<String, (Instant, Health)>>> =
    LazyLock::new(Default::default);

/// `check_path`, but a recent answer for the same folder is reused. For pages, not for jobs
/// (which should look again).
pub async fn check_path_cached(lp: &super::LibPath, timeout: Duration) -> Health {
    let key = format!(
        "{}|{}|{}",
        lp.path,
        lp.remote_url.as_deref().unwrap_or(""),
        lp.remote_subdir.as_deref().unwrap_or("")
    );
    let known = {
        let recent = RECENT.lock().unwrap_or_else(|e| e.into_inner());
        recent.get(&key).and_then(|(at, h)| {
            let keep = if h.online {
                FRESH_FOR
            } else {
                OFFLINE_FRESH_FOR
            };
            (at.elapsed() < keep).then(|| h.clone())
        })
    };
    if let Some(h) = known {
        return h;
    }
    let h = check_path(lp, timeout).await;
    let mut recent = RECENT.lock().unwrap_or_else(|e| e.into_inner());
    if recent.len() > 256 {
        recent.retain(|_, (at, _)| at.elapsed() < OFFLINE_FRESH_FOR);
    }
    recent.insert(key, (Instant::now(), h.clone()));
    h
}

pub async fn check(path: &Path, timeout: Duration) -> Health {
    let p = path.to_path_buf();
    match tokio::time::timeout(
        timeout,
        tokio::task::spawn_blocking(move || check_blocking(&p)),
    )
    .await
    {
        Ok(Ok(h)) => h,
        Ok(Err(e)) => offline(e.to_string()),
        Err(_) => offline("Not responding (a network share may be down)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_online_with_space_and_offline_with_a_reason() {
        let h = check_blocking(Path::new("/tmp"));
        assert!(h.online && h.total > 0 && h.free <= h.total);
        let gone = check_blocking(Path::new("/definitely/not/here"));
        assert!(!gone.online);
        assert!(gone.problem.unwrap().contains("mounted"));
        assert!(!check_blocking(Path::new("/etc/hostname")).online);
    }
}
