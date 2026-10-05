//! Reading a Usenet release name ("Gears.of.War.3.XGD3.PAL.SPANiSH.XBOX360-FBi") and deciding
//! whether it is the game that was asked for, and how good a copy it is.
//!
//! Indexers search loosely ("Halo 3" also finds Halo Reach and ODST), so a release only counts
//! when, with the platform, region, language and format words taken out, its name is the wanted
//! game's name, give or take edition words such as "GOTY". Anything else is rejected with the
//! reason, which the interactive search shows.

use serde::{Deserialize, Serialize};

use crate::igdb::rank::norm;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Region {
    Usa,
    Pal,
    Japan,
    /// Region free / multi region.
    Free,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// A disc image (the usual).
    Iso,
    /// Games on Demand folders.
    God,
    /// An Xbox Live Arcade package (the arcade game itself).
    Package,
}

/// What can be read from a release name.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Parsed {
    /// The game's name as the release spells it, platform and tags removed.
    pub name: String,
    pub region: Region,
    pub format: Format,
    pub disc: Option<u32>,
    pub proper: bool,
    pub is_360: bool,
    pub other_platform: Option<String>,
    /// DLC, title update, demo, trial, dashboard...: not the game itself.
    pub not_a_game: Option<String>,
    /// A fragment of a multi-part post ("part01").
    pub fragment: bool,
    pub group: Option<String>,
}

const PLATFORM_360: &[&str] = &["xbox360", "x360", "xbox 360", "xbox-360"];
const OTHER_PLATFORMS: &[(&str, &str)] = &[
    ("ps3", "PS3"),
    ("ps4", "PS4"),
    ("ps5", "PS5"),
    ("wii", "Wii"),
    ("wiiu", "Wii U"),
    ("nds", "DS"),
    ("psp", "PSP"),
    ("pc", "PC"),
    ("xboxone", "Xbox One"),
    ("xbox one", "Xbox One"),
    ("xone", "Xbox One"),
    ("switch", "Switch"),
    ("nsw", "Switch"),
    ("ps2", "PS2"),
];
const REGION_WORDS: &[(&str, Region)] = &[
    ("usa", Region::Usa),
    ("ntscu", Region::Usa),
    ("ntsc-u", Region::Usa),
    ("us", Region::Usa),
    ("pal", Region::Pal),
    ("eur", Region::Pal),
    ("europe", Region::Pal),
    ("jpn", Region::Japan),
    ("jap", Region::Japan),
    ("ntscj", Region::Japan),
    ("ntsc-j", Region::Japan),
    ("japan", Region::Japan),
    ("rf", Region::Free),
    ("regionfree", Region::Free),
    // Redump and No-Intro say "World" for a disc that works everywhere.
    ("world", Region::Free),
    ("ntsc", Region::Usa),
];
const LANGUAGES: &[&str] = &[
    "german",
    "french",
    "spanish",
    "italian",
    "dutch",
    "swedish",
    "danish",
    "norwegian",
    "finnish",
    "polish",
    "russian",
    "portuguese",
    "czech",
    "hungarian",
    "greek",
    "korean",
    "chinese",
    "arabic",
    "turkish",
    "english",
    "multi",
    "multi2",
    "multi3",
    "multi4",
    "multi5",
    "multi6",
    "multi7",
    "multi8",
    "multi9",
    "multi10",
    "multi11",
    "multi12",
    "dutch",
    "swe",
    "ger",
    "fre",
    "spa",
    "ita",
];
/// Two-letter language codes, as in Redump names: `(En,Ja,Fr,De,Es,It,Pt,Zh,Ko)`. Short words like
/// "no" and "it" start real game names, so a code only counts as one in a run of codes or right
/// after a region, never as the first word.
const LANGUAGE_CODES: &[&str] = &[
    "en", "ja", "fr", "de", "es", "it", "pt", "zh", "ko", "nl", "sv", "no", "da", "fi", "pl", "ru",
    "cs", "hu", "tr", "ar", "el", "he", "ca", "sk", "uk", "bg", "ro", "hr",
];

