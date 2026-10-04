//! Carrying out an import plan, one game at a time.

use std::{path::PathBuf, sync::Arc};

use serde_json::json;

use super::{
    engine::{self, LinkKind, Moved, Progress},
    loc::Loc,
    plan::{Mode, Plan, Removal, Work},
};
use crate::{
    convert::{Ctl, god, run::Stage},
    db::Db,
    error::Error,
    jobs::Job,
    library,
};

pub struct RunOpts {
    pub threads: usize,
    pub overwrite: bool,
    /// Where to prepare conversions whose result goes to a drive on another computer.
    pub stage_root: PathBuf,
}

/// The folders inside a title folder that hold the game itself. Everything else (saved games,
/// add-ons, title updates, profiles) is somebody's data and is never removed by a replace.
pub(crate) const GAME_CONTENT: [&str; 4] = ["00007000", "00005000", "000D0000", "00004000"];

/// Remove an old copy of a game. Returns a note for the log.
pub fn remove_old(r: &Removal) -> Result<String, Error> {
    if r.rel.trim_matches('/').is_empty() {
        return Err(Error::validation("Refusing to remove the top folder"));
    }
    if r.kind != "god" {
        r.loc.delete(&r.rel)?;
        return Ok(format!("removed {}", r.rel));
    }
    let tree = r.loc.tree(&r.rel)?;
    let mut kept = Vec::new();
    for e in tree.iter().filter(|e| e.is_dir && !e.rel.contains('/')) {
        if GAME_CONTENT.iter().any(|g| g.eq_ignore_ascii_case(&e.rel)) {
            r.loc
                .delete(&format!("{}/{}", r.rel.trim_matches('/'), e.rel))?;
        } else {
            kept.push(e.rel.clone());
        }
    }
    // Loose files directly inside the title folder are left too.
    let left = r.loc.tree(&r.rel)?;
    if left.iter().all(|e| e.is_dir) {
        r.loc.delete(&r.rel)?;
        // The "Game Name" folder above it, if that is now empty.
        if let Some((parent, _)) = r.rel.rsplit_once('/')
            && r.loc.tree(parent).is_ok_and(|t| t.iter().all(|e| e.is_dir))
        {
            r.loc.delete(parent)?;
        }
        Ok(format!("removed {}", r.rel))
    } else {
        Ok(format!(
            "removed the game from {} and kept {}",
            r.rel,
            if kept.is_empty() {
                "the other files in it".to_string()
            } else {
                kept.join(", ")
            }
        ))
    }
}

fn remove_old_copies(
    w: &Work,
    also: bool,
    before: bool,
    log: &mut Vec<String>,
) -> Result<(), Error> {
    for r in w
        .removals
        .iter()
        .filter(|r| r.also == also && r.before == before)
    {
        log.push(format!("{}: {}", r.label, remove_old(r)?));
    }
    Ok(())
}

