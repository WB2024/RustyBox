//! USB API: which sticks are safe to touch, formatting one, and building a Bad Avatar stick.
//! Every action re-checks the device against the live list and needs its path typed back.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ApiError, ApiResult, AppState, S};
use crate::{
    convert::Ctl,
    error::Error,
    usb::{self, Device, badavatar},
};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/usb/devices", get(devices))
        .route("/api/usb/badavatar/source", get(source))
        .route(
            "/api/usb/badavatar/file",
            axum::routing::put(put_file)
                .layer(axum::extract::DefaultBodyLimit::max(512 * 1024 * 1024)),
        )
        .route(
            "/api/usb/badavatar/package",
            axum::routing::delete(delete_package),
        )
        .route("/api/usb/badavatar/payload", post(fetch_payload))
        .route("/api/usb/badavatar/plan", post(plan))
        .route("/api/usb/badavatar/start", post(start))
        .route("/api/usb/format", post(format))
        .route("/api/usb/backups", get(backups).delete(delete_backup))
        .route("/api/usb/backup", post(backup))
        .route("/api/usb/restore", post(restore))
}

async fn all_devices(st: &AppState) -> Result<Vec<Device>, Error> {
    if st.cfg.mock {
        return Ok(usb::mock_devices(&st.cfg.config_dir));
    }
    let protect = vec![st.cfg.config_dir.clone()];
    tokio::task::spawn_blocking(move || usb::detect(&protect))
        .await
        .map_err(|e| Error::backend(e.to_string()))?
}

async fn devices(State(st): S) -> ApiResult<Json<Value>> {
    match all_devices(&st).await {
        Ok(d) => Ok(Json(
            json!({"devices": d, "mock": st.cfg.mock, "note": null}),
        )),
        // No lsblk (a plain container): say so instead of failing the page.
        Err(Error::Coded { code, message, .. }) if code == "NO_LSBLK" => {
            Ok(Json(json!({"devices": [], "mock": false, "note": message})))
        }
        Err(e) => Err(e.into()),
    }
}

async fn eligible(st: &AppState, path: &str) -> ApiResult<Device> {
    let d = all_devices(st)
        .await?
        .into_iter()
        .find(|d| d.path == path)
        .ok_or_else(|| ApiError::not_found(format!("{path} isn't connected any more")))?;
    if !d.eligible {
        return Err(ApiError::new(
            axum::http::StatusCode::FORBIDDEN,
            "DEVICE_NOT_ALLOWED",
            format!(
                "{path} is off limits: {}",
                d.why_not.clone().unwrap_or_default()
            ),
            false,
        ));
    }
    Ok(d)
}

/// The exploit package folder: the one given (inside an allowed folder) or `<config>/badavatar`.
fn source_dir(st: &AppState, given: Option<&str>) -> Result<PathBuf, Error> {
    match given.filter(|g| !g.trim().is_empty()) {
        Some(g) => crate::library::validate_path(&st.cfg.roots, g),
        None => {
            let d = st.cfg.config_dir.join("badavatar");
            if st.cfg.mock && !d.exists() {
                badavatar::write_sample_package(&d)?;
            }
            Ok(d)
        }
    }
}

#[derive(Deserialize)]
struct SourceQuery {
    dir: Option<String>,
}

async fn source(State(st): S, Query(q): Query<SourceQuery>) -> ApiResult<Json<Value>> {
    let dir = source_dir(&st, q.dir.as_deref())?;
    let c = tokio::task::spawn_blocking(move || badavatar::check_source(&dir))
        .await
        .map_err(|e| Error::backend(e.to_string()))?;
    Ok(Json(json!(c)))
}

#[derive(Deserialize, Clone)]
struct BuildReq {
    device: String,
    source: Option<String>,
    #[serde(default)]
    format: bool,
    #[serde(default)]
    set_default: bool,
    label: Option<String>,
    /// The device path typed back, required to start.
    confirm: Option<String>,
}

async fn plan(State(st): S, Json(req): Json<BuildReq>) -> ApiResult<Json<Value>> {
    let d = eligible(&st, &req.device).await?;
    let src = source_dir(&st, req.source.as_deref())?;
    let dest = d.mountpoints.first().map(PathBuf::from);
    let (mock, fmt, def) = (st.cfg.mock, req.format, req.set_default);
    let p = tokio::task::spawn_blocking(move || match &dest {
        Some(m) => badavatar::plan(&src, m, def, fmt, !mock),
        None => {
            let mut p = badavatar::plan(&src, Path::new("/nonexistent"), def, fmt, false);
            if !fmt {
                p.problems.push("The stick isn't mounted. Tick Format first (it is mounted afterwards), or mount it and try again.".into());
                p.ok = false;
            }
            p
        }
    })
    .await
    .map_err(|e| Error::backend(e.to_string()))?;
    Ok(Json(json!({"device": d, "plan": p})))
}