/// Tags that say something about the release, not the game.
const TAGS: &[&str] = &[
    "xbla",
    "proper",
    "repack",
    "readnfo",
    "nfo",
    "internal",
    "rerip",
    "retail",
    "dvd9",
    "dvd5",
    "dvdr",
    "xgd1",
    "xgd2",
    "xgd3",
    "xgd",
    "iso",
    "unrar",
    "unrared",
    "dlcunlocker",
    "extras",
    "reup",
    "re-up",
    "complete",
    "fix",
    "fixed",
    "dirfix",
    "nfofix",
    "xex",
    "jtag",
    "rgh",
    "freeboot",
    "region",
    "free",
    "disc",
    "dvd",
    "ntsc",
    "cracked",
    "crack",
    "bonus",
    "ac",
    "mixed",
    "xbla",
    "arcade",
];
/// Words that may follow a game's name without making it a different game.
const EDITION_WORDS: &[&str] = &[
    "goty",
    "game",
    "of",
    "the",
    "year",
    "edition",
    "platinum",
    "hits",
    "greatest",
    "classics",
    "collectors",
    "collector",
    "limited",
    "special",
    "ultimate",
    "complete",
    "deluxe",
    "gold",
    "legendary",
    "premium",
    "definitive",
    "anniversary",
    "remastered",
    "hd",
    "s",
];

pub fn tokens(s: &str) -> Vec<String> {
    norm(s)
        .split(' ')
        .filter(|t| !t.is_empty())
        .map(String::from)
        .collect()
}

/// Roman numerals up to X as numbers, so "Fable II" matches "Fable 2".
fn numeral(t: &str) -> String {
    match t {
        "i" => "1",
        "ii" => "2",
        "iii" => "3",
        "iv" => "4",
        "v" => "5",
        "vi" => "6",
        "vii" => "7",
        "viii" => "8",
        "ix" => "9",
        "x" => "10",
        o => o,
    }
    .to_string()
}

