//! Unpacking a disc image into a folder, and packing a folder into a disc image, using the
//! `xdvdfs` library.

use std::{
    fs::{self, File},
    io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::Path,
};

use xdvdfs::{
    blockdev::{BlockDeviceWrite, OffsetWrapper},
    write::{
        fs::StdFilesystem,
        img::{ProgressInfo, create_xdvdfs_image},
    },
};

use super::Ctl;
use crate::error::Error;

fn be(e: impl std::fmt::Display) -> Error {
    Error::backend(e.to_string())
}

fn open_image(path: &Path) -> Result<OffsetWrapper<BufReader<File>, io::Error>, Error> {
    let f = BufReader::new(File::open(path)?);
    OffsetWrapper::new(f).map_err(|e| Error::validation(format!("Not an Xbox disc image: {e}")))
}

/// A name from inside an image is used as a file name, so it must not be able to leave the folder.
fn safe_component(name: &str) -> Result<&str, Error> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', '\0']) {
        return Err(Error::validation(format!(
            "The image contains an unsafe file name: {name:?}"
        )));
    }
    Ok(name)
}

/// (files, bytes) an image would unpack to.
pub fn unpack_size(iso: &Path) -> Result<(usize, u64), Error> {
    let mut img = open_image(iso)?;
    let volume = xdvdfs::read::read_volume(&mut img)
        .map_err(|e| Error::validation(format!("Not an Xbox disc image: {e}")))?;
    let tree = volume.root_table.file_tree(&mut img).map_err(be)?;
    let files: Vec<_> = tree
        .iter()
        .filter(|(_, d)| !d.node.dirent.is_directory())
        .collect();
    Ok((
        files.len(),
        files
            .iter()
            .map(|(_, d)| d.node.dirent.data.size as u64)
            .sum(),
    ))
}

/// Unpack `iso` into `dest` (which must not exist yet; its parents are created).
pub fn unpack(iso: &Path, dest: &Path, ctl: &Ctl) -> Result<(), Error> {
    let mut img = open_image(iso)?;
    let volume = xdvdfs::read::read_volume(&mut img)
        .map_err(|e| Error::validation(format!("Not an Xbox disc image: {e}")))?;
    let tree = volume.root_table.file_tree(&mut img).map_err(be)?;
    let total: u64 = tree
        .iter()
        .filter(|(_, d)| !d.node.dirent.is_directory())
        .map(|(_, d)| d.node.dirent.data.size as u64)
        .sum();
    fs::create_dir_all(dest)?;
    let mut done = 0u64;
    for (dir, entry) in &tree {
        ctl.check()?;
        let name = entry.name_str::<io::Error>().map_err(be)?.to_string();
        let name = safe_component(&name)?;
        let mut folder = dest.to_path_buf();
        for part in dir.split('/').filter(|p| !p.is_empty()) {
            folder.push(safe_component(part)?);
        }
        fs::create_dir_all(&folder)?;
        let target = folder.join(name);
        if entry.node.dirent.is_directory() {
            fs::create_dir_all(&target)?;
            continue;
        }
        let mut out = File::create(&target)?;
        if entry.node.dirent.is_empty() {
            continue;
        }
        entry.node.dirent.seek_to(&mut img).map_err(be)?;
        let mut remaining = entry.node.dirent.data.size as u64;
        let mut buf = vec![0u8; 1 << 20];
        let reader = img.get_mut();
        while remaining > 0 {
            ctl.check()?;
            let n = (remaining as usize).min(buf.len());
            reader.read_exact(&mut buf[..n])?;
            out.write_all(&buf[..n])?;
            remaining -= n as u64;
            done += n as u64;
            (ctl.progress)(done as f32 / total.max(1) as f32, name);
        }
    }
    (ctl.progress)(1.0, "Done");
    Ok(())
}

/// The output file, which gives up (with an error the pack can't carry on past) once cancelled.
struct CancellableFile<'a> {
    inner: BufWriter<File>,
    cancelled: &'a (dyn Fn() -> bool + Sync),
}

impl Write for CancellableFile<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if (self.cancelled)() {
            return Err(io::Error::other("cancelled"));
        }
        Write::write(&mut self.inner, buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl Seek for CancellableFile<'_> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

// The `sync` feature of xdvdfs turns the trait's async methods into plain ones.
impl BlockDeviceWrite<io::Error> for CancellableFile<'_> {
    fn write(&mut self, offset: u64, buffer: &[u8]) -> Result<(), io::Error> {
        Seek::seek(self, SeekFrom::Start(offset))?;
        Write::write_all(self, buffer)
    }

    fn len(&mut self) -> Result<u64, io::Error> {
        Ok(self.inner.get_mut().metadata()?.len())
    }
}

