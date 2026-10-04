//! The title list: title ID (and media ID) to the game's name. Embedded in the binary.

use std::{collections::HashMap, sync::OnceLock};

const CSV: &str = include_str!("../../data/gamelist_xbox360.csv");

#[derive(Debug)]
struct Entry {
    media_id: String,
    name: String,
}

pub struct Catalog {
    by_title: HashMap<String, Vec<Entry>>,
}

impl Catalog {
    /// Parse the tab-separated list: `title_id  media_id  title_name`, one row per disc variant.
    pub fn parse(text: &str) -> Catalog {
        let mut by_title: HashMap<String, Vec<Entry>> = HashMap::new();
        for line in text.lines().skip(1) {
            let mut c = line.split('\t');
            let (Some(t), Some(m), Some(n)) = (c.next(), c.next(), c.next()) else {
                continue;
            };
            let (t, m, n) = (t.trim().to_uppercase(), m.trim().to_uppercase(), n.trim());
            if t.len() != 8 || n.is_empty() {
                continue;
            }
            by_title.entry(t).or_default().push(Entry {
                media_id: m,
                name: n.to_string(),
            });
        }
        Catalog { by_title }
    }

    /// The game's name. A matching media ID picks the right regional variant; otherwise the
    /// first listed name for the title is used.
    pub fn name(&self, title_id: &str, media_id: Option<&str>) -> Option<&str> {
        let entries = self.by_title.get(&title_id.to_uppercase())?;
        let exact =
            media_id.and_then(|m| entries.iter().find(|e| e.media_id.eq_ignore_ascii_case(m)));
        exact.or_else(|| entries.first()).map(|e| e.name.as_str())
    }

    /// Every known disc media ID for a title (one per regional variant). Empty means unknown.
    pub fn media_ids(&self, title_id: &str) -> Vec<String> {
        let mut v: Vec<String> = self
            .by_title
            .get(&title_id.to_uppercase())
            .map(|es| {
                es.iter()
                    .map(|e| e.media_id.clone())
                    .filter(|m| !m.is_empty() && m != "00000000")
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v.dedup();
        v
    }

    pub fn len(&self) -> usize {
        self.by_title.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_title.is_empty()
    }
}

pub fn get() -> &'static Catalog {
    static C: OnceLock<Catalog> = OnceLock::new();
    C.get_or_init(|| Catalog::parse(CSV))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_id_picks_the_regional_variant() {
        let c = Catalog::parse(
            "title_id\tmedia_id\ttitle_name\n4D530805\tAAAAAAAA\tAlan Wake (USA)\n4D530805\tBBBBBBBB\tAlan Wake (Europe)\nbad line\n",
        );
        assert_eq!(
            c.name("4d530805", Some("bbbbbbbb")),
            Some("Alan Wake (Europe)")
        );
        assert_eq!(
            c.name("4D530805", Some("CCCCCCCC")),
            Some("Alan Wake (USA)")
        );
        assert_eq!(c.name("4D530805", None), Some("Alan Wake (USA)"));
        assert_eq!(c.name("FFFFFFFF", None), None);
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn the_embedded_list_loads() {
        let c = get();
        assert!(c.len() > 3000, "{} titles", c.len());
        assert!(c.name("465307D3", None).unwrap().contains("eNCHANT"));
    }
}
