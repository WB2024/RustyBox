//! Turning a title-list name into search queries, and picking the right game from the results.

use super::Candidate;

/// Lower-case, `&` as "and", punctuation and symbols removed, spaces collapsed.
pub fn norm(s: &str) -> String {
    let s = s.replace('&', " and ");
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_lowercase().next().unwrap_or(c)
            } else {
                ' '
            }
        })
        .collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Drop `(Europe)`, `[eM]`, and the like, and trademark signs.
pub fn clean(name: &str) -> String {
    let mut out = String::new();
    let mut depth = 0i32;
    for c in name.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = (depth - 1).max(0),
            '™' | '®' | '©' => {}
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(|c: char| c == '-' || c == ':' || c.is_whitespace())
        .to_string()
}

/// Words that mark a release as a special edition or a disc, not part of the game's name.
const EDITION_TAILS: &[&str] = &[
    "bonus disc",
    "limited collector's edition",
    "limited collectors edition",
    "collector's edition",
    "collectors edition",
    "limited edition",
    "special edition",
    "game of the year edition",
    "game of the year",
    "goty edition",
    "goty",
    "platinum hits",
    "greatest hits",
    "classics",
    "complete edition",
    "demo",
    "trial",
];

pub fn strip_edition(name: &str) -> String {
    let mut cur = name.trim().to_string();
    loop {
        let lower = cur.to_lowercase();
        let Some(tail) = EDITION_TAILS.iter().find(|t| lower.ends_with(**t)) else {
            break;
        };
        // Lower-casing can change a name's length (some non-English letters), so the cut point
        // found in `lower` may not be a character boundary in `cur`.
        let Some(cut) = cur
            .len()
            .checked_sub(tail.len())
            .filter(|n| cur.is_char_boundary(*n))
        else {
            break;
        };
        cur = cur[..cut]
            .trim_end_matches(|c: char| c == '-' || c == ':' || c.is_whitespace())
            .to_string();
    }
    cur
}

/// The searches to try, most specific first.
pub fn variants(name: &str) -> Vec<String> {
    let full = clean(name);
    let stripped = strip_edition(&full);
    let mut out = Vec::new();
    for v in [full, stripped] {
        if !v.is_empty() && !out.contains(&v) {
            out.push(v);
        }
    }
    out
}

/// How well a candidate's name matches the query, 0 to 100.
pub fn score(query: &str, candidate: &str) -> u32 {
    let (a, b) = (norm(query), norm(candidate));
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    if a == b {
        return 100;
    }
    // One name starts with the other ("Halo 3" and "Halo 3 ODST"): the closer in length, the better.
    if b.starts_with(&a) || a.starts_with(&b) {
        let diff = a.len().abs_diff(b.len()) as u32;
        return 85u32.saturating_sub(diff.min(45));
    }
    let wa: std::collections::HashSet<&str> = a.split(' ').collect();
    let wb: std::collections::HashSet<&str> = b.split(' ').collect();
    let inter = wa.intersection(&wb).count() as f32;
    let union = wa.union(&wb).count() as f32;
    (inter / union * 80.0) as u32
}

/// Below this, a match isn't trusted and the game is left for a human to choose.
pub const MIN_SCORE: u32 = 60;

/// The best candidate for `query`, keeping IGDB's own order for ties. `None` if nothing is close enough.
pub fn best<'a>(query: &str, candidates: &'a [Candidate]) -> Option<(&'a Candidate, u32)> {
    let mut best: Option<(&Candidate, u32)> = None;
    for c in candidates {
        let s = score(query, &c.name);
        if s >= MIN_SCORE && best.is_none_or(|(_, b)| s > b) {
            best = Some((c, s));
        }
    }
    best
}

#[cfg(test)]
mod tests {
    #[test]
    fn names_with_letters_that_change_length_when_lower_cased_never_panic() {
        // "İ" is two bytes and lower-cases to three.
        for n in [
            "İstanbul Edition",
            "Ünïcode Game - Gold Edition",
            "İİİ Collection",
        ] {
            let _ = strip_edition(n);
        }
        assert_eq!(
            strip_edition("Fable II - Game of the Year Edition"),
            "Fable II"
        );
    }

    use super::*;

    fn cand(id: i64, name: &str) -> Candidate {
        Candidate {
            id,
            name: name.into(),
            summary: None,
            release: None,
            genres: vec![],
            developers: vec![],
            publishers: vec![],
            rating: None,
            url: None,
            cover: None,
        }
    }

    #[test]
    fn cleans_regional_tags_and_symbols() {
        assert_eq!(
            clean("[eM] -eNCHANT arM- (Asian Version)"),
            "-eNCHANT arM-".trim_matches('-')
        );
        assert_eq!(clean("Gears of War 2 (Europe)"), "Gears of War 2");
        assert_eq!(clean("Splinter Cell™ Blacklist"), "Splinter Cell Blacklist");
        assert_eq!(clean("Alan Wake: "), "Alan Wake");
    }

    #[test]
    fn strips_edition_words_from_the_end_only() {
        assert_eq!(
            strip_edition("Alan Wake Limited Collector's Edition Bonus Disc"),
            "Alan Wake"
        );
        assert_eq!(
            strip_edition("Fable II Game of the Year Edition"),
            "Fable II"
        );
        assert_eq!(strip_edition("Demo Derby"), "Demo Derby");
        assert_eq!(
            strip_edition("Forza Motorsport 4 Platinum Hits"),
            "Forza Motorsport 4"
        );
    }

    #[test]
    fn variants_are_distinct_and_specific_first() {
        assert_eq!(
            variants("Alan Wake Limited Collector's Edition Bonus Disc (USA)"),
            vec![
                "Alan Wake Limited Collector's Edition Bonus Disc",
                "Alan Wake"
            ]
        );
        assert_eq!(variants("Halo 3"), vec!["Halo 3"]);
        assert!(variants("(USA)").is_empty());
    }

    #[test]
    fn picks_the_right_game_not_the_dlc_or_a_longer_name() {
        let list = vec![
            cand(1, "Gears of War 2: Dark Corners"),
            cand(2, "Gears of War 2"),
            cand(3, "Gears of War 2: Game of the Year Edition"),
        ];
        assert_eq!(best("Gears of War 2", &list).unwrap().0.id, 2);
        let halo = vec![
            cand(10, "Halo 3: Legendary Edition"),
            cand(11, "Halo 3: ODST"),
            cand(12, "Halo 3"),
        ];
        assert_eq!(best("Halo 3", &halo).unwrap().0.id, 12);
        // Close names still win over unrelated ones; unrelated results are refused.
        assert_eq!(
            best(
                "Forza Motorsport 4",
                &[
                    cand(1, "Forza Motorsport 4: Essentials Edition"),
                    cand(2, "Sonic")
                ]
            )
            .unwrap()
            .0
            .id,
            1
        );
        assert!(best("Mystery Game", &[cand(1, "Something Else Entirely")]).is_none());
        assert!(best("x", &[]).is_none());
    }

    #[test]
    fn matching_ignores_case_punctuation_and_ampersands() {
        assert_eq!(
            score("Call of Duty: Black Ops II", "call of duty black ops ii"),
            100
        );
        assert_eq!(score("Banjo-Kazooie", "Banjo Kazooie"), 100);
        assert_eq!(score("Tom & Jerry", "Tom and Jerry"), 100);
        assert!(score("Halo", "Gears of War") < MIN_SCORE);
    }
}
