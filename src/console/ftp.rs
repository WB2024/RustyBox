//! A small FTP client written for the quirks of Aurora's FTP server on a modded Xbox 360:
//!
//! - `LIST` ignores its argument and lists the current folder, so a listing is always `CWD` first,
//!   then `LIST` with nothing after it;
//! - there is no `MLSD`/`MLST` and `SIZE` may not exist, so sizes come from listings;
//! - folders are made one level at a time with raw `MKD`, and "already exists" is not an error;
//! - the root lists the drives: `Hdd1`, `Usb0`, `Usb1`, `Game`, ...;
//! - writes are plain sequential `STOR`s, so an interrupted file is sent again from the start.
//!
//! It is blocking (std sockets) and meant to run inside `spawn_blocking` or a job.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{IpAddr, TcpStream, ToSocketAddrs},
    time::Duration,
};

use crate::error::Error;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const IO_TIMEOUT: Duration = Duration::from_secs(30);

/// Where and how to log in.
#[derive(Debug, Clone)]
pub struct Login {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

pub struct Ftp {
    login: Login,
    ctl: BufReader<TcpStream>,
    peer: IpAddr,
    store: Option<TcpStream>,
}

fn unreachable(host: &str, why: impl std::fmt::Display) -> Error {
    Error::coded(
        502,
        "CONSOLE_UNREACHABLE",
        format!(
            "Can't reach the console at {host}: {why}. Is it on, is Aurora running, and is FTP switched on in Aurora's settings?"
        ),
    )
}

fn ftp_err(code: u16, text: &str) -> Error {
    let t = text.trim();
    match code {
        530 => Error::coded(
            401,
            "CONSOLE_LOGIN",
            "The console refused the login. Aurora's FTP user and password are both `xbox` unless you changed them.",
        ),
        550 => Error::not_found(format!("Not found on the console ({t})")),
        _ => Error::backend(format!("The console answered {code} {t}")),
    }
}

/// Turn an Xbox-style path (`Hdd:\Aurora\x`, `Usb0:/Games`) or a plain one into the FTP path
/// Aurora shows: `/Hdd1/Aurora/x`. Backslashes become slashes and doubled slashes collapse.
pub fn ftp_path(path: &str) -> String {
    let mut p = path.replace('\\', "/");
    while p.contains("//") {
        p = p.replace("//", "/");
    }
    let lower = p.to_lowercase();
    for (prefix, drive) in [
        ("hdd1:", "Hdd1"),
        ("hdd:", "Hdd1"),
        ("usb0:", "Usb0"),
        ("usb1:", "Usb1"),
        ("usb:", "Usb0"),
        ("dvdrom0:", "Game"),
    ] {
        if lower.starts_with(prefix) {
            let rest = p[prefix.len()..].trim_matches('/');
            return if rest.is_empty() {
                format!("/{drive}")
            } else {
                format!("/{drive}/{rest}")
            };
        }
    }
    let p = p.trim_end_matches('/');
    if p.is_empty() {
        "/".into()
    } else if p.starts_with('/') {
        p.into()
    } else {
        format!("/{p}")
    }
}

/// Parent folder of an FTP path (`/` for a top-level name).
pub fn parent(path: &str) -> String {
    let p = path.trim_end_matches('/');
    match p.rsplit_once('/') {
        Some(("", _)) | None => "/".into(),
        Some((a, _)) => a.into(),
    }
}

/// The last part of an FTP path.
pub fn file_name(path: &str) -> &str {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or("")
}

/// One line of a `LIST` answer, in Unix (`drwxr-xr-x 1 x x 0 Jan 1 00:00 name`) or DOS
/// (`01-01-20 12:00AM <DIR> name`) style.
pub fn parse_list_line(line: &str) -> Option<Entry> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.trim().is_empty() || line.starts_with("total ") {
        return None;
    }
    let first = line.chars().next()?;
    let (name, is_dir, size) = if matches!(first, 'd' | '-' | 'l') {
        // Unix: eight whitespace-separated fields, then the name (which may contain spaces).
        let mut rest = line;
        let mut fields = Vec::new();
        for _ in 0..8 {
            rest = rest.trim_start();
            let end = rest.find(char::is_whitespace)?;
            fields.push(&rest[..end]);
            rest = &rest[end..];
        }
        let name = rest
            .strip_prefix(' ')
            .unwrap_or(rest)
            .trim_start_matches(' ');
        let size = fields[4].parse::<u64>().unwrap_or(0);
        (name.to_string(), first == 'd', size)
    } else if first.is_ascii_digit() {
        // DOS: date, time, <DIR> or size, name.
        let mut parts = line.split_whitespace();
        let (_d, _t) = (parts.next()?, parts.next()?);
        let kind = parts.next()?;
        let idx = line.find(kind)? + kind.len();
        let name = line[idx..].trim_start().to_string();
        let is_dir = kind.eq_ignore_ascii_case("<dir>");
        (name, is_dir, kind.replace(',', "").parse().unwrap_or(0))
    } else {
        return None;
    };
    let name = name.split(" -> ").next().unwrap_or(&name).to_string();
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    Some(Entry { name, is_dir, size })
}

