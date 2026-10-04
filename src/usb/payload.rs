//! The exploit's payload: XeUnshackle, fetched from its GitHub release on request. The Bad Avatar
//! package doesn't include a payload, and its zip lays out `BadUpdatePayload/default.xex`,
//! `launch.ini` and a few helper programs the way they go on the stick.
//!
//! Only this one pinned release address is ever downloaded (a test can point it elsewhere), and
//! the zip is unpacked with the same path checks as everything else.

use std::{fs, io::Read, path::Path, time::Duration};

use crate::{convert::Ctl, error::Error};

pub const XEUNSHACKLE_URL: &str =
    "https://github.com/Byrom90/XeUnshackle/releases/download/v1.03/XeUnshackle-BETA-v1_03.zip";
pub const XEUNSHACKLE_NAME: &str = "XeUnshackle BETA v1.03";

fn url() -> String {
    std::env::var("RUSTYBOX_XEUNSHACKLE_URL").unwrap_or_else(|_| XEUNSHACKLE_URL.into())
}

/// Copy every file of `from` into `to` (replacing files with the same name).
fn merge(from: &Path, to: &Path) -> Result<usize, Error> {
    let mut n = 0;
    let mut stack = vec![from.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d)?.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let rel = p
                .strip_prefix(from)
                .map_err(|e| Error::backend(e.to_string()))?;
            let dest = to.join(rel);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            let part = dest.with_extension("part");
            fs::copy(&p, &part)?;
            fs::rename(&part, &dest)?;
            n += 1;
        }
    }
    Ok(n)
}

/// Download the release and merge it into the package folder. Returns the files placed.
pub fn fetch_xeunshackle(package: &Path, staging: &Path, ctl: &Ctl) -> Result<usize, Error> {
    let _ = fs::remove_dir_all(staging);
    fs::create_dir_all(staging)?;
    let result = (|| -> Result<usize, Error> {
        (ctl.progress)(0.05, "Downloading XeUnshackle");
        let zip_path = staging.join("payload.zip");
        let url = url();
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(15))
            .timeout_read(Duration::from_secs(120))
            .redirects(5)
            .user_agent("RustyBox")
            .build();
        let r = agent
            .get(&url)
            .call()
            .map_err(|e| Error::backend(format!("Couldn't download XeUnshackle: {e}")))?;
        let mut data = Vec::new();
        r.into_reader()
            .take(40 * 1024 * 1024)
            .read_to_end(&mut data)
            .map_err(|e| Error::backend(format!("Couldn't read the download: {e}")))?;
        ctl.check()?;
        fs::write(&zip_path, &data)?;
        (ctl.progress)(0.5, "Unpacking");
        let out = staging.join("unpacked");
        let files = crate::content::install::unzip(&zip_path, &out)?;
        if !files
            .iter()
            .any(|f| f.ends_with("BadUpdatePayload/default.xex"))
        {
            return Err(Error::backend(
                "That download doesn't contain BadUpdatePayload/default.xex, so it isn't the payload RustyBox expects",
            ));
        }
        // The zip has one folder at the top; its contents are what goes on the stick.
        let tops: Vec<_> = fs::read_dir(&out)?.flatten().collect();
        let root = if tops.len() == 1 && tops[0].path().is_dir() {
            tops[0].path()
        } else {
            out.clone()
        };
        (ctl.progress)(0.8, "Adding it to the package");
        let n = merge(&root, package)?;
        (ctl.progress)(1.0, "");
        Ok(n)
    })();
    let _ = fs::remove_dir_all(staging);
    result
}