/// Erase a mock stick: only ever inside RustyBox's own pretend-USB folder.
fn wipe_mock(dir: &Path, root: &Path) -> Result<(), Error> {
    if !dir.starts_with(root) || dir == root {
        return Err(Error::validation("Refusing to wipe that"));
    }
    for e in std::fs::read_dir(dir)?.flatten() {
        let p = e.path();
        if p.is_dir() {
            std::fs::remove_dir_all(&p)?;
        } else {
            std::fs::remove_file(&p)?;
        }
    }
    Ok(())
}

/// Format (real or mock) and return where the stick is mounted afterwards.
fn do_format(
    st: &AppState,
    d: &Device,
    label: &str,
    job: &crate::jobs::Job,
) -> Result<PathBuf, Error> {
    let label = usb::valid_label(label)?;
    if st.cfg.mock {
        let dir = PathBuf::from(
            d.mountpoints
                .first()
                .ok_or_else(|| Error::backend("The pretend stick has no folder"))?,
        );
        wipe_mock(&dir, &st.cfg.config_dir.join("mock-usb"))?;
        std::fs::write(dir.join(".mock-label"), &label)?;
        job.log(format!(
            "(pretend) formatted {} as FAT32 named {label}",
            d.path
        ));
        return Ok(dir);
    }
    job.step(format!("Unmounting {}", d.path));
    usb::unmount_all(d)?;
    job.step(format!("Formatting {} as FAT32 named {label}", d.path));
    usb::mkfs_fat32(Path::new(&d.path), &label)?;
    job.log(format!("Formatted {} as FAT32 named {label}", d.path));
    job.step("Mounting it again");
    usb::mount(d)
}

async fn start(State(st): S, Json(req): Json<BuildReq>) -> ApiResult<Json<Value>> {
    let d = eligible(&st, &req.device).await?;
    if req.format && req.confirm.as_deref() != Some(d.path.as_str()) {
        return Err(ApiError::bad(format!(
            "Formatting erases everything on {}. Type {} to confirm.",
            d.path, d.path
        )));
    }
    let src = source_dir(&st, req.source.as_deref())?;
    let chk = badavatar::check_source(&src);
    if !chk.ok {
        return Err(ApiError::bad(
            "The exploit package isn't there. Check its folder first.",
        ));
    }
    if !chk.has_payload || (req.set_default && !chk.has_aurora) {
        return Err(ApiError::new(
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "PLAN_HAS_PROBLEMS",
            if chk.has_payload {
                "Aurora can't be the default: there is no Apps/Aurora folder in the package."
            } else {
                "There is no BadUpdatePayload/default.xex (XeUnshackle or FreeMyXe). The exploit runs it, so a stick without it can't work."
            },
            true,
        ));
    }
    let job = st.jobs.create_queued(
        "usb",
        &format!("Build a Bad Avatar stick on {}", d.path),
        vec![format!("usb:{}", d.path)],
    );
    let job_id = job.id;
    let st2 = st.clone();
    st.jobs.spawn(job, move |job| async move {
        let (j, token) = (job.clone(), job.cancel_token());
        let mock = st2.cfg.mock;
        let rep = tokio::task::spawn_blocking(move || -> Result<badavatar::Report, Error> {
            let dest = if req.format {
                do_format(&st2, &d, req.label.as_deref().unwrap_or("BADUPDATE"), &j)?
            } else {
                PathBuf::from(
                    d.mountpoints
                        .first()
                        .ok_or_else(|| Error::validation("The stick isn't mounted"))?,
                )
            };
            let p = badavatar::plan(&src, &dest, req.set_default, req.format, !mock);
            if !p.ok {
                return Err(Error::validation(p.problems.join("\n")));
            }
            j.step("Copying the exploit files");
            let cancelled = || token.is_cancelled();
            let progress = |f: f32, what: &str| {
                j.progress(f * 100.0);
                if !what.is_empty() {
                    j.step(format!("Copying {what}"));
                }
            };
            badavatar::build(
                &src,
                &dest,
                req.set_default,
                &Ctl {
                    cancelled: &cancelled,
                    progress: &progress,
                },
            )
        })
        .await
        .map_err(|e| Error::backend(e.to_string()))??;
        for n in &rep.notes {
            job.log(n.clone());
        }
        job.log(format!(
            "{} files copied. The stick is ready: eject it safely before pulling it out.",
            rep.files
        ));
        Ok(())
    });
    Ok(Json(json!({"job": job_id})))
}

#[derive(Deserialize)]
struct FormatReq {
    device: String,
    label: Option<String>,
    confirm: Option<String>,
}