/// Split `Name.Words.TAGS.XBOX360-GROUP` into its parts.
pub fn parse(title: &str) -> Parsed {
    let spaced = title.replace(['.', '_'], " ");
    let lower = spaced.to_lowercase();
    // The group after the last dash, if it looks like one (no spaces).
    let group = title
        .rsplit_once('-')
        .map(|(_, g)| g.trim().to_string())
        .filter(|g| !g.is_empty() && g.len() <= 24 && !g.contains(' ') && !g.contains('.'));
    let all = tokens(&lower.replace('-', " "));
    let joined = all.join(" ");
    let is_360 = PLATFORM_360
        .iter()
        .any(|p| joined.contains(&p.replace('-', " ")) || lower.contains(p))
        || all.iter().any(|t| t == "xbox360" || t == "x360")
        || joined.contains("xbox 360");
    let other_platform = OTHER_PLATFORMS
        .iter()
        .find(|(k, _)| all.iter().any(|t| t == k) || joined.contains(&format!(" {k} ")))
        .map(|(_, n)| n.to_string())
        .or_else(|| {
            // Original Xbox: "XBOX" with no 360.
            (all.iter().any(|t| t == "xbox") && !is_360).then(|| "original Xbox".to_string())
        });
    let mut region = Region::Unknown;
    for t in &all {
        if let Some((_, r)) = REGION_WORDS.iter().find(|(w, _)| w == t) {
            // RF wins over a plain NTSC/PAL mention ("USA RF").
            if region == Region::Unknown || *r == Region::Free {
                region = *r;
            }
        }
    }
    if joined.contains("region free") {
        region = Region::Free;
    }
    let god = joined.contains(" god ")
        || joined.ends_with(" god")
        || joined.contains("games on demand")
        || joined.contains("gods ");
    let not_a_game = [
        ("dlc", "DLC"),
        ("addon", "an add-on"),
        ("addons", "add-ons"),
        ("update", "a title update"),
        ("tu", "a title update"),
        ("demo", "a demo"),
        ("beta", "a beta"),
        ("proto", "a prototype"),
        ("prototype", "a prototype"),
        ("sample", "a sample"),
        ("preview", "a preview"),
        ("trial", "a trial"),
        ("dashboard", "a dashboard"),
        ("avatar", "an avatar item"),
        ("theme", "a theme"),
        ("gamerpic", "a gamer picture"),
        ("soundtrack", "a soundtrack"),
        ("ost", "a soundtrack"),
        ("kinect", ""),
    ]
    .iter()
    .find(|(k, why)| !why.is_empty() && all.iter().any(|t| t == k))
    .map(|(_, why)| why.to_string());
    let format = if all.iter().any(|t| t == "xbla") || not_a_game.is_some() {
        Format::Package
    } else if god {
        Format::God
    } else {
        Format::Iso
    };
    let disc = all
        .windows(2)
        .find_map(|w| {
            if w[0] == "disc" || w[0] == "cd" {
                w[1].parse::<u32>().ok()
            } else {
                None
            }
        })
        .or_else(|| {
            all.iter()
                .find_map(|t| t.strip_prefix("disc").and_then(|n| n.parse().ok()))
        });
    let fragment = all.iter().any(|t| {
        t.strip_prefix("part")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    });
    // The name: words up to the first marker (platform, region, language, tag).
    let noise = |t: &str| {
        PLATFORM_360.contains(&t)
            || t == "xbox"
            // A title id in the name, as No-Intro writes "(584108B7)".
            || t.len() == 8
                && t.bytes().all(|b| b.is_ascii_hexdigit())
                && t.bytes().any(|b| b.is_ascii_digit())
                && t.bytes().any(|b| b.is_ascii_alphabetic())
            || REGION_WORDS.iter().any(|(w, _)| *w == t)
            || LANGUAGES.contains(&t)
            || TAGS.contains(&t)
            || t.starts_with("disc") && t[4..].bytes().all(|b| b.is_ascii_digit())
            || t.starts_with("part") && t[4..].bytes().all(|b| b.is_ascii_digit())
            || OTHER_PLATFORMS.iter().any(|(k, _)| *k == t)
    };
    // A language code is noise only next to other codes or a region (never as the first word).
    let code_at = |i: usize| {
        let t = all[i].as_str();
        LANGUAGE_CODES.contains(&t)
            && i > 0
            && (LANGUAGE_CODES.contains(&all[i - 1].as_str())
                || REGION_WORDS.iter().any(|(w, _)| *w == all[i - 1])
                || all
                    .get(i + 1)
                    .is_some_and(|n| LANGUAGE_CODES.contains(&n.as_str())))
    };
    let mut name_tokens: Vec<&String> = Vec::new();
    for (i, t) in all.iter().enumerate() {
        if noise(t) || code_at(i) {
            break;
        }
        name_tokens.push(t);
    }
    // If the name came out empty (the tags came first), use everything that isn't noise.
    if name_tokens.is_empty() {
        name_tokens = all
            .iter()
            .enumerate()
            .filter(|(i, t)| !noise(t) && !code_at(*i))
            .map(|(_, t)| t)
            .collect();
    }
    let name = name_tokens
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    Parsed {
        name,
        region,
        format,
        disc,
        proper: all.iter().any(|t| t == "proper"),
        is_360,
        other_platform,
        not_a_game,
        fragment,
        group,
    }
}

/// How a release's name compares with the wanted game's.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum NameMatch {
    /// The same name.
    Exact,
    /// The same, plus edition words ("GOTY").
    Edition,
    /// A different game: the reason says how.
    Different(String),
}

