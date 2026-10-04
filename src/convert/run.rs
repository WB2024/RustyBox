//! Carrying out a plan, one game at a time.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

use serde_json::json;

use super::{
    Ctl, god, image,
    plan::{Entry, Op, Plan},
};
use crate::{db::Db, error::Error, jobs::Job, library, perms};

pub struct RunOpts {
    pub threads: usize,
    pub overwrite: bool,
}

const STAGING: &str = ".rustybox-staging";

/// A working folder that is removed again however the work ends.
pub(crate) struct Stage(pub(crate) PathBuf);

impl Stage {
    pub(crate) fn create(root: &Path, name: &str) -> Result<Stage, Error> {
        let dir = root.join(STAGING).join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir)?;
        Ok(Stage(dir))
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
        // Remove the staging parent too once nothing else is using it.
        if let Some(parent) = self.0.parent() {
            let _ = fs::remove_dir(parent);
        }
    }
}

/// Staging folders left behind by a crash or a power cut are removed after a day.
fn clean_old_staging(root: &Path) {
    let Ok(rd) = fs::read_dir(root.join(STAGING)) else {
        return;
    };
    for e in rd.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age > Duration::from_secs(24 * 3600));
        if old {
            let _ = fs::remove_dir_all(e.path());
        }
    }
    let _ = fs::remove_dir(root.join(STAGING));
}

/// Move finished results from staging to their final places. Anything being replaced is set aside
/// first and put back if a later step fails, so a failure never loses the old copy.
fn move_into_place(
    pairs: &[(PathBuf, PathBuf)],
    overwrite: bool,
    stage: &Path,
) -> Result<(), Error> {
    let aside = stage.join("replaced");
    let mut set_aside: Vec<(PathBuf, PathBuf)> = Vec::new(); // (where it was, where it is now)
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new(); // (staged, final)

    let result = (|| -> Result<(), Error> {
        for (i, (_, dest)) in pairs.iter().enumerate() {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            if dest.exists() {
                if !overwrite {
                    return Err(Error::conflict(format!(
                        "{} already exists",
                        dest.display()
                    )));
                }
                fs::create_dir_all(&aside)?;
                let to = aside.join(i.to_string());
                fs::rename(dest, &to)?;
                set_aside.push((dest.clone(), to));
            }
        }
        for (src, dest) in pairs {
            fs::rename(src, dest).map_err(|e| {
                Error::backend(format!("Could not move the result into place: {e}"))
            })?;
            moved.push((src.clone(), dest.clone()));
        }
        Ok(())
    })();

    if result.is_err() {
        for (src, dest) in moved.iter().rev() {
            let _ = fs::rename(dest, src);
        }
        for (orig, now) in set_aside.iter().rev() {
            let _ = fs::rename(now, orig);
        }
        return result;
    }
    for (_, dest) in pairs {
        perms::own_parents(dest);
        perms::own_tree(dest);
    }
    Ok(())
}

fn exec_one(
    op: Op,
    e: &Entry,
    stage_root: &Path,
    name: &str,
    opts: &RunOpts,
    ctl: &Ctl,
) -> Result<(), Error> {
    let stage = Stage::create(stage_root, name)?;
    match op {
        Op::IsoToGod => {
            let r = god::iso_to_god(
                &god::IsoToGod {
                    iso: &e.source,
                    stage: &stage.0.join("god"),
                    threads: opts.threads,
                    trim: true,
                },
                ctl,
            )?;
            let data = e
                .output_data
                .clone()
                .ok_or_else(|| Error::backend("Internal error: no data folder planned"))?;
            move_into_place(
                &[(r.container, e.output.clone()), (r.data_dir, data)],
                opts.overwrite,
                &stage.0,
            )
        }
        Op::GodToIso => {
            let out = stage.0.join("out.iso");
            god::god_to_iso(&e.source, &out, ctl)?;
            move_into_place(&[(out, e.output.clone())], opts.overwrite, &stage.0)
        }
        Op::Extract => {
            let out = stage.0.join("out");
            image::unpack(&e.source, &out, ctl)?;
            move_into_place(&[(out, e.output.clone())], false, &stage.0)
        }
        Op::Create => {
            let out = stage.0.join("out.iso");
            image::pack(&e.source, &out, ctl)?;
            move_into_place(&[(out, e.output.clone())], opts.overwrite, &stage.0)
        }
    }
}

/// The job body. Games that fail don't stop the others; the job fails at the end if any did.
pub async fn run(
    db: Db,
    job: Arc<Job>,
    plan: Plan,
    dest_library_id: i64,
    opts: RunOpts,
) -> Result<(), Error> {
    let root = plan.dest_root.clone();
    let n = plan.entries.len();
    let r = root.clone();
    tokio::task::spawn_blocking(move || clean_old_staging(&r))
        .await
        .ok();

    let mut failures: Vec<String> = Vec::new();
    let mut written = 0u64;
    let mut outputs = Vec::new();
    let opts = Arc::new(opts);
    for (i, entry) in plan.entries.iter().enumerate() {
        if job.is_cancelled() {
            return Ok(());
        }
        job.step(format!("{} of {n}: {}", i + 1, entry.name));
        let (j, token) = (job.clone(), job.cancel_token());
        let (e, op, root2, o) = (entry.clone(), plan.op, root.clone(), opts.clone());
        let name = format!("{}-{i}", job.id);
        let res = tokio::task::spawn_blocking(move || {
            let cancelled = || token.is_cancelled();
            let progress = |f: f32, what: &str| {
                j.progress((i as f32 + f.clamp(0.0, 1.0)) / n as f32 * 100.0 * 0.97);
                if !what.is_empty() && !what.starts_with("Part ") {
                    j.step(format!("{} of {n}: {} — {what}", i + 1, e.name));
                }
            };
            exec_one(
                op,
                &e,
                &root2,
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
            Ok(()) => {
                written += entry.bytes_out;
                outputs.push(entry.output.to_string_lossy().to_string());
                job.log(format!("Finished {}", entry.output.display()));
            }
            Err(e) if job.is_cancelled() => {
                let _ = e;
                return Ok(());
            }
            Err(e) => {
                job.log(format!("{} failed: {e}", entry.name));
                failures.push(format!("{}: {e}", entry.name));
            }
        }
    }

    // Make the new items show up.
    job.step("Updating the library");
    if let Ok(lib) = db
        .run(move |c| library::get_library(c, dest_library_id))
        .await
    {
        library::scan::scan_library(db.clone(), job.clone(), lib).await?;
    }
    job.result("convert", json!({"op": plan.op_label, "done": outputs.len(), "failed": failures.len(), "bytes": written, "outputs": outputs}));
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
