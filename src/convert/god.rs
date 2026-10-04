//! ISO to GOD (using the `iso2god` library) and GOD back to ISO.

use std::{
    collections::VecDeque,
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

use iso2god::{
    executable::TitleInfo,
    god::{self, ContentType},
    iso,
};
use rayon::prelude::*;

use super::Ctl;
use crate::{error::Error, xbox::catalog};

fn be(e: impl std::fmt::Display) -> Error {
    Error::backend(format!("{e:#}"))
}

// ── ISO → GOD ────────────────────────────────────────────────────────────────

pub struct IsoToGod<'a> {
    pub iso: &'a Path,
    /// A folder to build the result in; it ends up holding `<TitleID>/<content type>/<container>`.
    pub stage: &'a Path,
    pub threads: usize,
    /// Leave out unused space at the end of the image.
    pub trim: bool,
}

#[derive(Debug, Clone)]
pub struct GodResult {
    pub title_id: String,
    pub media_id: String,
    pub content_type: String,
    pub name: Option<String>,
    /// The container file inside the staging folder, and its `.data` folder.
    pub container: PathBuf,
    pub data_dir: PathBuf,
    pub bytes: u64,
}

/// What converting an image would produce, without writing anything.
pub fn inspect_iso(iso_path: &Path) -> Result<(String, String, ContentType, u64), Error> {
    let file = File::open(iso_path).map_err(be)?;
    let len = file.metadata().map_err(be)?.len();
    let mut image = iso::IsoReader::read(file).map_err(be)?;
    let info = TitleInfo::from_image(&mut image).map_err(be)?;
    let e = info.execution_info;
    let used = image.get_max_used_prefix_size().min(len);
    Ok((
        format!("{:08X}", e.title_id),
        format!("{:08X}", e.media_id),
        info.content_type,
        used,
    ))
}

pub fn iso_to_god(o: &IsoToGod, ctl: &Ctl) -> Result<GodResult, Error> {
    let file = File::open(o.iso).map_err(be)?;
    let file_len = file.metadata().map_err(be)?.len();
    let mut source = iso::IsoReader::read(file).map_err(be)?;
    let info = TitleInfo::from_image(&mut source).map_err(be)?;
    let (exe_info, content_type) = (info.execution_info, info.content_type);

    let data_size = if o.trim {
        source.get_max_used_prefix_size()
    } else {
        file_len.saturating_sub(source.volume_descriptor.root_offset)
    };
    let block_count = data_size.div_ceil(god::BLOCK_SIZE);
    let part_count = block_count.div_ceil(god::BLOCKS_PER_PART);
    if part_count == 0 {
        return Err(Error::validation("The image has no game data"));
    }

    let layout = god::FileLayout::new(o.stage, &exe_info, content_type);
    fs::create_dir_all(layout.data_dir_path())?;
    let root_offset = source.volume_descriptor.root_offset;
    let title_id = format!("{:08X}", exe_info.title_id);
    ctl.check()?;
    (ctl.progress)(0.0, &format!("Writing {part_count} part file(s)"));

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(o.threads.max(1))
        .build()
        .map_err(be)?;
    let done = AtomicUsize::new(0);
    pool.install(|| {
        (0..part_count)
            .into_par_iter()
            .try_for_each(|part_index| -> Result<(), Error> {
                ctl.check()?;
                let mut volume = File::open(o.iso).map_err(be)?;
                volume.seek(SeekFrom::Start(root_offset)).map_err(be)?;
                let part_file = File::options()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(layout.part_file_path(part_index))
                    .map_err(be)?;
                god::write_part(volume, part_index, part_file).map_err(be)?;
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                // Writing the parts is nearly all of the work; the hash chain and header are quick.
                (ctl.progress)(
                    n as f32 / part_count as f32 * 0.95,
                    &format!("Part {n} of {part_count}"),
                );
                Ok(())
            })
    })?;

    ctl.check()?;
    (ctl.progress)(0.95, "Calculating the hash chain");
    let read_mht = |i: u64| -> Result<god::HashList, Error> {
        god::HashList::read(File::open(layout.part_file_path(i)).map_err(be)?).map_err(be)
    };
    let mut mht = read_mht(part_count - 1)?;
    for prev in (0..part_count - 1).rev() {
        let mut prev_mht = read_mht(prev)?;
        prev_mht.add_hash(&mht.digest());
        let mut f = File::options()
            .write(true)
            .open(layout.part_file_path(prev))
            .map_err(be)?;
        prev_mht.write(&mut f).map_err(be)?;
        mht = prev_mht;
    }

    let last_part_size = fs::metadata(layout.part_file_path(part_count - 1))?.len();
    let name = catalog::get()
        .name(&title_id, Some(&format!("{:08X}", exe_info.media_id)))
        .map(str::to_string);
    let mut header = god::ConHeaderBuilder::new()
        .with_execution_info(&exe_info)
        .with_block_counts(block_count as u32, 0)
        .with_data_parts_info(
            part_count as u32,
            last_part_size + (part_count - 1) * god::BLOCK_SIZE * 0xa290,
        )
        .with_content_type(content_type)
        .with_mht_hash(&mht.digest());
    if let Some(n) = &name {
        header = header.with_game_title(n);
    }
    let bytes_header = header.finalize();
    fs::write(layout.con_header_file_path(), bytes_header)?;
    (ctl.progress)(1.0, "Done");

    let container = layout.con_header_file_path();
    let data_dir = layout.data_dir_path();
    let bytes = dir_size(&data_dir) + fs::metadata(&container).map(|m| m.len()).unwrap_or(0);
    Ok(GodResult {
        title_id,
        media_id: format!("{:08X}", exe_info.media_id),
        content_type: format!("{:08X}", content_type as u32),
        name,
        container,
        data_dir,
        bytes,
    })
}