/// Pack the folder `src` into a new image at `out_iso`.
pub fn pack(src: &Path, out_iso: &Path, ctl: &Ctl) -> Result<(), Error> {
    if !src.is_dir() {
        return Err(Error::validation("The source isn't a folder"));
    }
    let image = File::options().write(true).create_new(true).open(out_iso)?;
    let mut out = CancellableFile {
        inner: BufWriter::with_capacity(1 << 20, image),
        cancelled: ctl.cancelled,
    };
    let mut fs_src = StdFilesystem::create(src);
    let (mut files, mut done) = (0usize, 0usize);
    let result = create_xdvdfs_image(&mut fs_src, &mut out, |p| match p {
        ProgressInfo::FileCount(n) => files = n,
        ProgressInfo::FileAdded(name, _) => {
            done += 1;
            (ctl.progress)(done as f32 / files.max(1) as f32 * 0.98, &name);
        }
        _ => {}
    });
    match result {
        Ok(()) => {
            out.flush()?;
            (ctl.progress)(1.0, "Done");
            Ok(())
        }
        Err(_) if (ctl.cancelled)() => Err(Error::backend("Cancelled")),
        Err(e) => Err(be(format!("Could not build the image: {e:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xbox::{xex, xiso};

    const NO_CANCEL: &(dyn Fn() -> bool + Sync) = &|| false;
    const NO_PROGRESS: &(dyn Fn(f32, &str) + Sync) = &|_, _| {};

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("rustybox_img_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_folder_packs_into_an_image_that_unpacks_to_the_same_files() {
        let d = tmp("roundtrip");
        let src = d.join("game");
        fs::create_dir_all(src.join("Media/Video")).unwrap();
        let xex_bytes = xex::build(0x1111_2222, 0x4D53_0805, 1, 1);
        fs::write(src.join("default.xex"), &xex_bytes).unwrap();
        fs::write(src.join("Media/Video/intro.bik"), vec![9u8; 300_000]).unwrap();
        fs::write(src.join("Media/readme.txt"), b"hello").unwrap();
        fs::write(src.join("empty.bin"), b"").unwrap();

        let ctl = Ctl {
            cancelled: NO_CANCEL,
            progress: NO_PROGRESS,
        };
        let iso = d.join("game.iso");
        pack(&src, &iso, &ctl).unwrap();
        assert!(
            pack(&src, &iso, &ctl).is_err(),
            "never overwrites an existing image"
        );

        // Our own reader recognises it and reads the game's identity.
        let m = crate::xbox::meta::read_iso_file(&iso).unwrap();
        assert_eq!(
            (m.title_id.as_str(), m.media_id.as_deref()),
            ("4D530805", Some("11112222"))
        );

        assert_eq!(
            unpack_size(&iso).unwrap(),
            (4, 300_000 + 5 + xex_bytes.len() as u64)
        );
        let out = d.join("out");
        unpack(&iso, &out, &ctl).unwrap();
        assert_eq!(fs::read(out.join("default.xex")).unwrap(), xex_bytes);
        assert_eq!(
            fs::read(out.join("Media/Video/intro.bik")).unwrap(),
            vec![9u8; 300_000]
        );
        assert_eq!(fs::read(out.join("Media/readme.txt")).unwrap(), b"hello");
        assert_eq!(fs::metadata(out.join("empty.bin")).unwrap().len(), 0);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn unpacking_something_that_is_not_a_disc_fails_clearly() {
        let d = tmp("bad");
        fs::write(d.join("x.iso"), vec![1u8; 200_000]).unwrap();
        let ctl = Ctl {
            cancelled: NO_CANCEL,
            progress: NO_PROGRESS,
        };
        let err = unpack(&d.join("x.iso"), &d.join("o"), &ctl).unwrap_err();
        assert!(err.to_string().contains("Not an Xbox disc image"), "{err}");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn it_reads_the_discs_our_own_builder_makes_and_names_stay_inside_the_folder() {
        assert!(
            safe_component("../x").is_err()
                && safe_component("a/b").is_err()
                && safe_component("..").is_err()
        );
        assert_eq!(safe_component("default.xex").unwrap(), "default.xex");
        let disc = xiso::build_simple_disc(&xex::build(1, 0x4D53_0805, 1, 1));
        let d = tmp("simple");
        fs::write(d.join("s.iso"), disc).unwrap();
        let ctl = Ctl {
            cancelled: NO_CANCEL,
            progress: NO_PROGRESS,
        };
        unpack(&d.join("s.iso"), &d.join("o"), &ctl).unwrap();
        assert!(d.join("o/default.xex").is_file());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn cancelling_a_pack_stops_it() {
        let d = tmp("cancel");
        let src = d.join("g");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("default.xex"), xex::build(1, 2, 1, 1)).unwrap();
        let ctl = Ctl {
            cancelled: &|| true,
            progress: NO_PROGRESS,
        };
        let err = pack(&src, &d.join("g.iso"), &ctl).unwrap_err();
        assert!(err.to_string().contains("Cancelled"), "{err}");
        let _ = fs::remove_dir_all(&d);
    }
}
