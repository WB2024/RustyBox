//! Reading `.torrent` files and magnet links: what is inside, and the info hash that names it.
//!
//! The files come from indexers and from folders of downloaded torrents, so nothing in them is
//! trusted: sizes are bounded, nesting is bounded, and the names inside are only ever shown or
//! handed to the download client, never used as paths here.

use std::io::Read;

use sha1::{Digest, Sha1};

use crate::error::Error;

/// The biggest `.torrent` file read (the Redump Xbox 360 collection is about 10 MB).
pub const MAX_TORRENT_BYTES: usize = 64 * 1024 * 1024;
const MAX_DEPTH: usize = 24;
/// More files than this in one torrent is refused rather than shown.
const MAX_FILES: usize = 250_000;

#[derive(Debug, Clone, PartialEq)]
enum Value {
    Int(i64),
    Bytes(Vec<u8>),
    List(Vec<Value>),
    Dict(Vec<(Vec<u8>, Value)>),
}

impl Value {
    fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Dict(d) => d.iter().find(|(k, _)| k == key.as_bytes()).map(|(_, v)| v),
            _ => None,
        }
    }
    fn int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }
    fn bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }
    fn list(&self) -> Option<&[Value]> {
        match self {
            Value::List(l) => Some(l),
            _ => None,
        }
    }
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
    /// Where the value of the top-level `info` key starts and ends.
    info_span: Option<(usize, usize)>,
}

fn bad(why: &str) -> Error {
    Error::validation(format!("That isn't a readable torrent file ({why})"))
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn value(&mut self, depth: usize, top_dict: bool) -> Result<Value, Error> {
        if depth > MAX_DEPTH {
            return Err(bad("nested too deeply"));
        }
        match self.peek().ok_or_else(|| bad("it ends too soon"))? {
            b'i' => {
                self.i += 1;
                let end = self.b[self.i..]
                    .iter()
                    .position(|c| *c == b'e')
                    .ok_or_else(|| bad("a number never ends"))?;
                let n = std::str::from_utf8(&self.b[self.i..self.i + end])
                    .ok()
                    .and_then(|s| s.parse::<i64>().ok())
                    .ok_or_else(|| bad("a number isn't one"))?;
                self.i += end + 1;
                Ok(Value::Int(n))
            }
            b'l' => {
                self.i += 1;
                let mut out = Vec::new();
                while self.peek().ok_or_else(|| bad("a list never ends"))? != b'e' {
                    out.push(self.value(depth + 1, false)?);
                }
                self.i += 1;
                Ok(Value::List(out))
            }
            b'd' => {
                self.i += 1;
                let mut out = Vec::new();
                while self.peek().ok_or_else(|| bad("a dictionary never ends"))? != b'e' {
                    let Value::Bytes(k) = self.value(depth + 1, false)? else {
                        return Err(bad("a dictionary key isn't text"));
                    };
                    let start = self.i;
                    let v = self.value(depth + 1, false)?;
                    if top_dict && k == b"info" {
                        self.info_span = Some((start, self.i));
                    }
                    out.push((k, v));
                }
                self.i += 1;
                Ok(Value::Dict(out))
            }
            b'0'..=b'9' => {
                let colon = self.b[self.i..]
                    .iter()
                    .position(|c| *c == b':')
                    .ok_or_else(|| bad("a string has no length"))?;
                let n: usize = std::str::from_utf8(&self.b[self.i..self.i + colon])
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| bad("a string length isn't a number"))?;
                let start = self.i + colon + 1;
                let end = start.checked_add(n).filter(|e| *e <= self.b.len());
                let end = end.ok_or_else(|| bad("a string is longer than the file"))?;
                self.i = end;
                Ok(Value::Bytes(self.b[start..end].to_vec()))
            }
            _ => Err(bad("something unexpected in it")),
        }
    }
}

/// One file inside a torrent.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct TorrentFile {
    /// Its number in the torrent: what the download client calls it.
    pub index: usize,
    /// Folders and name inside the torrent, joined with `/` (the torrent's own top folder not included).
    pub path: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Torrent {
    /// The torrent's name (its top folder, or the file for a one-file torrent).
    pub name: String,
    /// 40 hex digits, lower case.
    pub info_hash: String,
    pub files: Vec<TorrentFile>,
    pub total: u64,
    /// Does it have one top folder (several files), or is it one file?
    pub multi: bool,
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).replace('\0', "")
}

