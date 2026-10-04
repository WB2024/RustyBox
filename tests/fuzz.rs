//! Garbage in must never crash RustyBox: files, FTP listings, indexer feeds and agent answers
//! all come from outside. These feed random and mangled input to the parsers (a cheap fuzz).

use rustybox::{
    console::ftp,
    grabber::{newznab, release, torrent},
    xbox::{meta, stfs, xex},
};

/// A small deterministic generator, so a failure can be repeated.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
    fn text(&mut self, n: usize) -> String {
        const PIECES: &[&str] = &[
            "a", "Z", "0", "9", " ", "-", "_", ".", "/", "(", ")", "é", "İ", "ß", "日", "😀", "\t",
            "+", ":", ",", "[", "]", "disc", "part", "xbox", "360", "iso", "GOD", "Region",
        ];
        (0..n)
            .map(|_| PIECES[self.next() as usize % PIECES.len()])
            .collect()
    }
}

#[test]
fn xbox_file_parsers_survive_garbage() {
    let mut r = Rng(0x1234_5678_9abc_def1);
    for n in [0usize, 1, 3, 4, 16, 100, 0x344, 0x1800, 5000, 70_000] {
        for _ in 0..20 {
            let b = r.bytes(n);
            let _ = stfs::parse(&b);
            let _ = xex::parse(&b);
            let slice: &[u8] = &b;
            let _ = meta::read_iso(&slice);
        }
    }
    // Mangled headers: right magic, nonsense after it.
    for magic in [&b"CON "[..], b"LIVE", b"PIRS", b"XEX2"] {
        for _ in 0..50 {
            let mut b = magic.to_vec();
            b.extend(r.bytes(0x2000));
            let _ = stfs::parse(&b);
            let _ = xex::parse(&b);
        }
    }
}

#[test]
fn text_parsers_survive_garbage() {
    let mut r = Rng(42);
    for _ in 0..3000 {
        let n = (r.next() % 80) as usize;
        let s = r.text(n);
        let _ = ftp::parse_list_line(&s);
        let _ = newznab::parse_rfc2822(&s);
        let _ = release::parse(&s);
    }
    // Typical FTP listing lines, mangled.
    for line in [
        "drwxr-xr-x 1 root root 0 Jan 1 2020 Games",
        "-rw-r--r-- 1 root root 123 Jan  1 12:00 a file.txt",
        "d--------- 1 x x 0 Feb 30 25:61 é日",
        "total 5",
        "",
        "-rw-r--r-- 1 root root 99999999999999999999999 Jan 1 2020 x",
    ] {
        for cut in 0..line.chars().count() {
            let s: String = line.chars().take(cut).collect();
            let _ = ftp::parse_list_line(&s);
        }
    }
}

#[test]
fn torrent_files_and_magnet_links_survive_garbage() {
    let mut r = Rng(7);
    // Random bytes, and bytes that start like a torrent and then go wrong.
    for n in [0usize, 1, 5, 40, 500, 5000] {
        for _ in 0..200 {
            let _ = torrent::parse(&r.bytes(n));
            let mut b = b"d4:infod".to_vec();
            b.extend(r.bytes(n));
            let _ = torrent::parse(&b);
        }
    }
    // A valid torrent with each prefix cut off.
    let good = b"d8:announce3:udp4:infod5:filesld6:lengthi100e4:pathl1:a5:x.zipeee4:name4:Root12:piece lengthi16384e6:pieces20:AAAAAAAAAAAAAAAAAAAAee".to_vec();
    assert!(torrent::parse(&good).is_ok());
    for cut in 0..good.len() {
        let _ = torrent::parse(&good[..cut]);
    }
    for _ in 0..3000 {
        let n = (r.next() % 90) as usize;
        let s = format!(
            "magnet:?xt=urn:btih:{}&dn={}",
            r.text(n % 45),
            r.text(n % 30)
        );
        let _ = torrent::magnet(&s);
        let _ = torrent::magnet(&r.text(n));
    }
}