impl Ftp {
    pub fn connect(login: &Login) -> Result<Ftp, Error> {
        let addr = (login.host.as_str(), login.port)
            .to_socket_addrs()
            .map_err(|e| unreachable(&login.host, e))?
            .next()
            .ok_or_else(|| unreachable(&login.host, "no such host"))?;
        let sock = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
            .map_err(|e| unreachable(&login.host, e))?;
        sock.set_read_timeout(Some(IO_TIMEOUT)).ok();
        sock.set_write_timeout(Some(IO_TIMEOUT)).ok();
        sock.set_nodelay(true).ok();
        let peer = addr.ip();
        let mut me = Ftp {
            login: login.clone(),
            ctl: BufReader::new(sock),
            peer,
            store: None,
        };
        let (code, text) = me.reply()?;
        if code != 220 {
            return Err(ftp_err(code, &text));
        }
        let (code, text) = me.cmd(&format!("USER {}", login.user))?;
        match code {
            230 => {}
            331 | 332 => {
                let (code, text) = me.cmd(&format!("PASS {}", login.password))?;
                if code != 230 {
                    return Err(ftp_err(code, &text));
                }
            }
            _ => return Err(ftp_err(code, &text)),
        }
        me.cmd("TYPE I")?;
        Ok(me)
    }

    /// Read one reply, joining a multi-line one (`123-...` until `123 ...`).
    fn reply(&mut self) -> Result<(u16, String), Error> {
        let mut text = String::new();
        let mut code = 0u16;
        loop {
            let mut line = String::new();
            let n = self
                .ctl
                .read_line(&mut line)
                .map_err(|e| unreachable(&self.login.host, e))?;
            if n == 0 {
                return Err(unreachable(&self.login.host, "the connection closed"));
            }
            let l = line.trim_end();
            if l.len() < 3 {
                continue;
            }
            let c = l[..3].parse::<u16>().unwrap_or(0);
            if code == 0 {
                code = c;
            }
            text.push_str(l.get(4..).unwrap_or(""));
            text.push('\n');
            if c == code && l.as_bytes().get(3) != Some(&b'-') {
                return Ok((code, text));
            }
        }
    }

    fn cmd(&mut self, line: &str) -> Result<(u16, String), Error> {
        let host = self.login.host.clone();
        let w = self.ctl.get_mut();
        w.write_all(format!("{line}\r\n").as_bytes())
            .map_err(|e| unreachable(&host, e))?;
        self.reply()
    }

    fn expect(&mut self, line: &str, ok: &[u16]) -> Result<String, Error> {
        let (code, text) = self.cmd(line)?;
        if ok.contains(&code) {
            Ok(text)
        } else {
            Err(ftp_err(code, &text))
        }
    }

    fn data(&mut self) -> Result<TcpStream, Error> {
        let text = self.expect("PASV", &[227])?;
        let open = text
            .find('(')
            .ok_or_else(|| Error::backend("Odd PASV answer"))?;
        let close = text
            .find(')')
            .ok_or_else(|| Error::backend("Odd PASV answer"))?;
        let n: Vec<u16> = text[open + 1..close]
            .split(',')
            .filter_map(|x| x.trim().parse().ok())
            .collect();
        if n.len() != 6 {
            return Err(Error::backend("Odd PASV answer"));
        }
        // The address in the answer can be wrong behind NAT or on odd firmwares: use the one we dialled.
        let addr = std::net::SocketAddr::new(self.peer, n[4] * 256 + n[5]);
        let s = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
            .map_err(|e| unreachable(&self.login.host, e))?;
        s.set_read_timeout(Some(IO_TIMEOUT)).ok();
        s.set_write_timeout(Some(IO_TIMEOUT)).ok();
        Ok(s)
    }