/// A name inside a torrent made safe to show: no empty or dot parts.
fn clean_part(b: &[u8]) -> String {
    let s = text(b);
    match s.as_str() {
        "" | "." | ".." => "_".to_string(),
        _ => s.replace('/', "_"),
    }
}

pub fn parse(bytes: &[u8]) -> Result<Torrent, Error> {
    if bytes.len() > MAX_TORRENT_BYTES {
        return Err(bad("it is far bigger than a torrent file should be"));
    }
    let mut p = Parser {
        b: bytes,
        i: 0,
        info_span: None,
    };
    if p.peek() != Some(b'd') {
        return Err(bad("it doesn't start like one"));
    }
    let root = p.value(0, true)?;
    let (a, z) = p.info_span.ok_or_else(|| bad("it has no info section"))?;
    let info = root
        .get("info")
        .ok_or_else(|| bad("it has no info section"))?;
    let name = info
        .get("name.utf-8")
        .or_else(|| info.get("name"))
        .and_then(Value::bytes)
        .map(clean_part)
        .ok_or_else(|| bad("it has no name"))?;
    let mut files = Vec::new();
    let multi;
    if let Some(list) = info.get("files").and_then(Value::list) {
        multi = true;
        if list.len() > MAX_FILES {
            return Err(bad("it lists too many files"));
        }
        for (index, f) in list.iter().enumerate() {
            let size = f
                .get("length")
                .and_then(Value::int)
                .filter(|n| *n >= 0)
                .ok_or_else(|| bad("a file has no size"))? as u64;
            let parts = f
                .get("path.utf-8")
                .or_else(|| f.get("path"))
                .and_then(Value::list)
                .ok_or_else(|| bad("a file has no name"))?;
            let path = parts
                .iter()
                .filter_map(Value::bytes)
                .map(clean_part)
                .collect::<Vec<_>>()
                .join("/");
            if path.is_empty() {
                return Err(bad("a file has an empty name"));
            }
            files.push(TorrentFile { index, path, size });
        }
    } else {
        multi = false;
        let size = info
            .get("length")
            .and_then(Value::int)
            .filter(|n| *n >= 0)
            .ok_or_else(|| bad("it has neither a file list nor a size"))? as u64;
        files.push(TorrentFile {
            index: 0,
            path: name.clone(),
            size,
        });
    }
    let total = files.iter().map(|f| f.size).fold(0u64, u64::saturating_add);
    let hash = Sha1::digest(&bytes[a..z]);
    Ok(Torrent {
        name,
        info_hash: hash.iter().map(|b| format!("{b:02x}")).collect(),
        files,
        total,
        multi,
    })
}

// ── Magnet links ─────────────────────────────────────────────────────────────

