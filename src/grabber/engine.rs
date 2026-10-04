//! Searching every indexer for a wanted game and judging what comes back.

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use super::{
    config::Config,
    newznab::{self, Indexer, Release},
    release::{self, Verdict},
};
use crate::jobs::now;

/// A search result with the verdict on it.
#[derive(Debug, Clone, Serialize)]
pub struct Judged {
    #[serde(flatten)]
    pub release: Release,
    pub verdict: Verdict,
    /// Days since it was posted.
    pub age_days: Option<i64>,
}

#[derive(Debug, Default, Serialize)]
pub struct Outcome {
    pub results: Vec<Judged>,
    /// Indexers that couldn't answer, with why.
    pub errors: Vec<String>,
    /// Indexers that were searched.
    pub searched: usize,
    /// Indexers left out, with why (torrent indexers when qBittorrent isn't set up).
    pub skipped: Vec<String>,
}

impl Outcome {
    /// The best release that isn't rejected and scores at least `min`.
    pub fn best(&self, min: i32) -> Option<&Judged> {
        self.results
            .iter()
            .find(|j| j.verdict.rejected.is_empty() && j.verdict.score >= min)
    }

    /// A line for the wanted list: what the last search found.
    pub fn summary(&self) -> String {
        let ok = self
            .results
            .iter()
            .filter(|j| j.verdict.rejected.is_empty())
            .count();
        let errs = if self.errors.is_empty() {
            String::new()
        } else {
            format!("; {} indexer(s) failed", self.errors.len())
        };
        format!("{} result(s), {ok} acceptable{errs}", self.results.len())
    }
}

/// The indexers worth searching: enabled, and not known to lack Xbox 360 games.
pub fn usable(cfg: &Config) -> Vec<Indexer> {
    let mut v: Vec<Indexer> = cfg
        .indexers
        .iter()
        .filter(|i| {
            i.enabled
                && i.has_xbox360 != Some(false)
                // A torrent can only be fetched through qBittorrent.
                && (i.protocol != "torrent" || cfg.qbit.configured())
        })
        .cloned()
        .collect();
    v.sort_by_key(|i| (i.priority, i.id));
    v
}

/// Search all usable indexers (in parallel) for any of `names`, then judge, de-duplicate and rank.
pub fn search_game(cfg: &Config, names: &[String], blocked: &HashSet<String>) -> Outcome {
    let indexers = usable(cfg);
    let queries: Vec<String> = release::query_variants(names).into_iter().take(4).collect();
    let mut out = Outcome {
        searched: indexers.len(),
        ..Default::default()
    };
    if !cfg.qbit.configured() {
        let n = cfg
            .indexers
            .iter()
            .filter(|i| i.enabled && i.protocol == "torrent")
            .count();
        if n > 0 {
            out.skipped.push(format!(
                "{n} torrent indexer(s) weren't searched: qBittorrent isn't set up"
            ));
        }
    }
    let answers: Vec<(String, Result<Vec<Release>, crate::error::Error>)> =
        std::thread::scope(|s| {
            let handles: Vec<_> = indexers
                .iter()
                .map(|ix| {
                    let queries = &queries;
                    s.spawn(move || {
                        let mut all = Vec::new();
                        let mut err = None;
                        for q in queries {
                            match newznab::search(ix, q) {
                                Ok(r) => all.extend(r),
                                Err(e) => {
                                    err = Some(e);
                                    break;
                                }
                            }
                        }
                        (
                            ix.name.clone(),
                            match (err, all.is_empty()) {
                                (Some(e), true) => Err(e),
                                _ => Ok(all),
                            },
                        )
                    })
                })
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        });
    let mut seen: HashMap<(String, u64), usize> = HashMap::new();
    let t = now() as i64;
    for (name, res) in answers {
        match res {
            Err(e) => out.errors.push(format!("{name}: {e}")),
            Ok(list) => {
                for r in list {
                    let age = r.posted.map(|p| ((t - p) / 86_400).max(0));
                    let mut verdict = release::judge(
                        &release::Candidate {
                            title: &r.title,
                            size: r.size,
                            categories: &r.categories,
                            age_days: age,
                            grabs: r.grabs,
                            indexer_priority: r.indexer_priority,
                            seeders: r.seeders,
                        },
                        names,
                        &cfg.profile,
                    );
                    if blocked.contains(&r.guid) || blocked.contains(&r.title.to_lowercase()) {
                        verdict
                            .rejected
                            .push("On the blocklist (an earlier download of it failed)".into());
                    }
                    // The same post found through two indexers is listed once, via the better one.
                    let key = (r.title.to_lowercase(), r.size);
                    match seen.get(&key) {
                        Some(&i) => {
                            if r.indexer_priority < out.results[i].release.indexer_priority {
                                out.results[i] = Judged {
                                    release: r,
                                    verdict,
                                    age_days: age,
                                };
                            }
                        }
                        None => {
                            seen.insert(key, out.results.len());
                            out.results.push(Judged {
                                release: r,
                                verdict,
                                age_days: age,
                            });
                        }
                    }
                }
            }
        }
    }
    // Acceptable first, then the best score, then the bigger release; ties by name.
    let key = |j: &Judged| {
        (
            j.verdict.rejected.is_empty(),
            j.verdict.score,
            j.release.size,
        )
    };
    out.results.sort_by(|a, b| {
        key(b)
            .cmp(&key(a))
            .then(a.release.title.cmp(&b.release.title))
    });
    out
}
