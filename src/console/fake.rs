//! A pretend Aurora FTP server on a folder, for tests and `--mock`. It has Aurora's quirks on
//! purpose: `LIST` ignores its argument, `MLSD` and `SIZE` don't exist, and `STOR` won't make
//! folders, so the real client has to `CWD`, make folders itself and so on.

use std::{
    fs,
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::Arc,
    thread,
};

pub struct FakeAurora {
    pub addr: SocketAddr,
    pub root: PathBuf,
}

struct Cfg {
    root: PathBuf,
    user: String,
    pass: String,
    quirks: Quirks,
}

/// Ways a real console's FTP server might differ from the usual, for tests.
#[derive(Debug, Clone, Copy, Default)]
pub struct Quirks {
    /// List in the DOS style (`01-01-10  12:00AM  <DIR>  name`) instead of the Unix one.
    pub dos_list: bool,
    /// Say a wrong address in the passive-mode answer (as a server behind NAT might).
    pub wrong_pasv_ip: bool,
}

/// Start serving `root` on a free port of the loopback address. The thread runs until the
/// process ends.
pub fn start(root: &Path, user: &str, pass: &str) -> std::io::Result<FakeAurora> {
    start_with(root, user, pass, Quirks::default())
}

pub fn start_with(
    root: &Path,
    user: &str,
    pass: &str,
    quirks: Quirks,
) -> std::io::Result<FakeAurora> {
    fs::create_dir_all(root)?;
    let root = fs::canonicalize(root)?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    let cfg = Arc::new(Cfg {
        root: root.clone(),
        user: user.into(),
        pass: pass.into(),
        quirks,
    });
    thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let cfg = cfg.clone();
            thread::spawn(move || {
                let _ = session(conn, &cfg);
            });
        }
    });
    Ok(FakeAurora { addr, root })
}

fn say(w: &mut TcpStream, s: &str) -> std::io::Result<()> {
    w.write_all(format!("{s}\r\n").as_bytes())
}

/// A virtual path (absolute, or relative to `cwd`) made absolute with no `.` or `..`.
fn norm(cwd: &str, arg: &str) -> Option<String> {
    let base = if arg.starts_with('/') {
        String::new()
    } else {
        cwd.to_string()
    };
    let mut parts: Vec<&str> = base.split('/').filter(|p| !p.is_empty()).collect();
    for p in arg.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            x => parts.push(x),
        }
    }
    Some(format!("/{}", parts.join("/")))
}

fn real(cfg: &Cfg, v: &str) -> PathBuf {
    cfg.root.join(v.trim_start_matches('/'))
}

fn list_line(p: &Path, dos: bool) -> Option<String> {
    let m = fs::metadata(p).ok()?;
    let name = p.file_name()?.to_string_lossy();
    if dos {
        return Some(if m.is_dir() {
            format!("01-01-10  12:00AM       <DIR>          {name}")
        } else {
            format!("01-01-10  12:00AM {:>14} {name}", m.len())
        });
    }
    Some(format!(
        "{}rwxr-xr-x   1 root  root  {:>12} Jan 01  2010 {name}",
        if m.is_dir() { 'd' } else { '-' },
        if m.is_dir() { 0 } else { m.len() }
    ))
}

