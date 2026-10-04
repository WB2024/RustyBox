//! Job registry: long-running operations run as tokio tasks and report into a replayable
//! event log that browsers follow over SSE.
//!
//! Jobs lock *resources* (a console, a USB device, a library path), not one global lock: two
//! jobs conflict only when they name the same resource.

use std::{
    future::Future,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::{
    db::Db,
    error::{BoxError, Error},
};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Waiting for another job to finish with a resource this one needs.
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl Status {
    pub fn is_terminal(self) -> bool {
        !matches!(self, Status::Running | Status::Queued)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Progress {
        pct: f32,
    },
    Step {
        msg: String,
    },
    Log {
        msg: String,
    },
    Status {
        status: Status,
    },
    /// Structured output for the browser, e.g. a transfer summary.
    Result {
        name: String,
        data: serde_json::Value,
    },
    Error {
        error: String,
        message: String,
        recoverable: bool,
    },
}

struct Inner {
    events: Vec<Event>,
    status: Status,
    pct: f32,
    step: String,
    error: Option<BoxError>,
    finished: Option<u64>,
}

pub struct Job {
    pub id: u64,
    pub kind: String,
    pub title: String,
    /// What this job holds while it runs, e.g. `console:living-room`, `usb:/dev/sdb`, `path:/data/iso`.
    pub resources: Vec<String>,
    pub started: u64,
    inner: Mutex<Inner>,
    tx: watch::Sender<usize>,
    cancel: CancellationToken,
    /// Earlier jobs holding resources this one needs; it starts when they have all finished.
    waits_for: Mutex<Vec<Arc<Job>>>,
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Job #{} ({})", self.id, self.title)
    }
}

#[derive(Serialize)]
pub struct JobSummary {
    pub id: u64,
    pub kind: String,
    pub title: String,
    pub resources: Vec<String>,
    pub status: Status,
    pub pct: f32,
    pub step: String,
    pub error: Option<BoxError>,
    pub started: u64,
    pub finished: Option<u64>,
    pub events: usize,
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Job {
    pub fn push(&self, ev: Event) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        // The same percentage again adds nothing: don't grow the log (and every browser's replay).
        if let Event::Progress { pct } = &ev
            && (*pct - g.pct).abs() < f32::EPSILON
            && matches!(g.events.last(), Some(Event::Progress { .. }))
        {
            return;
        }
        match &ev {
            Event::Progress { pct } => g.pct = *pct,
            Event::Step { msg } => g.step = msg.clone(),
            Event::Status { status } => {
                g.status = *status;
                if status.is_terminal() {
                    g.finished = Some(now());
                }
            }
            Event::Error {
                error,
                message,
                recoverable,
            } => {
                g.error = Some(BoxError {
                    error: error.clone(),
                    message: message.clone(),
                    recoverable: *recoverable,
                });
            }
            _ => {}
        }
        g.events.push(ev);
        let len = g.events.len();
        drop(g);
        self.tx.send_replace(len);
    }

    pub fn progress(&self, pct: f32) {
        self.push(Event::Progress {
            pct: pct.clamp(0.0, 100.0),
        });
    }

    pub fn step(&self, msg: impl Into<String>) {
        self.push(Event::Step { msg: msg.into() });
    }

    pub fn log(&self, msg: impl Into<String>) {
        self.push(Event::Log { msg: msg.into() });
    }

    pub fn result(&self, name: &str, data: serde_json::Value) {
        self.push(Event::Result {
            name: name.into(),
            data,
        });
    }

    pub fn status(&self) -> Status {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).status
    }

    pub fn summary(&self) -> JobSummary {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        JobSummary {
            id: self.id,
            kind: self.kind.clone(),
            title: self.title.clone(),
            resources: self.resources.clone(),
            status: g.status,
            pct: g.pct,
            step: g.step.clone(),
            error: g.error.clone(),
            started: self.started,
            finished: g.finished,
            events: g.events.len(),
        }
    }

    /// Events from `from` onward, plus whether the job has reached a terminal state.
    pub fn events_since(&self, from: usize) -> (Vec<Event>, bool) {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        (
            g.events.get(from..).map(|s| s.to_vec()).unwrap_or_default(),
            g.status.is_terminal(),
        )
    }

    pub fn subscribe(&self) -> watch::Receiver<usize> {
        self.tx.subscribe()
    }

    pub fn request_cancel(&self) {
        self.cancel.cancel();
    }

    /// Long-running work should poll this (or `.cancelled().await`) and stop promptly.
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Wait until every earlier job holding one of this job's resources has finished.
    async fn wait_for_turn(&self) {
        let mut told = self
            .waits_for
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .first()
            .map_or(0, |j| j.id);
        loop {
            let blocker = self
                .waits_for
                .lock()
                .unwrap()
                .iter()
                .find(|j| !j.status().is_terminal())
                .cloned();
            match blocker {
                Some(b) => {
                    if told != b.id {
                        told = b.id;
                        self.step(format!("Waiting for job #{} ({})", b.id, b.title));
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                }
                None => break,
            }
        }
        self.waits_for
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        if self.status() == Status::Queued {
            self.push(Event::Status {
                status: Status::Running,
            });
        }
    }

    fn fail(&self, e: &Error) {
        let e = e.to_box_error();
        self.push(Event::Error {
            error: e.error,
            message: e.message,
            recoverable: e.recoverable,
        });
        self.push(Event::Status {
            status: Status::Failed,
        });
    }
}

#[derive(Default)]
pub struct Jobs {
    list: Mutex<Vec<Arc<Job>>>,
    next: AtomicU64,
    /// Where the record of jobs is kept, once there is a database.
    store: Arc<OnceLock<Db>>,
}

/// How many events of a job are kept in the record (the newest ones).
const KEPT_EVENTS: usize = 800;
const KEPT_JOBS: usize = 200;

/// Write a job's current state to the record. Failing to is never a reason to stop a job.
fn save(store: &OnceLock<Db>, job: &Job) {
    let Some(db) = store.get() else { return };
    let s = job.summary();
    let (mut events, _) = job.events_since(0);
    if events.len() > KEPT_EVENTS {
        events.drain(..events.len() - KEPT_EVENTS);
    }
    let _ = db.with(|c| {
        c.execute(
            "INSERT INTO job_history(id, kind, title, resources, status, pct, step, error, started, finished, events)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(id) DO UPDATE SET status = excluded.status, pct = excluded.pct, step = excluded.step,
                 error = excluded.error, finished = excluded.finished, events = excluded.events",
            rusqlite::params![
                s.id as i64,
                s.kind,
                s.title,
                serde_json::to_string(&s.resources).unwrap_or_default(),
                serde_json::to_string(&s.status).unwrap_or_default().trim_matches('"'),
                s.pct as f64,
                s.step,
                s.error.as_ref().and_then(|e| serde_json::to_string(e).ok()),
                s.started as i64,
                s.finished.map(|f| f as i64),
                serde_json::to_string(&events).unwrap_or_else(|_| "[]".into()),
            ],
        )?;
        c.execute(
            "DELETE FROM job_history WHERE id <= (SELECT max(id) FROM job_history) - ?1",
            [KEPT_JOBS as i64],
        )?;
        Ok(())
    });
}

impl Jobs {
    /// Keep a record of jobs in `db`, and bring back the ones from before a restart: finished
    /// ones as they were, ones that were still running as failed (they were cut off). Job
    /// numbers carry on from the last one, so a number saved elsewhere never means another job.
    pub fn attach(&self, db: Db) -> Result<(), Error> {
        type Row = (
            i64,
            String,
            String,
            String,
            String,
            f64,
            String,
            Option<String>,
            i64,
            Option<i64>,
            String,
        );
        let rows: Vec<Row> = db.with(|c| {
            let mut s = c.prepare(
                "SELECT id, kind, title, resources, status, pct, step, error, started, finished, events FROM job_history ORDER BY id",
            )?;
            Ok(s.query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?))
            })?
            .collect::<Result<_, _>>()?)
        })?;
        let mut list = self.list.lock().unwrap_or_else(|e| e.into_inner());
        let mut max = 0u64;
        for (id, kind, title, resources, status, pct, step, error, started, finished, events) in
            rows
        {
            let id = id as u64;
            max = max.max(id);
            let mut status: Status =
                serde_json::from_str(&format!("\"{status}\"")).unwrap_or(Status::Failed);
            let mut events: Vec<Event> = serde_json::from_str(&events).unwrap_or_default();
            let mut error: Option<BoxError> = error.and_then(|e| serde_json::from_str(&e).ok());
            let mut finished = finished.map(|f| f as u64);
            if !status.is_terminal() {
                let e = BoxError {
                    error: "INTERRUPTED".into(),
                    message: "RustyBox was restarted while this was running, so it was cut off."
                        .into(),
                    recoverable: true,
                };
                events.push(Event::Error {
                    error: e.error.clone(),
                    message: e.message.clone(),
                    recoverable: true,
                });
                events.push(Event::Status {
                    status: Status::Failed,
                });
                error = Some(e);
                status = Status::Failed;
                finished = Some(now());
            }
            let (tx, _rx) = watch::channel(events.len());
            let job = Arc::new(Job {
                id,
                kind,
                title,
                resources: serde_json::from_str(&resources).unwrap_or_default(),
                started: started as u64,
                inner: Mutex::new(Inner {
                    events,
                    status,
                    pct: pct as f32,
                    step,
                    error,
                    finished,
                }),
                tx,
                cancel: CancellationToken::new(),
                waits_for: Mutex::new(Vec::new()),
            });
            save(&self.store, &job);
            list.push(job);
        }
        self.next.fetch_max(max, Ordering::SeqCst);
        drop(list);
        let _ = self.store.set(db);
        Ok(())
    }

    /// Register a job. Fails with the job already holding one of the requested resources.
    pub fn create(
        &self,
        kind: &str,
        title: &str,
        resources: Vec<String>,
    ) -> Result<Arc<Job>, Arc<Job>> {
        self.register(kind, title, resources, false)
    }

    /// Register a job that waits its turn when a resource it needs is busy, instead of failing.
    pub fn create_queued(&self, kind: &str, title: &str, resources: Vec<String>) -> Arc<Job> {
        self.register(kind, title, resources, true)
            .unwrap_or_else(|j| j)
    }

    fn register(
        &self,
        kind: &str,
        title: &str,
        resources: Vec<String>,
        queue: bool,
    ) -> Result<Arc<Job>, Arc<Job>> {
        let mut list = self.list.lock().unwrap_or_else(|e| e.into_inner());
        let blockers: Vec<Arc<Job>> = list
            .iter()
            .filter(|j| {
                !j.status().is_terminal() && j.resources.iter().any(|r| resources.contains(r))
            })
            .cloned()
            .collect();
        if !queue && let Some(busy) = blockers.first() {
            return Err(busy.clone());
        }
        let queued = !blockers.is_empty();
        let first = if queued {
            Status::Queued
        } else {
            Status::Running
        };
        let (tx, _rx) = watch::channel(0usize);
        let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        let mut events = vec![Event::Status { status: first }];
        let mut step = String::new();
        if let Some(b) = blockers.first() {
            step = format!("Waiting for job #{} ({})", b.id, b.title);
            events.push(Event::Step { msg: step.clone() });
        }
        let job = Arc::new(Job {
            id,
            kind: kind.into(),
            title: title.into(),
            resources,
            started: now(),
            inner: Mutex::new(Inner {
                events,
                status: first,
                pct: 0.0,
                step,
                error: None,
                finished: None,
            }),
            tx,
            cancel: CancellationToken::new(),
            waits_for: Mutex::new(blockers),
        });
        list.push(job.clone());
        save(&self.store, &job);
        // Keep the history bounded: drop the oldest finished jobs beyond 200.
        if list.len() > 200
            && let Some(pos) = list.iter().position(|j| j.status().is_terminal())
        {
            list.remove(pos);
        }
        Ok(job)
    }

    /// Run `work` for `job` as a task. Returning `Ok` finishes the job as done, `Err` as failed,
    /// and a cancel request stops the task and finishes it as cancelled.
    pub fn spawn<F, Fut>(&self, job: Arc<Job>, work: F)
    where
        F: FnOnce(Arc<Job>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), Error>> + Send + 'static,
    {
        let store = self.store.clone();
        tokio::spawn(async move {
            let token = job.cancel_token();
            let j2 = job.clone();
            let fut = async move {
                j2.wait_for_turn().await;
                work(j2).await
            };
            // A panic in the work must end the job (and free its resources), not leave it
            // "running" for ever.
            let fut = futures_util::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(fut));
            tokio::select! {
                r = fut => match r {
                    Ok(Ok(())) => {
                        job.progress(100.0);
                        job.push(Event::Status { status: Status::Done });
                    }
                    Ok(Err(e)) => job.fail(&e),
                    Err(_) => job.fail(&Error::backend("The job stopped unexpectedly (an internal error); see the server log")),
                },
                _ = token.cancelled() => {
                    job.step("Cancelled");
                    job.push(Event::Status { status: Status::Cancelled });
                }
            }
            save(&store, &job);
        });
    }

    pub fn get(&self, id: u64) -> Option<Arc<Job>> {
        self.list
            .lock()
            .unwrap()
            .iter()
            .find(|j| j.id == id)
            .cloned()
    }

    /// Newest first.
    pub fn all(&self) -> Vec<Arc<Job>> {
        self.list
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .rev()
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflicts_only_on_shared_resources() {
        let jobs = Jobs::default();
        let a = jobs.create("t", "a", vec!["usb:/dev/sdb".into()]).unwrap();
        assert!(jobs.create("t", "b", vec!["console:x".into()]).is_ok());
        let busy = jobs
            .create("t", "c", vec!["usb:/dev/sdb".into(), "console:y".into()])
            .unwrap_err();
        assert_eq!(busy.id, a.id);
        a.push(Event::Status {
            status: Status::Done,
        });
        assert!(jobs.create("t", "d", vec!["usb:/dev/sdb".into()]).is_ok());
    }

    #[tokio::test]
    async fn queued_jobs_wait_their_turn_in_order() {
        let jobs = Jobs::default();
        let first = jobs.create("t", "first", vec!["r".into()]).unwrap();
        let second = jobs.create_queued("t", "second", vec!["r".into()]);
        let third = jobs.create_queued("t", "third", vec!["r".into()]);
        assert_eq!(second.status(), Status::Queued);
        let order = Arc::new(Mutex::new(Vec::new()));
        for (j, n) in [(&second, 2), (&third, 3)] {
            let o = order.clone();
            jobs.spawn(j.clone(), move |_| async move {
                o.lock().unwrap_or_else(|e| e.into_inner()).push(n);
                Ok(())
            });
        }
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        assert!(
            order.lock().unwrap_or_else(|e| e.into_inner()).is_empty(),
            "must not start while the first runs"
        );
        first.push(Event::Status {
            status: Status::Done,
        });
        for _ in 0..100 {
            if third.status().is_terminal() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(*order.lock().unwrap_or_else(|e| e.into_inner()), vec![2, 3]);
        assert_eq!(second.status(), Status::Done);
    }

    #[tokio::test]
    async fn job_history_survives_a_restart_and_numbers_are_not_reused() {
        let db = Db::open_memory().unwrap();
        let first = Jobs::default();
        first.attach(db.clone()).unwrap();
        let done = first.create("t", "finished", vec!["r".into()]).unwrap();
        first.spawn(done.clone(), |j| async move {
            j.log("did it");
            Ok(())
        });
        let cut_off = first.create("t", "running", vec!["x".into()]).unwrap();
        for _ in 0..50 {
            if done.status().is_terminal() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        // "Restart": a new registry over the same database.
        let second = Jobs::default();
        second.attach(db).unwrap();
        let old = second.get(done.id).expect("the finished job is remembered");
        assert_eq!(old.status(), Status::Done);
        assert!(
            old.events_since(0)
                .0
                .iter()
                .any(|e| matches!(e, Event::Log { msg } if msg == "did it"))
        );
        let interrupted = second.get(cut_off.id).unwrap();
        assert_eq!(interrupted.status(), Status::Failed);
        assert_eq!(interrupted.summary().error.unwrap().error, "INTERRUPTED");
        // The next job gets a number nobody has used, and resources of the dead job are free.
        let fresh = second.create("t", "new", vec!["x".into()]).unwrap();
        assert!(fresh.id > cut_off.id);
    }

    #[tokio::test]
    async fn a_panicking_job_fails_and_frees_its_resources() {
        let jobs = Jobs::default();
        let j = jobs.create("t", "boom", vec!["r".into()]).unwrap();
        jobs.spawn(j.clone(), |_| async {
            if true {
                panic!("boom");
            }
            Ok(())
        });
        for _ in 0..50 {
            if j.status().is_terminal() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(j.status(), Status::Failed);
        assert!(jobs.create("t", "next", vec!["r".into()]).is_ok());
    }

    #[test]
    fn repeated_progress_is_not_logged_twice() {
        let jobs = Jobs::default();
        let j = jobs.create("t", "p", vec![]).unwrap();
        let before = j.summary().events;
        j.progress(10.0);
        j.progress(10.0);
        j.progress(10.0);
        assert_eq!(j.summary().events, before + 1);
    }

    #[tokio::test]
    async fn spawn_reports_done_failed_and_cancelled() {
        let jobs = Jobs::default();
        let ok = jobs.create("t", "ok", vec![]).unwrap();
        jobs.spawn(ok.clone(), |_| async { Ok(()) });
        let bad = jobs.create("t", "bad", vec![]).unwrap();
        jobs.spawn(bad.clone(), |_| async { Err(Error::validation("nope")) });
        let slow = jobs.create("t", "slow", vec![]).unwrap();
        jobs.spawn(slow.clone(), |_| async {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            Ok(())
        });
        slow.request_cancel();
        for _ in 0..50 {
            if [&ok, &bad, &slow].iter().all(|j| j.status().is_terminal()) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(ok.status(), Status::Done);
        assert_eq!(bad.status(), Status::Failed);
        assert_eq!(bad.summary().error.unwrap().message, "nope");
        assert_eq!(slow.status(), Status::Cancelled);
    }
}
