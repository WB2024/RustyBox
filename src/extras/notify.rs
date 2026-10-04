//! Notifications: a short JSON message POSTed to a web address when a job finishes.
//!
//! The body has the keys most services look for (`title`, `message`, and `text` and `content`
//! for Slack and Discord); ntfy also gets `Title` and `Tags` headers. The address is a secret
//! (Discord and ntfy put tokens in it) and is never sent back to the browser.

use std::time::Duration;

use serde_json::json;

use crate::{error::Error, jobs::JobSummary};

pub fn send(
    url: &str,
    title: &str,
    message: &str,
    status: &str,
    job: Option<u64>,
) -> Result<(), Error> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(8))
        .timeout_read(Duration::from_secs(15))
        .user_agent("RustyBox")
        .build();
    let tag = match status {
        "done" => "white_check_mark",
        "failed" => "x",
        _ => "information_source",
    };
    let r = agent
        .post(url)
        .set("Title", title)
        .set("Tags", tag)
        .send_json(json!({
            "app": "RustyBox",
            "title": title,
            "message": message,
            "status": status,
            "job": job,
            "text": format!("{title}: {message}"),
            "content": format!("**{title}**\n{message}"),
        }));
    match r {
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(code, _)) => Err(Error::backend(format!(
            "The notification address answered {code}"
        ))),
        Err(e) => Err(Error::backend(format!(
            "Couldn't reach the notification address: {e}"
        ))),
    }
}

/// Should a finished job be announced, given the setting?
pub fn wanted(mode: &str, j: &JobSummary) -> bool {
    use crate::jobs::Status::*;
    if matches!(j.kind.as_str(), "igdb" | "demo") {
        return false;
    }
    match mode {
        "all" => matches!(j.status, Done | Failed),
        "failures" => matches!(j.status, Failed),
        _ => false,
    }
}

pub fn describe(j: &JobSummary) -> (String, String) {
    let status = match j.status {
        crate::jobs::Status::Done => "finished",
        crate::jobs::Status::Failed => "failed",
        _ => "stopped",
    };
    let msg = j
        .error
        .as_ref()
        .map(|e| e.message.lines().next().unwrap_or("").to_string())
        .unwrap_or_else(|| j.title.clone());
    (format!("{} {status}", j.title), msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::{Jobs, Status};

    #[test]
    fn only_wanted_jobs_are_announced() {
        let jobs = Jobs::default();
        let j = jobs.create("import", "Copy: Fable", vec![]).unwrap();
        j.push(crate::jobs::Event::Status {
            status: Status::Failed,
        });
        let s = j.summary();
        assert!(wanted("failures", &s) && wanted("all", &s) && !wanted("off", &s));
        let ok = jobs.create("import", "x", vec![]).unwrap();
        ok.push(crate::jobs::Event::Status {
            status: Status::Done,
        });
        assert!(!wanted("failures", &ok.summary()) && wanted("all", &ok.summary()));
        let ig = jobs.create("igdb", "x", vec![]).unwrap();
        ig.push(crate::jobs::Event::Status {
            status: Status::Failed,
        });
        assert!(
            !wanted("all", &ig.summary()),
            "background lookups are never announced"
        );
    }
}