fn base32(s: &str) -> Option<Vec<u8>> {
    let (mut bits, mut acc, mut out) = (0u32, 0u32, Vec::new());
    for c in s.chars() {
        let v = match c.to_ascii_uppercase() {
            c @ 'A'..='Z' => c as u32 - 'A' as u32,
            c @ '2'..='7' => c as u32 - '2' as u32 + 26,
            _ => return None,
        };
        acc = (acc << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// The info hash (40 lower-case hex digits) of a magnet link, and its display name if it has one.
pub fn magnet(link: &str) -> Option<(String, Option<String>)> {
    let q = link.strip_prefix("magnet:?")?;
    let (mut hash, mut dn) = (None, None);
    for part in q.split('&') {
        let (k, v) = part.split_once('=').unwrap_or((part, ""));
        match k {
            "xt" => {
                if let Some(h) = v.strip_prefix("urn:btih:") {
                    hash = match h.len() {
                        40 if h.bytes().all(|b| b.is_ascii_hexdigit()) => {
                            Some(h.to_ascii_lowercase())
                        }
                        32 => base32(h)
                            .filter(|b| b.len() == 20)
                            .map(|b| b.iter().map(|x| format!("{x:02x}")).collect::<String>()),
                        _ => None,
                    };
                }
            }
            "dn" => {
                dn = Some(
                    percent_decode(v)
                        .replace('+', " ")
                        .chars()
                        .filter(|c| !c.is_control())
                        .collect(),
                )
            }
            _ => {}
        }
    }
    hash.map(|h| (h, dn))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let (mut out, mut i) = (Vec::new(), 0);
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

// ── Fetching a torrent an indexer pointed at ─────────────────────────────────

/// What a download link turned out to be.
#[derive(Debug)]
pub enum Fetched {
    File(Vec<u8>),
    Magnet(String),
}

/// Hide key-like query values (`apikey=...`) from a message.
fn scrub_url(msg: &str) -> String {
    let mut out = String::new();
    let mut rest = msg;
    while let Some(i) = rest.find("apikey=") {
        out.push_str(&rest[..i + 7]);
        rest = &rest[i + 7..];
        let end = rest.find(['&', ' ', '"', ')', '\'']).unwrap_or(rest.len());
        out.push_str("***");
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// Get the torrent a release's link points at. A link can be the `.torrent` file, a magnet link,
/// or an address that answers with a redirect to a magnet link (Prowlarr does this for some
/// indexers), so redirects are followed by hand.
pub fn fetch(link: &str) -> Result<Fetched, Error> {
    let mut url = link.to_string();
    for _ in 0..6 {
        if url.starts_with("magnet:") {
            return Ok(Fetched::Magnet(url));
        }
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(Error::validation(
                "The indexer gave a link that isn't a web address or a magnet",
            ));
        }
        let agent = ureq::AgentBuilder::new()
            .redirects(0)
            .timeout_connect(std::time::Duration::from_secs(10))
            .timeout_read(std::time::Duration::from_secs(60))
            .user_agent("RustyBox")
            .build();
        let resp = match agent.get(&url).call() {
            Ok(r) => r,
            Err(ureq::Error::Status(code, r)) if (300..400).contains(&code) => r,
            Err(ureq::Error::Status(code, _)) => {
                return Err(Error::backend(format!(
                    "The indexer answered {code} for the torrent"
                )));
            }
            Err(ureq::Error::Transport(t)) => {
                return Err(Error::backend(scrub_url(&format!(
                    "Can't fetch the torrent: {t}"
                ))));
            }
        };
        if (300..400).contains(&resp.status()) {
            let Some(next) = resp.header("location") else {
                return Err(Error::backend(
                    "The indexer redirected without saying where",
                ));
            };
            url = if next.starts_with("http") || next.starts_with("magnet:") {
                next.to_string()
            } else {
                // A path on the same server.
                let base = url.split('/').take(3).collect::<Vec<_>>().join("/");
                format!(
                    "{base}{}{next}",
                    if next.starts_with('/') { "" } else { "/" }
                )
            };
            continue;
        }
        let mut bytes = Vec::new();
        std::io::Read::take(resp.into_reader(), MAX_TORRENT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| {
                Error::backend(scrub_url(&format!("The torrent couldn't be read: {e}")))
            })?;
        if bytes.len() > MAX_TORRENT_BYTES {
            return Err(Error::backend(
                "That torrent file is far too big to be real",
            ));
        }
        // Some indexers answer a magnet link as plain text.
        if bytes.starts_with(b"magnet:") {
            return Ok(Fetched::Magnet(
                String::from_utf8_lossy(&bytes).trim().to_string(),
            ));
        }
        parse(&bytes)?; // it must be a real torrent before anything is done with it
        return Ok(Fetched::File(bytes));
    }
    Err(Error::backend("The torrent link redirected too many times"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A multi-file torrent: d4:infod5:filesld6:lengthi100e4:pathl1:a5:x.zipeed6:lengthi50e4:pathl5:y.isoeee4:name4:Rootee
    fn sample() -> Vec<u8> {
        let mut b = b"d8:announce3:udp4:infod5:filesl".to_vec();
        b.extend(b"d6:lengthi100e4:pathl1:a5:x.zipee");
        b.extend(b"d6:lengthi50e4:pathl5:y.isoee");
        b.extend(b"e4:name4:Root12:piece lengthi16384e6:pieces20:");
        b.extend([7u8; 20]);
        b.extend(b"ee");
        b
    }

    #[test]
    fn reads_files_name_total_and_the_info_hash() {
        let t = parse(&sample()).unwrap();
        assert_eq!((t.name.as_str(), t.multi, t.total), ("Root", true, 150));
        assert_eq!(t.files.len(), 2);
        assert_eq!(
            (t.files[0].path.as_str(), t.files[0].size),
            ("a/x.zip", 100)
        );
        assert_eq!((t.files[1].index, t.files[1].path.as_str()), (1, "y.iso"));
        // The hash is that of the info dictionary's exact bytes.
        let b = sample();
        let a = b.windows(4).position(|w| w == b"info").unwrap() + 4;
        let z = b.len() - 1;
        let want: String = Sha1::digest(&b[a..z])
            .iter()
            .map(|x| format!("{x:02x}"))
            .collect();
        assert_eq!(t.info_hash, want);
        assert_eq!(t.info_hash.len(), 40);
    }

    #[test]
    fn a_single_file_torrent_is_one_file() {
        let mut b =
            b"d4:infod6:lengthi42e4:name8:game.iso12:piece lengthi16384e6:pieces0:ee".to_vec();
        let t = parse(&b).unwrap();
        assert!(!t.multi);
        assert_eq!(
            (t.files.len(), t.files[0].size, t.files[0].path.as_str()),
            (1, 42, "game.iso")
        );
        b.truncate(10);
        assert!(parse(&b).is_err());
    }

    #[test]
    fn hostile_input_is_refused_not_obeyed() {
        for bad in [
            &b""[..],
            b"d",
            b"i42e",
            b"d4:infoi5ee",
            b"d4:infod4:name1:xee",
            // a string claiming to be enormous
            b"d4:info99999999999:xe",
            // negative size
            b"d4:infod6:lengthi-5e4:name1:xee",
        ] {
            assert!(parse(bad).is_err(), "{:?}", String::from_utf8_lossy(bad));
        }
        // Deep nesting stops at a limit instead of exhausting the stack.
        let deep: Vec<u8> = std::iter::repeat_n(b'l', 5000).collect();
        let mut b = b"d4:info".to_vec();
        b.extend(deep);
        assert!(parse(&b).is_err());
        // `..` and slashes in names can't become paths.
        let mut t = b"d4:infod5:filesld6:lengthi1e4:pathl2:..4:a/b.eee4:name1:xee".to_vec();
        let parsed = parse(&t).unwrap();
        assert_eq!(parsed.files[0].path, "_/a_b.");
        t.clear();
    }

    #[test]
    fn magnets_give_the_hash_in_hex_or_base32() {
        let hex = "0123456789abcdef0123456789ABCDEF01234567";
        let (h, dn) = magnet(&format!(
            "magnet:?xt=urn:btih:{hex}&dn=Halo%203+USA&tr=udp%3A%2F%2Fx"
        ))
        .unwrap();
        assert_eq!(h, hex.to_lowercase());
        assert_eq!(dn.as_deref(), Some("Halo 3 USA"));
        // 20 bytes of 0x00 .. in base32 is 32 chars.
        let b32 = "A".repeat(32);
        assert_eq!(
            magnet(&format!("magnet:?xt=urn:btih:{b32}")).unwrap().0,
            "0".repeat(40)
        );
        assert!(magnet("http://x").is_none());
        assert!(magnet("magnet:?xt=urn:btih:short").is_none());
    }

    /// With a real torrent file: `RUSTYBOX_TEST_TORRENT=/path/x.torrent cargo test real_torrent -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_torrent_file() {
        let Ok(p) = std::env::var("RUSTYBOX_TEST_TORRENT") else {
            return;
        };
        let t0 = std::time::Instant::now();
        let t = parse(&std::fs::read(p).unwrap()).unwrap();
        println!(
            "{} files, {} bytes, hash {}, name {} ({:?})",
            t.files.len(),
            t.total,
            t.info_hash,
            t.name,
            t0.elapsed()
        );
        println!("first: {:?}", t.files.first());
    }
}