async fn format(State(st): S, Json(req): Json<FormatReq>) -> ApiResult<Json<Value>> {
    let d = eligible(&st, &req.device).await?;
    if req.confirm.as_deref() != Some(d.path.as_str()) {
        return Err(ApiError::bad(format!(
            "Formatting erases everything on {}. Type {} to confirm.",
            d.path, d.path
        )));
    }
    let label = usb::valid_label(req.label.as_deref().unwrap_or("XBOX360"))?;
    let job = st.jobs.create_queued(
        "usb",
        &format!("Format {}", d.path),
        vec![format!("usb:{}", d.path)],
    );
    let job_id = job.id;
    let st2 = st.clone();
    st.jobs.spawn(job, move |job| async move {
        let j = job.clone();
        tokio::task::spawn_blocking(move || do_format(&st2, &d, &label, &j).map(|_| ()))
            .await
            .map_err(|e| Error::backend(e.to_string()))?
    });
    Ok(Json(json!({"job": job_id})))
}

// ── Backups ──────────────────────────────────────────────────────────────────

fn backup_dir(st: &AppState) -> PathBuf {
    st.cfg.config_dir.join("backups")
}

async fn backups(State(st): S) -> Json<Value> {
    let dir = backup_dir(&st);
    let tools = usb::backup::have_tools().err().map(|e| e.to_string());
    Json(json!({
        "dir": dir.to_string_lossy(),
        "backups": usb::backup::list(&dir),
        "tools_missing": tools,
    }))
}

#[derive(Deserialize)]
struct BackupReq {
    device: String,
    name: String,
}

/// Back up a stick's filesystem (a job).
async fn backup(State(st): S, Json(req): Json<BackupReq>) -> ApiResult<Json<Value>> {
    usb::backup::have_tools()?;
    let d = eligible(&st, &req.device).await?;
    let fs_path = d.fs_path.clone().ok_or_else(|| {
        ApiError::bad("That stick has no filesystem to back up (format it first)")
    })?;
    let name = usb::backup::clean_name(&req.name)?;
    let dir = backup_dir(&st);
    if usb::backup::path_of(&dir, &name).exists() {
        return Err(ApiError::new(
            axum::http::StatusCode::CONFLICT,
            "EXISTS",
            format!("A backup called {name} already exists"),
            true,
        ));
    }
    let job = st.jobs.create_queued(
        "usb",
        &format!("Back up {}", d.path),
        vec![format!("usb:{}", d.path)],
    );
    let job_id = job.id;
    st.jobs.spawn(job, move |job| async move {
        let (j, token) = (job.clone(), job.cancel_token());
        let meta = tokio::task::spawn_blocking(move || {
            let cancelled = || token.is_cancelled();
            let progress = |_: f32, what: &str| j.step(what.to_string());
            usb::backup::backup(
                Path::new(&fs_path),
                &dir,
                &name,
                d.label.clone(),
                &Ctl {
                    cancelled: &cancelled,
                    progress: &progress,
                },
            )
        })
        .await
        .map_err(|e| Error::backend(e.to_string()))??;
        job.log(format!("Saved backup {}", meta.name));
        Ok(())
    });
    Ok(Json(json!({"job": job_id})))
}

#[derive(Deserialize)]
struct RestoreReq {
    backup: String,
    device: String,
    confirm: Option<String>,
}

/// Restore a backup onto a stick, erasing what is on it (a job).
async fn restore(State(st): S, Json(req): Json<RestoreReq>) -> ApiResult<Json<Value>> {
    usb::backup::have_tools()?;
    let d = eligible(&st, &req.device).await?;
    if req.confirm.as_deref() != Some(d.path.as_str()) {
        return Err(ApiError::bad(format!(
            "Restoring erases everything on {}. Type {} to confirm.",
            d.path, d.path
        )));
    }
    let name = usb::backup::clean_name(&req.backup)?;
    let file = usb::backup::path_of(&backup_dir(&st), &name);
    if !file.is_file() {
        return Err(ApiError::not_found(format!("No backup called {name}")));
    }
    let job = st.jobs.create_queued(
        "usb",
        &format!("Restore {name} onto {}", d.path),
        vec![format!("usb:{}", d.path)],
    );
    let job_id = job.id;
    let mock = st.cfg.mock;
    st.jobs.spawn(job, move |job| async move {
        let (j, token) = (job.clone(), job.cancel_token());
        tokio::task::spawn_blocking(move || -> Result<(), Error> {
            let dest = if mock {
                PathBuf::from(d.fs_path.clone().ok_or_else(|| {
                    Error::backend("This pretend stick has no image to restore onto")
                })?)
            } else {
                j.step(format!("Unmounting {}", d.path));
                usb::unmount_all(&d)?;
                PathBuf::from(d.fs_path.clone().unwrap_or_else(|| d.path.clone()))
            };
            let cancelled = || token.is_cancelled();
            let progress = |_: f32, what: &str| j.step(what.to_string());
            usb::backup::restore(
                &file,
                &dest,
                &Ctl {
                    cancelled: &cancelled,
                    progress: &progress,
                },
            )
        })
        .await
        .map_err(|e| Error::backend(e.to_string()))??;
        job.log("Restored. Unplug and re-plug the stick so it is mounted again.");
        Ok(())
    });
    Ok(Json(json!({"job": job_id})))
}

