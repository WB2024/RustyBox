//! Checking (and optionally fixing) an Xbox 360 ISO with `abgx360`, run as a separate program.
//!
//! abgx360 is GPL software, so RustyBox only ever runs it as its own process and never links it.
//! It can rebuild or delete an image on its own unless told not to, so these runs always pass
//! `--norebuild`, never create side files, and never use the network. A plain check also passes
//! `--nowrite`, which makes writing impossible.

use std::{
    ffi::OsString,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
};

use serde_json::json;
use tokio::{
    io::{AsyncReadExt, BufReader},
    process::Command,
};

use super::Ctl;
use crate::{error::Error, jobs::Job};

pub const BACKUP_SUFFIX: &str = ".rustybox-backup";

/// Where the abgx360 program is: the configured path, or `abgx360` on the PATH.
pub fn find(configured: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = configured {
        return p.is_file().then(|| p.to_path_buf());
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join("abgx360"))
            .find(|p| p.is_file())
    })
}

pub fn args(iso: &Path, fix: bool) -> Vec<OsString> {
    let mut a: Vec<OsString> = ["-s", "-o", "--noupdate", "-b", "-d"]
        .iter()
        .map(Into::into)
        .collect();
    if fix {
        // Fix whatever can be fixed, including zero padding.
        a.extend(["--af3", "-p"].iter().map(OsString::from));
    } else {
        a.extend(["--nowrite", "--af0"].iter().map(OsString::from));
    }
    a.push(iso.into());
    a
}

pub fn backup_path(iso: &Path) -> PathBuf {
    let mut s = iso.as_os_str().to_owned();
    s.push(BACKUP_SUFFIX);
    PathBuf::from(s)
}

/// Copy `from` to `to` (which must not exist), reporting progress and stopping when cancelled.
pub fn copy_backup(from: &Path, to: &Path, ctl: &Ctl) -> Result<(), Error> {
    let total = fs::metadata(from)?.len();
    let mut src = File::open(from)?;
    let mut dst = File::options()
        .write(true)
        .create_new(true)
        .open(to)
        .map_err(|e| Error::conflict(format!("Can't create the backup {}: {e}", to.display())))?;
    let mut buf = vec![0u8; 4 << 20];
    let mut done = 0u64;
    let result = (|| -> Result<(), Error> {
        loop {
            ctl.check()?;
            let n = src.read(&mut buf)?;
            if n == 0 {
                break;
            }
            dst.write_all(&buf[..n])?;
            done += n as u64;
            (ctl.progress)(done as f32 / total.max(1) as f32, "Backing up");
        }
        dst.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(to);
    }
    result
}

/// Run abgx360 on `iso`, passing its output to the job's log.
pub async fn run(job: Arc<Job>, program: PathBuf, iso: PathBuf, fix: bool) -> Result<(), Error> {
    let mut child = Command::new(&program)
        .args(args(&iso, fix))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| Error::backend(format!("Could not start abgx360: {e}")))?;
    let (out, err) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let (j1, j2) = (job.clone(), job.clone());
    let t1 = tokio::spawn(async move { pump(out, j1).await });
    let t2 = tokio::spawn(async move { pump(err, j2).await });
    let status = child
        .wait()
        .await
        .map_err(|e| Error::backend(e.to_string()))?;
    let (a, b) = tokio::join!(t1, t2);
    let errors: Vec<String> = a
        .unwrap_or_default()
        .into_iter()
        .chain(b.unwrap_or_default())
        .collect();
    job.result(
        "abgx",
        json!({"fix": fix, "exit_code": status.code(), "file": iso.to_string_lossy(), "errors": errors}),
    );
    // abgx360 exits 0 even when it reports an ERROR (for example "isn't recognized as an XBOX 360 ISO"),
    // so its own error lines count as a failure too.
    if status.success() && errors.is_empty() {
        Ok(())
    } else if status.success() {
        Err(Error::backend(format!(
            "abgx360 reported a problem: {}",
            errors.join(" / ")
        )))
    } else {
        Err(Error::backend(format!(
            "abgx360 finished with an error (exit code {}). The log above has its full output.",
            status.code().map_or("none".to_string(), |c| c.to_string())
        )))
    }
}

/// abgx360 redraws progress with carriage returns, so both \n and \r end a line.
/// Returns the lines abgx360 marked as errors.
async fn pump<R: tokio::io::AsyncRead + Unpin>(r: R, job: Arc<Job>) -> Vec<String> {
    let mut errors = Vec::new();
    let mut note = |text: String, job: &Job| {
        if text.to_ascii_uppercase().starts_with("ERROR") {
            errors.push(text.clone());
        }
        job.log(text);
    };
    let mut reader = BufReader::new(r);
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                while let Some(pos) = buf.iter().position(|b| *b == b'\n' || *b == b'\r') {
                    let line: Vec<u8> = buf.drain(..=pos).collect();
                    let text = String::from_utf8_lossy(&line).trim().to_string();
                    if !text.is_empty() {
                        note(text, &job);
                    }
                }
            }
        }
    }
    let rest = String::from_utf8_lossy(&buf).trim().to_string();
    if !rest.is_empty() {
        note(rest, &job);
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_check_can_never_write_and_a_fix_never_rebuilds() {
        let check: Vec<String> = args(Path::new("/x/g.iso"), false)
            .iter()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert!(check.contains(&"--nowrite".to_string()) && check.contains(&"--af0".to_string()));
        assert!(!check.contains(&"--af3".to_string()));
        let fix: Vec<String> = args(Path::new("/x/g.iso"), true)
            .iter()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert!(fix.contains(&"--af3".to_string()) && !fix.contains(&"--nowrite".to_string()));
        for a in [&check, &fix] {
            // Never rebuild or delete, never create a .dvd file, never go online.
            for must in ["-b", "-d", "-o", "--noupdate"] {
                assert!(a.contains(&must.to_string()), "{must} missing from {a:?}");
            }
            assert_eq!(a.last().unwrap(), "/x/g.iso");
        }
    }

    #[test]
    fn backup_names_sit_next_to_the_image() {
        assert_eq!(
            backup_path(Path::new("/d/Game.iso")),
            PathBuf::from("/d/Game.iso.rustybox-backup")
        );
    }
}