/// Compare a parsed release name with the wanted game's names (its IGDB name and variants).
pub fn match_name(release_name: &str, wanted: &[String]) -> NameMatch {
    let rel: Vec<String> = tokens(release_name)
        .into_iter()
        .map(|t| numeral(&t))
        .filter(|t| t != "the")
        .collect();
    let mut best: Option<NameMatch> = None;
    for w in wanted {
        let want: Vec<String> = tokens(w)
            .into_iter()
            .map(|t| numeral(&t))
            .filter(|t| t != "the")
            .collect();
        if want.is_empty() {
            continue;
        }
        if rel == want {
            return NameMatch::Exact;
        }
        // "Halo 3" in "Halo 3 GOTY Edition": the wanted words, then only edition words.
        if rel.len() > want.len() && rel[..want.len()] == want[..] {
            let extra = &rel[want.len()..];
            if extra.iter().all(|t| EDITION_WORDS.contains(&t.as_str())) {
                best = Some(NameMatch::Edition);
                continue;
            }
            let why = format!(
                "a different game: it has \"{}\" after the name",
                extra.join(" ")
            );
            if best.is_none() {
                best = Some(NameMatch::Different(why));
            }
            continue;
        }
        if want.len() > rel.len() && want[..rel.len()] == rel[..] && !rel.is_empty() {
            let extra = &want[rel.len()..];
            if extra.iter().all(|t| EDITION_WORDS.contains(&t.as_str())) {
                best = Some(NameMatch::Edition);
                continue;
            }
        }
        if best.is_none() {
            best = Some(NameMatch::Different(format!(
                "a different game: \"{}\"",
                rel.join(" ")
            )));
        }
    }
    best.unwrap_or(NameMatch::Different("no name to compare with".into()))
}

/// What the user wants, in order of preference.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Profile {
    /// Preferred regions, best first. A region that isn't listed is refused unless `any_region`.
    pub regions: Vec<Region>,
    pub any_region: bool,
    /// Preferred formats, best first.
    pub formats: Vec<Format>,
    /// Smallest and biggest sizes accepted for a disc image, in MB.
    pub min_iso_mb: u64,
    pub max_iso_mb: u64,
    /// Minimum score to grab automatically (0-200).
    pub min_score: i32,
    pub prefer_proper: bool,
    /// Words that reject a release (case-insensitive), such as "jtag".
    pub reject_words: Vec<String>,
}

impl Default for Profile {
    fn default() -> Self {
        Profile {
            regions: vec![
                Region::Usa,
                Region::Free,
                Region::Pal,
                Region::Unknown,
                Region::Japan,
            ],
            any_region: true,
            formats: vec![Format::Iso, Format::God, Format::Package],
            min_iso_mb: 1500,
            max_iso_mb: 25_000,
            min_score: 90,
            prefer_proper: true,
            reject_words: vec![],
        }
    }
}

/// The verdict on one release.
#[derive(Debug, Clone, Serialize)]
pub struct Verdict {
    pub parsed: Parsed,
    pub score: i32,
    /// Why it is refused, if it is.
    pub rejected: Vec<String>,
}

pub struct Candidate<'a> {
    pub title: &'a str,
    pub size: u64,
    /// Newznab category ids.
    pub categories: &'a [u32],
    pub age_days: Option<i64>,
    pub grabs: Option<u32>,
    pub indexer_priority: i32,
    /// For a torrent: how many people are sharing it.
    pub seeders: Option<u32>,
}