pub fn dir_size(dir: &Path) -> u64 {
    let Ok(rd) = fs::read_dir(dir) else { return 0 };
    rd.flatten()
        .map(|e| {
            e.metadata()
                .map(|m| {
                    if m.is_dir() {
                        dir_size(&e.path())
                    } else {
                        m.len()
                    }
                })
                .unwrap_or(0)
        })
        .sum()
}

// ── GOD → ISO ────────────────────────────────────────────────────────────────

/// Each subpart holds 0xcc blocks of 0x1000 bytes of data.
const SUBPART_DATA_SIZE: usize = 0xcc000;
/// A hash list sits between subparts.
const HASH_LIST_SIZE: usize = 0x1000;
/// Every part file starts with the master hash list and the first sub-hash list.
const PART_HEADER_SKIP: u64 = 0x2000;
/// The 64 KB start of a standard Xbox 360 ISO, for packages that don't carry their own.
const XSF_HEADER: &[u8] = include_bytes!("../../data/XSFHeader.bin");
const APP_NAME: &[u8] = b"RustyBox";

/// The part files (`Data0000`, `Data0001`, …) belonging to a container, in order.
pub fn data_parts(container: &Path) -> Result<Vec<PathBuf>, Error> {
    let mut data_dir = container.as_os_str().to_owned();
    data_dir.push(".data");
    let data_dir = PathBuf::from(data_dir);
    if !data_dir.is_dir() {
        return Err(Error::validation(format!(
            "The data folder is missing: {}",
            data_dir.display()
        )));
    }
    let mut parts = Vec::new();
    loop {
        let p = data_dir.join(format!("Data{:04}", parts.len()));
        if !p.exists() {
            break;
        }
        parts.push(p);
    }
    if parts.is_empty() {
        return Err(Error::validation(format!(
            "No Data0000 files found in {}",
            data_dir.display()
        )));
    }
    Ok(parts)
}

/// Where the disc's volume descriptor sits inside the GOD data, which tells us how to rebuild the image.
#[derive(Debug, PartialEq, Clone, Copy)]
enum Layout {
    /// At 0x10000: the data is already a whole image (the usual case); it is written as it is.
    Whole,
    /// At 0: the data starts at the descriptor, so the standard 64 KB image start is put in front.
    NeedsHeader,
}

const VOLUME_MAGIC: &[u8; 20] = b"MICROSOFT*XBOX*MEDIA";