/// Run one game's steps. `ctl.progress` gets the fraction of this game that is done.
fn exec(w: &Work, name: &str, opts: &RunOpts, ctl: &Ctl) -> Result<String, Error> {
    let main_share = if w.also.is_some() { 0.85 } else { 1.0 };
    let scaled = |lo: f32, hi: f32| {
        move |f: f32, what: &str| (ctl.progress)(lo + (hi - lo) * f.clamp(0.0, 1.0), what)
    };
    let mut how: String;
    let mut replaced: Vec<String> = Vec::new();
    remove_old_copies(w, false, true, &mut replaced)?;

    if w.convert {
        // Prepare in a hidden folder: next to the destination if it is here, else in RustyBox's own.
        let base = match &w.dst {
            Loc::Local(root) => root.clone(),
            Loc::Remote(_) => opts.stage_root.clone(),
        };
        let stage = Stage::create(&base, &format!("import-{name}"))?;
        let iso_path = match &w.src {
            Loc::Local(root) => crate::fsops::contained(root, &w.src_rel)?,
            Loc::Remote(_) => {
                let local = Loc::Local(stage.0.clone());
                let file = w.src_rel.rsplit('/').next().unwrap_or("image.iso");
                let p = scaled(0.0, 0.2);
                let c2 = Ctl {
                    cancelled: ctl.cancelled,
                    progress: &p,
                };
                engine::copy_tree(
                    &w.src,
                    &w.src_rel,
                    &local,
                    &format!("iso/{file}"),
                    true,
                    &Progress {
                        ctl: &c2,
                        total: 1,
                        base: 0,
                        label: "Copying the ISO from the drive",
                    },
                )?;
                stage.0.join("iso").join(file)
            }
        };
        let p = scaled(0.2, 0.7);
        let c2 = Ctl {
            cancelled: ctl.cancelled,
            progress: &p,
        };
        let r = god::iso_to_god(
            &god::IsoToGod {
                iso: &iso_path,
                stage: &stage.0.join("god"),
                threads: opts.threads,
                trim: true,
            },
            &c2,
        )?;
        let p = scaled(0.7, main_share);
        let c3 = Ctl {
            cancelled: ctl.cancelled,
            progress: &p,
        };
        let moved = engine::move_tree(
            &Loc::Local(stage.0.clone()),
            &format!("god/{}", r.title_id),
            &w.dst,
            &w.dst_rel,
            opts.overwrite,
            &Progress {
                ctl: &c3,
                total: 1,
                base: 0,
                label: "Placing the converted game",
            },
        )?;
        how = format!(
            "converted ({})",
            if moved == Moved::Renamed {
                "placed instantly"
            } else {
                "copied into place"
            }
        );
        // The ISO itself is only removed once the converted game is safely in place.
        if w.mode == Mode::Move {
            w.src.delete(&w.src_rel)?;
            how.push_str(", ISO removed");
        }
    } else {
        let p = scaled(0.0, main_share);
        let c2 = Ctl {
            cancelled: ctl.cancelled,
            progress: &p,
        };
        let prog = Progress {
            ctl: &c2,
            total: 1,
            base: 0,
            label: name,
        };
        match w.mode {
            Mode::Copy => {
                let s = engine::copy_tree(
                    &w.src,
                    &w.src_rel,
                    &w.dst,
                    &w.dst_rel,
                    opts.overwrite,
                    &prog,
                )?;
                how = format!(
                    "copied {} file(s){}",
                    s.files,
                    if s.skipped > 0 {
                        format!(", {} already there", s.skipped)
                    } else {
                        String::new()
                    }
                );
            }
            Mode::Move => {
                let m = engine::move_tree(
                    &w.src,
                    &w.src_rel,
                    &w.dst,
                    &w.dst_rel,
                    opts.overwrite,
                    &prog,
                )?;
                how = if m == Moved::Renamed {
                    "moved (instant)".into()
                } else {
                    "moved (copied, checked, original removed)".into()
                };
            }
            Mode::Hardlink | Mode::Symlink => {
                let (Loc::Local(sr), Loc::Local(dr)) = (&w.src, &w.dst) else {
                    return Err(Error::validation(
                        "Links only work between folders on this server",
                    ));
                };
                let kind = if w.mode == Mode::Hardlink {
                    LinkKind::Hard
                } else {
                    LinkKind::Soft
                };
                let s =
                    engine::link_tree(sr, &w.src_rel, dr, &w.dst_rel, kind, opts.overwrite, &c2)?;
                how = format!(
                    "{} {} file(s)",
                    if kind == LinkKind::Hard {
                        "hard-linked"
                    } else {
                        "symlinked"
                    },
                    s.files + s.skipped
                );
            }
        }
    }

    remove_old_copies(w, false, false, &mut replaced)?;
    if let Some((aloc, arel, _)) = &w.also {
        remove_old_copies(w, true, true, &mut replaced)?;
        let p = scaled(main_share, 1.0);
        let c2 = Ctl {
            cancelled: ctl.cancelled,
            progress: &p,
        };
        let s = engine::copy_tree(
            &w.dst,
            &w.dst_rel,
            aloc,
            arel,
            opts.overwrite,
            &Progress {
                ctl: &c2,
                total: 1,
                base: 0,
                label: "Copying to the second place",
            },
        )?;
        how.push_str(&format!("; copied on ({} file(s))", s.files));
        remove_old_copies(w, true, false, &mut replaced)?;
    }
    if !replaced.is_empty() {
        how.push_str(&format!("; replaced: {}", replaced.join("; ")));
    }
    Ok(how)
}

/// The job body. A game that fails doesn't stop the others; the job fails at the end if any did.
pub async fn run(db: Db, job: Arc<Job>, plan: Plan, opts: RunOpts) -> Result<(), Error> {
    let n = plan.entries.len();
    let opts = Arc::new(opts);
    let mut failures: Vec<String> = Vec::new();
    let mut done = 0usize;
    let mut libraries: Vec<i64> = Vec::new();
    let mut note = |id: i64| {
        if !libraries.contains(&id) {
            libraries.push(id);
        }
    };

    for (i, entry) in plan.entries.iter().enumerate() {
        if job.is_cancelled() {
            return Ok(());
        }
        let Some(work) = entry.work.clone() else {
            continue;
        };
        job.step(format!("{} of {n}: {}", i + 1, entry.name));
        let (j, token, o) = (job.clone(), job.cancel_token(), opts.clone());
        let (name, label) = (format!("{}-{i}", job.id), entry.name.clone());
        let w2 = work.clone();
        let res = tokio::task::spawn_blocking(move || {
            let cancelled = || token.is_cancelled();
            let progress = |f: f32, what: &str| {
                j.progress((i as f32 + f.clamp(0.0, 1.0)) / n as f32 * 97.0);
                if !what.is_empty() && what != label {
                    j.step(format!("{} of {n}: {label}: {what}", i + 1));
                }
            };
            exec(
                &w2,
                &name,
                &o,
                &Ctl {
                    cancelled: &cancelled,
                    progress: &progress,
                },
            )
        })
        .await
        .map_err(|e| Error::backend(e.to_string()))?;
        match res {
            Ok(how) => {
                done += 1;
                job.log(format!("{}: {how}", entry.name));
                note(work.dst_library);
                if let Some((_, _, lib)) = &work.also {
                    note(*lib);
                }
                if let Some(l) = work.src_library
                    && (work.mode == Mode::Move)
                {
                    note(l);
                }
            }
            Err(_) if job.is_cancelled() => return Ok(()),
            Err(e) => {
                job.log(format!("{} failed: {e}", entry.name));
                failures.push(format!("{}: {e}", entry.name));
            }
        }
    }

    // Make what changed show up.
    for lib_id in libraries {
        job.step("Updating the library");
        if let Ok(lib) = db.run(move |c| library::get_library(c, lib_id)).await {
            library::scan::scan_library(db.clone(), job.clone(), lib).await?;
        }
    }
    job.result(
        "import",
        json!({"done": done, "failed": failures.len(), "total": n}),
    );
    if failures.is_empty() {
        Ok(())
    } else {
        Err(Error::backend(format!(
            "{} of {n} failed:\n{}",
            failures.len(),
            failures.join("\n")
        )))
    }
}