/// Judge a release for a wanted game.
pub fn judge(c: &Candidate, wanted_names: &[String], p: &Profile) -> Verdict {
    let parsed = parse(c.title);
    let mut rejected = Vec::new();
    let in_360_category = c.categories.contains(&1050);
    if let Some(o) = &parsed.other_platform
        && !parsed.is_360
    {
        rejected.push(format!("Not for the Xbox 360 ({o})"));
    } else if !parsed.is_360 && !in_360_category {
        rejected.push("Doesn't say it is for the Xbox 360".into());
    }
    if let Some(n) = &parsed.not_a_game {
        rejected.push(format!("This is {n}, not the game"));
    }
    if parsed.fragment {
        rejected.push("A fragment of a multi-part post, not the whole release".into());
    }
    let lower = c.title.to_lowercase();
    for w in &p.reject_words {
        if !w.trim().is_empty() && lower.contains(&w.trim().to_lowercase()) {
            rejected.push(format!("Contains \"{}\"", w.trim()));
        }
    }
    let m = match_name(&parsed.name, wanted_names);
    let mut score = match &m {
        NameMatch::Exact => 100,
        NameMatch::Edition => 92,
        NameMatch::Different(why) => {
            rejected.push(format!("Not this game: {why}"));
            0
        }
    };
    if parsed.format == Format::Iso {
        let mb = c.size / 1_048_576;
        if c.size > 0 && mb < p.min_iso_mb {
            rejected.push(format!("Only {mb} MB: too small to be a disc image"));
        }
        if mb > p.max_iso_mb {
            rejected.push(format!(
                "{mb} MB: bigger than the {} MB limit",
                p.max_iso_mb
            ));
        }
    }
    if !p.formats.contains(&parsed.format) {
        rejected.push(format!(
            "A format you don't want ({})",
            match parsed.format {
                Format::Iso => "disc image",
                Format::God => "Games on Demand",
                Format::Package => "Arcade package",
            }
        ));
    } else if let Some(i) = p.formats.iter().position(|f| *f == parsed.format) {
        score += 15 - (i as i32 * 5).min(15);
    }
    match p.regions.iter().position(|r| *r == parsed.region) {
        Some(i) => score += 20 - (i as i32 * 4).min(20),
        None if !p.any_region => rejected.push("A region you don't want".into()),
        None => {}
    }
    if parsed.proper && p.prefer_proper {
        score += 5;
    }
    if let Some(g) = c.grabs {
        score += (g as i32 / 50).min(5);
    }
    // A torrent no one is sharing never finishes; a well-seeded one is quick and complete.
    match c.seeders {
        Some(0) => rejected.push("No one is sharing it (0 seeders)".into()),
        Some(n) => score += (n as i32 / 3).min(10),
        None => {}
    }
    if let Some(a) = c.age_days {
        score -= (a / 365).clamp(0, 5) as i32; // very old posts are likelier to be incomplete
    }
    score -= c.indexer_priority.clamp(0, 50) / 5;
    if parsed.disc.is_some() {
        score -= 3; // a single disc of a multi-disc game: fine, but the whole set is better
    }
    Verdict {
        parsed,
        score,
        rejected,
    }
}