#[derive(Deserialize)]
struct DeleteQuery {
    name: String,
}

async fn delete_backup(State(st): S, Query(q): Query<DeleteQuery>) -> ApiResult<Json<Value>> {
    let name = usb::backup::clean_name(&q.name)?;
    let dir = backup_dir(&st);
    let file = usb::backup::path_of(&dir, &name);
    if !file.is_file() {
        return Err(ApiError::not_found(format!("No backup called {name}")));
    }
    std::fs::remove_file(&file)?;
    let _ = std::fs::remove_file(dir.join(format!("{name}.json")));
    Ok(Json(json!({"ok": true})))
}

// ── The exploit package, uploaded from the browser ───────────────────────────

#[derive(Deserialize)]
struct FileQuery {
    path: String,
}

/// Put one file of the exploit package in `<config>/badavatar/`. The path is relative and may not
/// climb out of that folder; the file is written as `.part` and renamed.
async fn put_file(
    State(st): S,
    Query(q): Query<FileQuery>,
    body: axum::body::Bytes,
) -> ApiResult<Json<Value>> {
    let root = st.cfg.config_dir.join("badavatar");
    let rel = q.path.trim_matches('/').to_string();
    if rel.is_empty()
        || rel
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Err(ApiError::bad("That isn't a valid file path"));
    }
    let n = body.len();
    tokio::task::spawn_blocking(move || -> Result<(), Error> {
        let dest = crate::fsops::join_rel(&root, &rel)?;
        if let Some(p) = dest.parent() {
            std::fs::create_dir_all(p)?;
        }
        let part = PathBuf::from(format!("{}.part", dest.display()));
        std::fs::write(&part, &body)?;
        std::fs::rename(&part, &dest)?;
        Ok(())
    })
    .await
    .map_err(|e| Error::backend(e.to_string()))??;
    Ok(Json(json!({"ok": true, "bytes": n})))
}

#[derive(Deserialize)]
struct DeletePackage {
    #[serde(default)]
    confirm: bool,
}

/// Remove the uploaded package (only ever `<config>/badavatar`).
async fn delete_package(State(st): S, Query(q): Query<DeletePackage>) -> ApiResult<Json<Value>> {
    if !q.confirm {
        return Err(ApiError::bad("Confirm to remove the exploit package"));
    }
    let dir = st.cfg.config_dir.join("badavatar");
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir)?;
    }
    Ok(Json(json!({"ok": true})))
}

/// Download the XeUnshackle payload into the package (a job), when you ask for it.
async fn fetch_payload(State(st): S) -> ApiResult<Json<Value>> {
    let pkg = st.cfg.config_dir.join("badavatar");
    let staging = st.cfg.config_dir.join("staging").join("payload");
    if st.cfg.mock {
        // No network in mock mode: put a pretend payload where the real one would go.
        std::fs::create_dir_all(pkg.join("BadUpdatePayload"))?;
        std::fs::write(
            pkg.join("BadUpdatePayload/default.xex"),
            b"pretend XeUnshackle payload",
        )?;
    }
    let mock = st.cfg.mock;
    let job = st
        .jobs
        .create(
            "usb",
            "Fetch the XeUnshackle payload",
            vec!["usb-package".into()],
        )
        .map_err(|b| ApiError::busy(&b))?;
    let job_id = job.id;
    st.jobs.spawn(job, move |job| async move {
        if mock {
            job.log("(pretend) XeUnshackle added to the package");
            return Ok(());
        }
        let (j, token) = (job.clone(), job.cancel_token());
        let n = tokio::task::spawn_blocking(move || {
            let cancelled = || token.is_cancelled();
            let progress = |f: f32, what: &str| {
                j.progress(f * 100.0);
                if !what.is_empty() {
                    j.step(what.to_string());
                }
            };
            usb::payload::fetch_xeunshackle(
                &pkg,
                &staging,
                &Ctl {
                    cancelled: &cancelled,
                    progress: &progress,
                },
            )
        })
        .await
        .map_err(|e| Error::backend(e.to_string()))??;
        job.log(format!(
            "{n} files from {} added to the package",
            usb::payload::XEUNSHACKLE_NAME
        ));
        Ok(())
    });
    Ok(Json(json!({"job": job_id})))
}
