//! Sample data for `--mock`: a small fake tree of ISOs and GOD folders, set up as libraries on
//! first start, so the UI can be explored with no real disks.

use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::{
    db::Db,
    error::Error,
    library::{self, NewPath},
    xbox::{stfs, xex, xiso},
};

/// Where the sample files live (also added to the folders libraries may use).
pub fn data_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("mock-data")
}

/// A valid disc image for a title (partition at 0), padded to a plausible-looking size.
fn iso(title: u32, media: u32, disc: u8, discs: u8, pad: usize) -> Vec<u8> {
    let mut b = xiso::build_simple_disc(&xex::build(media, title, disc, discs));
    b.resize(b.len() + pad, 0x58);
    b
}

fn god(dir: &Path, title: u32, name: &str, pad: usize) -> Result<(), Error> {
    let t = format!("{title:08X}");
    let ct = dir.join(&t).join("00007000");
    fs::create_dir_all(ct.join("A1B2C3D4E5.data"))?;
    fs::write(
        ct.join("A1B2C3D4E5"),
        stfs::build(b"LIVE", 0x7000, title, 0, name, name),
    )?;
    fs::write(ct.join("A1B2C3D4E5.data/Data0000"), vec![0x58u8; pad])?;
    Ok(())
}

pub fn seed(config_dir: &Path, db: &Db) -> Result<(), Error> {
    let root = data_dir(config_dir);
    let write = |rel: &str, bytes: Vec<u8>| -> Result<(), Error> {
        let p = root.join(rel);
        if !p.exists() {
            fs::create_dir_all(p.parent().unwrap())?;
            fs::write(p, bytes)?;
        }
        Ok(())
    };
    // Real title IDs, so names come from the title list. Gears of War is on two disks (a duplicate),
    // Fable II is both an ISO and a GOD folder, one file is not a game at all, and one title is unknown.
    write(
        "iso-disk-a/Gears of War (Europe).iso",
        iso(0x4D53_07D5, 0x76E9_DF5B, 1, 1, 450_000),
    )?;
    write(
        "iso-disk-a/Forza Motorsport 4.iso",
        iso(0x4D53_0910, 0x61D6_2D9F, 1, 2, 600_000),
    )?;
    write(
        "iso-disk-a/Fable II (USA).iso",
        iso(0x4D53_07F1, 0x6339_E4E9, 1, 1, 250_000),
    )?;
    write("iso-disk-a/not-a-game.iso", vec![7u8; 4096])?;
    write(
        "iso-disk-b/Gears of War copy.iso",
        iso(0x4D53_07D5, 0x76E9_DF5B, 1, 1, 450_000),
    )?;
    god(
        &root.join("god/Alan Wake"),
        0x4D53_0805,
        "Alan Wake",
        400_000,
    )?;
    god(&root.join("god/Fable II"), 0x4D53_07F1, "Fable II", 300_000)?;
    god(&root.join("god/Mystery Game"), 0xABCD_EF12, "", 100_000)?;
    write("mods/Skyrim Better Textures.zip", vec![0x58u8; 120_000])?;
    db.with(|c| {
        if !library::list_libraries(c)?.is_empty() {
            return Ok(());
        }
        let p = |sub: &str, label: &str, writable| -> Result<(PathBuf, NewPath), Error> {
            let canon = fs::canonicalize(root.join(sub))?;
            Ok((
                canon,
                NewPath {
                    path: sub.into(),
                    label: label.into(),
                    writable,
                    remote: None,
                    role: String::new(),
                },
            ))
        };
        library::create_library(
            c,
            "ISOs",
            "iso",
            &[
                p("iso-disk-a", "Disk A", true)?,
                p("iso-disk-b", "Disk B", false)?,
            ],
        )?;
        library::create_library(c, "GOD games", "god", &[p("god", "GOD disk", true)?])?;
        library::create_library(c, "Mods", "mods", &[p("mods", "Mods", true)?])?;
        Ok(())
    })
}