fn session(conn: TcpStream, cfg: &Cfg) -> std::io::Result<()> {
    let mut w = conn.try_clone()?;
    let mut r = BufReader::new(conn);
    say(&mut w, "220 Aurora FTP (pretend)")?;
    let (mut cwd, mut authed, mut user_ok) = ("/".to_string(), false, false);
    let (mut pasv, mut rnfr, mut rest): (Option<TcpListener>, Option<PathBuf>, u64) =
        (None, None, 0);
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let l = line.trim_end();
        let (cmd, arg) = l.split_once(' ').unwrap_or((l, ""));
        let cmd = cmd.to_uppercase();
        if !authed && !matches!(cmd.as_str(), "USER" | "PASS" | "QUIT") {
            say(&mut w, "530 Please log in")?;
            continue;
        }
        match cmd.as_str() {
            "USER" => {
                user_ok = arg == cfg.user;
                say(&mut w, "331 Password please")?;
            }
            "PASS" => {
                if user_ok && arg == cfg.pass {
                    authed = true;
                    say(&mut w, "230 Welcome")?;
                } else {
                    say(&mut w, "530 Login incorrect")?;
                }
            }
            "TYPE" => say(&mut w, "200 OK")?,
            "NOOP" => say(&mut w, "200 OK")?,
            "SYST" => say(&mut w, "215 UNIX Type: L8")?,
            "PWD" => say(&mut w, &format!("257 \"{cwd}\""))?,
            "QUIT" => {
                say(&mut w, "221 Bye")?;
                return Ok(());
            }
            "CWD" => match norm(&cwd, arg).filter(|v| real(cfg, v).is_dir()) {
                Some(v) => {
                    cwd = v;
                    say(&mut w, "250 OK")?;
                }
                None => say(&mut w, "550 No such folder")?,
            },
            "PASV" => {
                let l = TcpListener::bind("127.0.0.1:0")?;
                let p = l.local_addr()?.port();
                pasv = Some(l);
                let ip = if cfg.quirks.wrong_pasv_ip {
                    "10,99,99,99"
                } else {
                    "127,0,0,1"
                };
                say(
                    &mut w,
                    &format!("227 Entering Passive Mode ({ip},{},{})", p / 256, p % 256),
                )?;
            }
            "REST" => {
                rest = arg.parse().unwrap_or(0);
                say(&mut w, "350 OK")?;
            }
            "LIST" => {
                // Aurora ignores the argument and lists the current folder.
                let Some(l) = pasv.take() else {
                    say(&mut w, "425 Use PASV first")?;
                    continue;
                };
                say(&mut w, "150 Here it comes")?;
                let (mut d, _) = l.accept()?;
                let mut names: Vec<_> = fs::read_dir(real(cfg, &cwd))?
                    .flatten()
                    .map(|e| e.path())
                    .collect();
                names.sort();
                for p in names {
                    if let Some(s) = list_line(&p, cfg.quirks.dos_list) {
                        d.write_all(format!("{s}\r\n").as_bytes())?;
                    }
                }
                drop(d);
                say(&mut w, "226 Done")?;
            }
            "RETR" => {
                let p = norm(&cwd, arg).map(|v| real(cfg, &v));
                let (Some(p), Some(l)) = (p.filter(|p| p.is_file()), pasv.take()) else {
                    say(&mut w, "550 No such file")?;
                    continue;
                };
                say(&mut w, "150 Sending")?;
                let (mut d, _) = l.accept()?;
                let mut f = fs::File::open(&p)?;
                f.seek(SeekFrom::Start(std::mem::take(&mut rest)))?;
                let ok = std::io::copy(&mut f, &mut d).is_ok();
                drop(d);
                say(&mut w, if ok { "226 Done" } else { "426 Aborted" })?;
            }
            "STOR" => {
                let p = norm(&cwd, arg).map(|v| real(cfg, &v));
                let Some(l) = pasv.take() else {
                    say(&mut w, "425 Use PASV first")?;
                    continue;
                };
                // Unlike a friendlier server, this one won't make folders.
                let Some(p) = p.filter(|p| p.parent().is_some_and(|d| d.is_dir())) else {
                    say(&mut w, "550 No such folder")?;
                    continue;
                };
                say(&mut w, "150 Receiving")?;
                let (mut d, _) = l.accept()?;
                let mut f = fs::File::create(&p)?;
                let mut buf = vec![0u8; 64 * 1024];
                loop {
                    let n = d.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    f.write_all(&buf[..n])?;
                }
                say(&mut w, "226 Done")?;
            }
            "MKD" => match norm(&cwd, arg).map(|v| real(cfg, &v)) {
                Some(p) if !p.exists() && p.parent().is_some_and(|d| d.is_dir()) => {
                    fs::create_dir(&p)?;
                    say(&mut w, "257 Created")?;
                }
                _ => say(&mut w, "550 Can't make it")?,
            },
            "DELE" => match norm(&cwd, arg).map(|v| real(cfg, &v)) {
                Some(p) if p.is_file() => {
                    fs::remove_file(p)?;
                    say(&mut w, "250 Deleted")?;
                }
                _ => say(&mut w, "550 No such file")?,
            },
            "RMD" => match norm(&cwd, arg).map(|v| real(cfg, &v)) {
                Some(p) if p.is_dir() && fs::remove_dir(&p).is_ok() => say(&mut w, "250 Removed")?,
                _ => say(&mut w, "550 Can't remove it")?,
            },
            "RNFR" => match norm(&cwd, arg).map(|v| real(cfg, &v)) {
                Some(p) if p.exists() => {
                    rnfr = Some(p);
                    say(&mut w, "350 Ready")?;
                }
                _ => say(&mut w, "550 No such file")?,
            },
            "RNTO" => match (rnfr.take(), norm(&cwd, arg).map(|v| real(cfg, &v))) {
                (Some(a), Some(b)) if !b.exists() && fs::rename(&a, &b).is_ok() => {
                    say(&mut w, "250 Renamed")?
                }
                _ => say(&mut w, "550 Can't rename")?,
            },
            // Aurora has neither.
            "MLSD" | "MLST" => say(&mut w, "500 Not understood")?,
            "SIZE" | "FEAT" => say(&mut w, "502 Not implemented")?,
            _ => say(&mut w, "500 Not understood")?,
        }
    }
}