fn detect_layout(first_part: &Path) -> Result<Layout, Error> {
    let f = File::open(first_part)?;
    let magic_at = |data_offset: u64| -> bool {
        let mut b = [0u8; 20];
        std::os::unix::fs::FileExt::read_exact_at(&f, &mut b, PART_HEADER_SKIP + data_offset)
            .is_ok()
            && &b == VOLUME_MAGIC
    };
    if magic_at(0x10000) {
        Ok(Layout::Whole)
    } else if magic_at(0) {
        Ok(Layout::NeedsHeader)
    } else {
        Err(Error::validation(
            "This package doesn't contain a recognisable Xbox game disc",
        ))
    }
}

pub fn god_to_iso(container: &Path, out_iso: &Path, ctl: &Ctl) -> Result<(), Error> {
    let parts = data_parts(container)?;
    let total: u64 = parts
        .iter()
        .filter_map(|p| fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();
    let layout = detect_layout(&parts[0])?;

    let mut done_bytes = 0u64;
    {
        let mut iso = BufWriter::with_capacity(1 << 20, File::create(out_iso)?);
        if layout == Layout::NeedsHeader {
            iso.write_all(XSF_HEADER)?;
        }
        let mut data = vec![0u8; SUBPART_DATA_SIZE];
        let mut hash = vec![0u8; HASH_LIST_SIZE];
        for (i, part) in parts.iter().enumerate() {
            let mut r = BufReader::with_capacity(1 << 20, File::open(part)?);
            r.seek(SeekFrom::Start(PART_HEADER_SKIP))?;
            loop {
                ctl.check()?;
                let n = read_up_to(&mut r, &mut data)?;
                if n == 0 {
                    break;
                }
                iso.write_all(&data[..n])?;
                done_bytes += n as u64;
                (ctl.progress)(
                    (done_bytes as f32 / total.max(1) as f32).min(1.0) * 0.98,
                    &format!("Part {} of {}", i + 1, parts.len()),
                );
                if n < SUBPART_DATA_SIZE {
                    break;
                }
                // Skip the hash list that follows each subpart.
                if read_up_to(&mut r, &mut hash)? < HASH_LIST_SIZE {
                    break;
                }
            }
        }
        iso.flush()?;
    }

    if layout == Layout::NeedsHeader {
        (ctl.progress)(0.98, "Fixing the image header");
        let mut f = OpenOptions::new().read(true).write(true).open(out_iso)?;
        fix_xsf_header(&mut f)?;
        fix_sector_offsets(&mut f, container)?;
    }
    (ctl.progress)(1.0, "Done");
    Ok(())
}

fn read_up_to<R: Read>(r: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    let mut total = 0;
    while total < buf.len() {
        match r.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(total)
}

/// Size fields and tool stamp in the header of an image built from a GOD package.
fn fix_xsf_header<F: Read + Write + Seek>(iso: &mut F) -> io::Result<()> {
    let len = iso.seek(SeekFrom::End(0))?;
    iso.seek(SeekFrom::Start(8))?;
    iso.write_all(&((len - 0x400) as i64).to_le_bytes())?;
    let sectors = (len / 2048) as u32;
    iso.seek(SeekFrom::Start(0x8050))?;
    iso.write_all(&sectors.to_le_bytes())?;
    iso.write_all(&sectors.to_be_bytes())?;
    iso.seek(SeekFrom::Start(0x7a69))?;
    iso.write_all(APP_NAME)?;
    Ok(())
}

/// Some packages record a sector offset that the image's directory tree must be corrected by.
fn fix_sector_offsets<F: Read + Write + Seek>(iso: &mut F, container: &Path) -> io::Result<()> {
    let god = fs::read(container)?;
    if god.get(0x391).copied().unwrap_or(0) & 0x40 != 0x40 || god.len() < 0x399 {
        return Ok(());
    }
    let raw = i32::from_le_bytes([god[0x395], god[0x396], god[0x397], god[0x398]]);
    if raw == 0 {
        return Ok(());
    }
    let offset = raw * 2 - 34;

    let mut b4 = [0u8; 4];
    iso.seek(SeekFrom::Start(0x10014))?;
    iso.read_exact(&mut b4)?;
    let root_sector = i32::from_le_bytes(b4);
    if root_sector <= 0 {
        return Ok(());
    }
    let corrected_root = root_sector - offset;
    iso.seek(SeekFrom::Start(0x10014))?;
    iso.write_all(&corrected_root.to_le_bytes())?;
    iso.read_exact(&mut b4)?;
    let root_size = i32::from_le_bytes(b4);

    let mut dirs: VecDeque<(i32, i32)> = VecDeque::from([(corrected_root, root_size)]);
    let mut visited = 0usize;
    while let Some((sector, size)) = dirs.pop_front() {
        visited += 1;
        if visited > 100_000 || sector < 0 || size < 0 {
            break; // a corrupt table must not loop forever
        }
        let start = sector as u64 * 2048;
        let end = start + size as u64;
        iso.seek(SeekFrom::Start(start))?;
        loop {
            let cur = iso.stream_position()?;
            if cur + 4 >= end {
                break;
            }
            if (cur + 4) / 2048 > cur / 2048 {
                iso.seek(SeekFrom::Start((cur / 2048 + 1) * 2048))?;
                continue;
            }
            iso.read_exact(&mut b4)?;
            if b4 == [0xff; 4] {
                let cur2 = iso.stream_position()?;
                if end - cur2 > 2048 {
                    iso.seek(SeekFrom::Start((cur2 / 2048 + 1) * 2048))?;
                    continue;
                }
                break;
            }
            iso.read_exact(&mut b4)?;
            let entry_sector = i32::from_le_bytes(b4);
            if entry_sector > 0 {
                iso.seek(SeekFrom::Current(-4))?;
                iso.write_all(&(entry_sector - offset).to_le_bytes())?;
            }
            iso.read_exact(&mut b4)?;
            let entry_size = i32::from_le_bytes(b4);
            let mut attr = [0u8; 1];
            iso.read_exact(&mut attr)?;
            if attr[0] & 0x10 == 0x10 {
                dirs.push_back((entry_sector - offset, entry_size));
            }
            iso.read_exact(&mut attr)?;
            let name_len = attr[0] as u64;
            iso.seek(SeekFrom::Current(name_len as i64))?;
            let record = 14 + name_len;
            if !record.is_multiple_of(4) {
                iso.seek(SeekFrom::Current((4 - record % 4) as i64))?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xbox::{ReadAt, xex, xiso};

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rustybox_conv_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    const NO_CANCEL: &(dyn Fn() -> bool + Sync) = &|| false;
    const NO_PROGRESS: &(dyn Fn(f32, &str) + Sync) = &|_, _| {};

    /// A disc image with a recognisable pattern after the executable, big enough for several GOD blocks.
    fn test_iso(path: &Path, title: u32, media: u32) -> Vec<u8> {
        let xex = xex::build(media, title, 1, 1);
        let mut bytes = xiso::build_simple_disc(&xex);
        bytes.extend((0..3_000_000u32).map(|i| (i % 251) as u8));
        // Pad to a whole number of sectors like a real image.
        while !bytes.len().is_multiple_of(2048) {
            bytes.push(0);
        }
        fs::write(path, &bytes).unwrap();
        bytes
    }

    #[test]
    fn iso_to_god_to_iso_keeps_the_game_intact() {
        let d = tmp("roundtrip");
        let iso = d.join("game.iso");
        let original = test_iso(&iso, 0x4D53_0805, 0x54E3_4DF4);

        let (title, media, _ct, used) = inspect_iso(&iso).unwrap();
        assert_eq!((title.as_str(), media.as_str()), ("4D530805", "54E34DF4"));
        assert!(used > 0);

        let ctl = Ctl {
            cancelled: NO_CANCEL,
            progress: NO_PROGRESS,
        };
        let stage = d.join("stage");
        let r = iso_to_god(
            &IsoToGod {
                iso: &iso,
                stage: &stage,
                threads: 2,
                trim: true,
            },
            &ctl,
        )
        .unwrap();
        assert_eq!(
            (
                r.title_id.as_str(),
                r.content_type.as_str(),
                r.media_id.as_str()
            ),
            ("4D530805", "00007000", "54E34DF4")
        );
        assert!(r.container.is_file() && r.data_dir.join("Data0000").is_file());
        assert_eq!(r.name.as_deref(), Some("Alan Wake"));
        // The container is a real STFS package naming the game.
        let h = crate::xbox::meta::read_god_dir(&stage.join("4D530805")).unwrap();
        assert_eq!(
            (h.title_id.as_str(), h.game_name.as_deref()),
            ("4D530805", Some("Alan Wake"))
        );

        let out = d.join("back.iso");
        god_to_iso(&r.container, &out, &ctl).unwrap();
        let back = fs::read(&out).unwrap();
        assert!(
            back.len() >= original.len() - 4096,
            "{} vs {}",
            back.len(),
            original.len()
        );

        // The rebuilt image is a readable disc with the same executable and the same data.
        let file = File::open(&out).unwrap();
        let vol = xiso::Volume::open(&file).unwrap().expect("a disc");
        let e = vol.find_in_root("default.xex").unwrap().unwrap();
        let xex_back = vol.read_file_prefix(&e, 4096).unwrap();
        assert_eq!(
            &xex_back[..0x200],
            &xex::build(0x54E3_4DF4, 0x4D53_0805, 1, 1)[..0x200]
        );
        // The pattern we appended survives byte for byte.
        let tail_at = original.len() - 3_000_000;
        let (mut a, mut b) = (vec![0u8; 1_000_000], vec![0u8; 1_000_000]);
        file.read_exact_at(&mut a, (tail_at + 500_000) as u64)
            .unwrap();
        b.copy_from_slice(&original[tail_at + 500_000..tail_at + 1_500_000]);
        assert_eq!(a, b);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn cancelling_stops_a_conversion() {
        let d = tmp("cancel");
        let iso = d.join("g.iso");
        test_iso(&iso, 0x4D53_0805, 1);
        let ctl = Ctl {
            cancelled: &|| true,
            progress: NO_PROGRESS,
        };
        let err = iso_to_god(
            &IsoToGod {
                iso: &iso,
                stage: &d.join("s"),
                threads: 1,
                trim: true,
            },
            &ctl,
        )
        .unwrap_err();
        assert!(err.to_string().contains("Cancelled"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_non_game_is_refused_with_a_reason() {
        let d = tmp("bad");
        fs::write(d.join("x.iso"), vec![0u8; 100_000]).unwrap();
        let ctl = Ctl {
            cancelled: NO_CANCEL,
            progress: NO_PROGRESS,
        };
        assert!(
            iso_to_god(
                &IsoToGod {
                    iso: &d.join("x.iso"),
                    stage: &d.join("s"),
                    threads: 1,
                    trim: true
                },
                &ctl
            )
            .is_err()
        );
        assert!(data_parts(&d.join("nothing")).is_err());
        let _ = fs::remove_dir_all(&d);
    }

    /// Hand-build a package whose data starts at the volume descriptor.
    #[test]
    fn data_that_starts_at_the_descriptor_gets_the_standard_header_in_front() {
        let d = tmp("prefix");
        let container = d.join("ABCDEF01");
        fs::write(&container, vec![0u8; 0x1000]).unwrap();
        fs::create_dir_all(d.join("ABCDEF01.data")).unwrap();
        let mut part = vec![0u8; PART_HEADER_SKIP as usize];
        let mut data = vec![0u8; 0x3000];
        data[..20].copy_from_slice(VOLUME_MAGIC);
        part.extend(&data);
        fs::write(d.join("ABCDEF01.data/Data0000"), &part).unwrap();

        let out = d.join("o.iso");
        god_to_iso(
            &container,
            &out,
            &Ctl {
                cancelled: NO_CANCEL,
                progress: NO_PROGRESS,
            },
        )
        .unwrap();
        let iso = fs::read(&out).unwrap();
        assert_eq!(iso.len(), 0x10000 + 0x3000);
        assert_eq!(&iso[0x10000..0x10014], VOLUME_MAGIC);
        // Neither layout: refused.
        fs::write(d.join("ABCDEF01.data/Data0000"), vec![0u8; 0x5000]).unwrap();
        assert!(
            god_to_iso(
                &container,
                &d.join("p.iso"),
                &Ctl {
                    cancelled: NO_CANCEL,
                    progress: NO_PROGRESS
                }
            )
            .is_err()
        );
        let _ = fs::remove_dir_all(&d);
    }
}