/// The searches to try for a game: its name, with the numeral spelled the other way ("Fable II"
/// and "Fable 2"), and without trailing edition words ("Halo 3: Legendary Edition" and "Halo 3").
pub fn query_variants(names: &[String]) -> Vec<String> {
    const ROMAN: [&str; 11] = [
        "", "", "ii", "iii", "iv", "v", "vi", "vii", "viii", "ix", "x",
    ];
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: String| {
        let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
        if !s.is_empty() && !out.iter().any(|x| x.eq_ignore_ascii_case(&s)) {
            out.push(s);
        }
    };
    for n in names {
        push(n.clone());
        // Without trailing edition words ("Game of the Year" and the like first, then single words).
        const TAIL: &[&str] = &[
            "edition",
            "legendary",
            "collectors",
            "collector",
            "limited",
            "special",
            "ultimate",
            "complete",
            "deluxe",
            "gold",
            "premium",
            "definitive",
            "anniversary",
            "remastered",
            "hd",
            "platinum",
            "hits",
            "greatest",
            "classics",
        ];
        let phrase_free = crate::igdb::rank::strip_edition(n);
        let toks: Vec<String> = phrase_free.split_whitespace().map(String::from).collect();
        let mut end = toks.len();
        while end > 1
            && TAIL.contains(
                &norm(toks[end - 1].trim_matches(|c: char| !c.is_alphanumeric())).as_str(),
            )
        {
            end -= 1;
        }
        let base = toks[..end]
            .join(" ")
            .trim_end_matches([':', '-', ' '])
            .to_string();
        push(base.clone());
        // The numeral the other way round, for the last number-like word of the base.
        let btoks: Vec<&str> = base.split_whitespace().collect();
        if let Some(last) = btoks.last() {
            let l = norm(last);
            let swapped = if let Some(i) = ROMAN.iter().position(|r| !r.is_empty() && *r == l) {
                Some(i.to_string())
            } else {
                l.parse::<usize>()
                    .ok()
                    .filter(|n| (2..=10).contains(n))
                    .map(|n| ROMAN[n].to_uppercase())
            };
            if let Some(w) = swapped {
                let mut t: Vec<String> = btoks[..btoks.len() - 1]
                    .iter()
                    .map(|x| x.to_string())
                    .collect();
                t.push(w);
                push(t.join(" "));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_codes_after_a_region_are_noise_but_game_words_are_not() {
        let n = |t: &str| parse(t).name;
        assert_eq!(
            n("Halo 3 (USA, Europe) (En,Ja,Fr,De,Es,It,Pt,Zh,Ko)"),
            "halo 3"
        );
        assert_eq!(n("Fable II (USA) (En)"), "fable ii");
        assert_eq!(n("No More Heroes (USA)"), "no more heroes");
        assert_eq!(n("It Came from Outer Space"), "it came from outer space");
        assert!(parse("Halo 3 (Beta) (USA)").not_a_game.is_some());
    }

    fn names(n: &str) -> Vec<String> {
        crate::igdb::rank::variants(n)
    }

    fn j(title: &str, wanted: &str, size_mb: u64) -> Verdict {
        judge(
            &Candidate {
                title,
                size: size_mb * 1_048_576,
                categories: &[1000, 1050],
                age_days: None,
                grabs: None,
                indexer_priority: 25,
                seeders: None,
            },
            &names(wanted),
            &Profile::default(),
        )
    }

    #[test]
    fn real_release_names_are_taken_apart() {
        let p = parse("Gears.of.War.3.XGD3.PAL.SPANiSH.XBOX360-FBi");
        assert_eq!(
            (p.name.as_str(), p.region, p.format, p.group.as_deref()),
            ("gears of war 3", Region::Pal, Format::Iso, Some("FBi"))
        );
        let p = parse("Gears of War 3 READNFO XGD3 0800 USA RF-XBOX360-RRoD");
        assert_eq!(
            (p.name.as_str(), p.region),
            ("gears of war 3", Region::Free),
            "USA RF is region free"
        );
        let p = parse("Halo 3 XBOX360-CCCLX");
        assert_eq!(
            (p.name.as_str(), p.region, p.is_360),
            ("halo 3", Region::Unknown, true)
        );
        let p = parse("Halo.4.FRENCH.RF.X360-disc2");
        assert_eq!(
            (p.name.as_str(), p.disc, p.region),
            ("halo 4", Some(2), Region::Free)
        );
        let p = parse("Forza.Motorsport.4.Racing.GOTY.PAL.XBOX360-iNSOMNi");
        assert_eq!(p.name, "forza motorsport 4 racing goty");
        assert!(parse("Gears Of War 2 [MULTI][XBOX360].part01").fragment);
        assert_eq!(parse("Some.Game.XBLA.XBOX360-GRP").format, Format::Package);
        assert_eq!(
            parse("Spartacus Legends XBLA X360-NeXt").name,
            "spartacus legends"
        );
        assert_eq!(parse("Some.Game.GoD.XBOX360-GRP").format, Format::God);
        assert_eq!(
            parse("Gears.of.War.3.PS3-GRP").other_platform.as_deref(),
            Some("PS3")
        );
        assert_eq!(
            parse("Game.XBOX-GRP").other_platform.as_deref(),
            Some("original Xbox")
        );
        assert!(parse("Fable.II.DLC.Pack.XBOX360-GRP").not_a_game.is_some());
        assert!(
            parse("Halo.3.Title.Update.5.XBOX360-GRP")
                .not_a_game
                .is_some()
        );
    }

    #[test]
    fn the_right_game_is_accepted_and_neighbours_are_refused() {
        // Searching "Halo 3" also finds these; only the first two are Halo 3.
        assert!(
            j("Halo 3 XBOX360-CCCLX", "Halo 3", 7193)
                .rejected
                .is_empty()
        );
        assert!(
            j("Halo.3.PAL.XBOX360-GAC", "Halo 3", 7139)
                .rejected
                .is_empty()
        );
        for other in [
            "Halo.Reach.PAL.SPANiSH.XBOX360-TibuRon",
            "Halo.4.FRENCH.RF.X360-disc1",
            "Halo.3.ODST.X360-Allstars",
            "Halo.Combat.Evolved.Anniversary.XBOX360-COMPLEX",
        ] {
            let v = j(other, "Halo 3", 8000);
            assert!(
                v.rejected.iter().any(|r| r.contains("Not this game")),
                "{other}: {:?}",
                v.rejected
            );
        }
        // Sequels are not the original.
        assert!(
            !j("Gears.of.War.3.XBOX360", "Gears of War", 9000)
                .rejected
                .is_empty()
        );
        assert!(
            !j("Gears.of.War.2.XBOX360", "Gears of War", 7886)
                .rejected
                .is_empty()
        );
        assert!(
            j(
                "Gears Of War READNFO MULTi2 NTSC XBOX360-SuperX360",
                "Gears of War",
                7397
            )
            .rejected
            .is_empty()
        );
        // Roman numerals and "The".
        assert!(
            j("Fable.2.XBOX360-GRP", "Fable II", 7000)
                .rejected
                .is_empty()
        );
        assert!(
            j("The.Simpsons.Game.XBOX360-GRP", "Simpsons Game", 7000)
                .rejected
                .is_empty()
        );
        // Edition words are fine.
        let v = j(
            "Gears.Of.War.2.Game.of.the.Year.Edition.RF.XBOX360-MOONWALKER-xpost",
            "Gears of War 2",
            7701,
        );
        assert!(v.rejected.is_empty(), "{:?}", v.rejected);
        assert!(
            v.score < j("Gears.of.War.2.RF.XBOX360-GRP", "Gears of War 2", 7701).score,
            "an exact name ranks above an edition"
        );
    }

    #[test]
    fn searches_cover_numerals_and_edition_words() {
        let q = |n: &str| query_variants(&[n.to_string()]);
        assert_eq!(q("Fable II"), vec!["Fable II", "Fable 2"]);
        assert_eq!(
            q("Gears of War 3"),
            vec!["Gears of War 3", "Gears of War III"]
        );
        assert_eq!(
            q("Halo 3: Legendary Edition"),
            vec!["Halo 3: Legendary Edition", "Halo 3", "Halo III"]
        );
        assert_eq!(
            q("Forza Motorsport 4"),
            vec!["Forza Motorsport 4", "Forza Motorsport IV"]
        );
        assert_eq!(q("The Simpsons Game"), vec!["The Simpsons Game"]);
        // Matching still treats the numerals as the same game, and the edition words as extra.
        assert!(
            j("Fable.2.XBOX360-GRP", "Fable II", 7000)
                .rejected
                .is_empty()
        );
        assert!(
            j("Halo 3 XBOX360-CCCLX", "Halo 3: Legendary Edition", 7193)
                .rejected
                .is_empty()
        );
        assert!(
            !j(
                "Halo.3.ODST.X360-Allstars",
                "Halo 3: Legendary Edition",
                7485
            )
            .rejected
            .is_empty()
        );
    }

    #[test]
    fn arcade_games_are_accepted_but_dlc_and_updates_are_not() {
        let v = j(
            "Spartacus Legends XBLA X360-NeXt",
            "Spartacus Legends",
            2405,
        );
        assert!(v.rejected.is_empty(), "{:?}", v.rejected);
        assert!(
            !j(
                "Spartacus.Legends.DLC.Pack.XBLA.X360-NeXt",
                "Spartacus Legends",
                300
            )
            .rejected
            .is_empty()
        );
        let no_arcade = Profile {
            formats: vec![Format::Iso],
            ..Default::default()
        };
        let v = judge(
            &Candidate {
                title: "Spartacus Legends XBLA X360-NeXt",
                size: 2_000_000_000,
                categories: &[1050],
                age_days: None,
                grabs: None,
                indexer_priority: 0,
                seeders: None,
            },
            &names("Spartacus Legends"),
            &no_arcade,
        );
        assert!(v.rejected.iter().any(|r| r.contains("Arcade package")));
    }

    #[test]
    fn size_platform_dlc_and_fragments_are_checked() {
        assert!(
            j("Halo 3 XBOX360-CCCLX", "Halo 3", 300)
                .rejected
                .iter()
                .any(|r| r.contains("too small"))
        );
        assert!(
            j("Halo 3 XBOX360-CCCLX", "Halo 3", 60_000)
                .rejected
                .iter()
                .any(|r| r.contains("bigger"))
        );
        assert!(
            j("Halo.3.PS3-GRP", "Halo 3", 7000)
                .rejected
                .iter()
                .any(|r| r.contains("PS3"))
        );
        assert!(
            j("Halo.3.DLC.Map.Pack.XBOX360-GRP", "Halo 3", 3000)
                .rejected
                .iter()
                .any(|r| r.contains("DLC"))
        );
        assert!(
            j(
                "Gears Of War 2 [MULTI][XBOX360].part01",
                "Gears of War 2",
                751
            )
            .rejected
            .iter()
            .any(|r| r.contains("fragment"))
        );
        // No platform word, but the indexer's category says Xbox 360.
        assert!(j("Halo 3", "Halo 3", 7000).rejected.is_empty());
        let v = judge(
            &Candidate {
                title: "Halo 3",
                size: 7_000_000_000,
                categories: &[],
                age_days: None,
                grabs: None,
                indexer_priority: 25,
                seeders: None,
            },
            &names("Halo 3"),
            &Profile::default(),
        );
        assert!(v.rejected.iter().any(|r| r.contains("Xbox 360")));
    }

    #[test]
    fn the_profile_decides_what_ranks_first() {
        let usa = j(
            "Forza Motorsport 4 USA XBOX360-ZRY",
            "Forza Motorsport 4",
            18358,
        );
        let pal = j(
            "Forza.Motorsport.4.PAL.XBOX360-COMPLEX",
            "Forza Motorsport 4",
            18429,
        );
        let jpn = j(
            "Forza Motorsport 4 JPN XBOX360-Caravan",
            "Forza Motorsport 4",
            18439,
        );
        assert!(
            usa.score > pal.score && pal.score > jpn.score,
            "{} {} {}",
            usa.score,
            pal.score,
            jpn.score
        );
        let strict = Profile {
            any_region: false,
            regions: vec![Region::Pal],
            ..Default::default()
        };
        let v = judge(
            &Candidate {
                title: "Forza Motorsport 4 USA XBOX360-ZRY",
                size: 18_000_000_000,
                categories: &[1050],
                age_days: None,
                grabs: None,
                indexer_priority: 0,
                seeders: None,
            },
            &names("Forza Motorsport 4"),
            &strict,
        );
        assert!(v.rejected.iter().any(|r| r.contains("region")));
        let p = Profile {
            reject_words: vec!["hdd".into()],
            ..Default::default()
        };
        assert!(
            judge(
                &Candidate {
                    title: "Halo.3.HDD.XBOX360-X",
                    size: 8_000_000_000,
                    categories: &[1050],
                    age_days: None,
                    grabs: None,
                    indexer_priority: 0,
                    seeders: None,
                },
                &names("Halo 3"),
                &p
            )
            .rejected
            .iter()
            .any(|r| r.contains("hdd"))
        );
    }
}