    /// The entries of a folder (`CWD`, then a bare `LIST`).
    pub fn list(&mut self, path: &str) -> Result<Vec<Entry>, Error> {
        let path = ftp_path(path);
        self.expect(&format!("CWD {path}"), &[250, 200])?;
        let mut d = self.data()?;
        let (code, text) = self.cmd("LIST")?;
        if !matches!(code, 125 | 150) {
            return Err(ftp_err(code, &text));
        }
        let mut raw = Vec::new();
        d.read_to_end(&mut raw)
            .map_err(|e| unreachable(&self.login.host, e))?;
        drop(d);
        let (code, text) = self.reply()?;
        if !matches!(code, 226 | 250) {
            return Err(ftp_err(code, &text));
        }
        Ok(String::from_utf8_lossy(&raw)
            .lines()
            .filter_map(parse_list_line)
            .collect())
    }

    /// What is at `path`, found from its folder's listing.
    pub fn stat(&mut self, path: &str) -> Result<Option<Entry>, Error> {
        let path = ftp_path(path);
        if path == "/" {
            return Ok(Some(Entry {
                name: String::new(),
                is_dir: true,
                size: 0,
            }));
        }
        match self.list(&parent(&path)) {
            Ok(es) => Ok(es.into_iter().find(|e| e.name == file_name(&path))),
            Err(Error::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Make a folder and any missing ones above it.
    pub fn mkdir_p(&mut self, path: &str) -> Result<(), Error> {
        let path = ftp_path(path);
        let mut cur = String::new();
        for part in path.split('/').filter(|p| !p.is_empty()) {
            cur = format!("{cur}/{part}");
            // Aurora answers an error when it exists; that is fine.
            let _ = self.cmd(&format!("MKD {cur}"))?;
        }
        Ok(())
    }

    pub fn delete_file(&mut self, path: &str) -> Result<(), Error> {
        self.expect(&format!("DELE {}", ftp_path(path)), &[250, 200])
            .map(|_| ())
    }

    pub fn remove_dir(&mut self, path: &str) -> Result<(), Error> {
        self.expect(&format!("RMD {}", ftp_path(path)), &[250, 200])
            .map(|_| ())
    }

    /// Delete a folder and everything in it, files first and folders innermost first.
    pub fn delete_tree(&mut self, path: &str) -> Result<(), Error> {
        let path = ftp_path(path);
        if path == "/" || path.matches('/').count() < 2 {
            return Err(Error::validation(
                "Refusing to delete a whole drive or the top of the console",
            ));
        }
        let mut dirs = vec![path.clone()];
        let mut stack = vec![path.clone()];
        let mut files = Vec::new();
        while let Some(d) = stack.pop() {
            for e in self.list(&d)? {
                let child = format!("{d}/{}", e.name);
                if e.is_dir {
                    dirs.push(child.clone());
                    stack.push(child);
                } else {
                    files.push(child);
                }
            }
        }
        for f in files {
            self.delete_file(&f)?;
        }
        for d in dirs.into_iter().rev() {
            self.remove_dir(&d)?;
        }
        Ok(())
    }

    pub fn rename(&mut self, from: &str, to: &str) -> Result<(), Error> {
        self.expect(&format!("RNFR {}", ftp_path(from)), &[350])?;
        self.expect(&format!("RNTO {}", ftp_path(to)), &[250, 200])
            .map(|_| ())
    }

    /// Read up to `len` bytes of a file from `offset`.
    pub fn read(&mut self, path: &str, offset: u64, len: u64) -> Result<Vec<u8>, Error> {
        let path = ftp_path(path);
        let mut d = self.data()?;
        if offset > 0 {
            self.expect(&format!("REST {offset}"), &[350])?;
        }
        let (code, text) = self.cmd(&format!("RETR {path}"))?;
        if !matches!(code, 125 | 150) {
            return Err(ftp_err(code, &text));
        }
        let mut out = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        while (out.len() as u64) < len {
            let want = buf.len().min((len - out.len() as u64) as usize);
            let n = d
                .read(&mut buf[..want])
                .map_err(|e| unreachable(&self.login.host, e))?;
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        drop(d);
        // Stopping early makes the server say 426; reading the whole thing says 226.
        let (code, text) = self.reply()?;
        if !matches!(code, 226 | 250 | 426 | 451 | 425) {
            return Err(ftp_err(code, &text));
        }
        Ok(out)
    }

    /// Stream a whole file (from `offset`) into `out`, calling `progress` with the bytes so far.
    /// Reading to the end means the server finishes the transfer itself, so there is no early
    /// close for a server to be slow about.
    pub fn read_to(
        &mut self,
        path: &str,
        offset: u64,
        out: &mut dyn Write,
        progress: &mut dyn FnMut(u64) -> Result<(), Error>,
    ) -> Result<u64, Error> {
        let path = ftp_path(path);
        let mut d = self.data()?;
        if offset > 0 {
            self.expect(&format!("REST {offset}"), &[350])?;
        }
        let (code, text) = self.cmd(&format!("RETR {path}"))?;
        if !matches!(code, 125 | 150) {
            return Err(ftp_err(code, &text));
        }
        let mut buf = vec![0u8; 256 * 1024];
        let mut total = 0u64;
        loop {
            let n = d
                .read(&mut buf)
                .map_err(|e| unreachable(&self.login.host, e))?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
            total += n as u64;
            progress(total)?;
        }
        drop(d);
        let (code, text) = self.reply()?;
        if !matches!(code, 226 | 250) {
            return Err(ftp_err(code, &text));
        }
        Ok(total)
    }

    /// Start writing a file. Follow with `store_write` calls and one `store_finish`.
    pub fn store_start(&mut self, path: &str) -> Result<(), Error> {
        let path = ftp_path(path);
        if let Some(dir) = path.rsplit_once('/').map(|(d, _)| d.to_string())
            && !dir.is_empty()
        {
            self.mkdir_p(&dir)?;
        }
        let d = self.data()?;
        let (code, text) = self.cmd(&format!("STOR {path}"))?;
        if !matches!(code, 125 | 150) {
            return Err(ftp_err(code, &text));
        }
        self.store = Some(d);
        Ok(())
    }

    pub fn store_write(&mut self, data: &[u8]) -> Result<(), Error> {
        let host = self.login.host.clone();
        let d = self
            .store
            .as_mut()
            .ok_or_else(|| Error::backend("No file is being written"))?;
        d.write_all(data).map_err(|e| unreachable(&host, e))
    }

    pub fn store_finish(&mut self) -> Result<(), Error> {
        if let Some(mut d) = self.store.take() {
            let _ = d.flush();
            let _ = d.shutdown(std::net::Shutdown::Write);
            drop(d);
            let (code, text) = self.reply()?;
            if !matches!(code, 226 | 250) {
                return Err(ftp_err(code, &text));
            }
        }
        Ok(())
    }

    /// Give up on a half-written file (the data connection is dropped).
    pub fn store_abort(&mut self) {
        if self.store.take().is_some() {
            let _ = self.reply();
        }
    }

    pub fn quit(mut self) {
        let _ = self.cmd("QUIT");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xbox_paths_become_ftp_paths() {
        assert_eq!(ftp_path("Hdd:\\Aurora\\User"), "/Hdd1/Aurora/User");
        assert_eq!(ftp_path("Usb0:\\"), "/Usb0");
        assert_eq!(ftp_path("usb1:/Games//x"), "/Usb1/Games/x");
        assert_eq!(ftp_path("Hdd1/Games"), "/Hdd1/Games");
        assert_eq!(ftp_path(""), "/");
        assert_eq!(parent("/Hdd1/Games/x"), "/Hdd1/Games");
        assert_eq!(parent("/Hdd1"), "/");
        assert_eq!(file_name("/Hdd1/Games/x/"), "x");
    }

    #[test]
    fn list_lines_in_unix_and_dos_style() {
        let d =
            parse_list_line("drwxr-xr-x   1 root  root         0 Jan 01  2010 My Game").unwrap();
        assert_eq!(
            d,
            Entry {
                name: "My Game".into(),
                is_dir: true,
                size: 0
            }
        );
        let f = parse_list_line("-rw-r--r-- 1 x x 123456 Jan 01 00:00 Data0000").unwrap();
        assert_eq!(
            (f.name.as_str(), f.is_dir, f.size),
            ("Data0000", false, 123456)
        );
        let dos = parse_list_line("01-01-12  12:00AM       <DIR>          Hdd1").unwrap();
        assert_eq!((dos.name.as_str(), dos.is_dir), ("Hdd1", true));
        let dos_file = parse_list_line("01-01-12  12:00AM          1,024 a b.xex").unwrap();
        assert_eq!((dos_file.name.as_str(), dos_file.size), ("a b.xex", 1024));
        assert!(parse_list_line("total 4").is_none());
        assert!(parse_list_line("drwxr-xr-x 1 x x 0 Jan 01 00:00 ..").is_none());
    }
}
