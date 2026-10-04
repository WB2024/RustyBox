//! User settings, stored as JSON in the config directory so they survive restarts
//! (mount a volume there when running in Docker).

use std::{
    path::{Path, PathBuf},
    sync::RwLock,
};

use serde::{Deserialize, Serialize};

pub const LAYOUTS: &[&str] = &["name_titleid", "titleid"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// Login set from the Settings page. Empty password hash = no login.
    pub auth_user: String,
    pub auth_password_hash: String,
    /// Worker threads for ISO to GOD conversion.
    pub convert_threads: u32,
    /// Where converted GOD games go in a library: `name_titleid` (Game Name/TitleID/...) or `titleid`.
    pub god_layout: String,
    /// IGDB (Twitch) application credentials, for cover art and game information.
    pub igdb_client_id: String,
    pub igdb_client_secret: String,
    /// Look up new games automatically after a scan or conversion.
    pub igdb_auto: bool,
    /// Rescan every library this often, in minutes (0 = never).
    pub scan_interval_minutes: u32,
    /// Where to tell about finished jobs (ntfy, Discord, Slack or anything taking JSON). Secret.
    pub notify_url: String,
    /// `failures`, `all` or `off`.
    pub notify_on: String,
}

pub const NOTIFY_MODES: &[&str] = &["failures", "all", "off"];

fn web_url(label: &str, v: &str, allow_path: bool) -> Result<(), String> {
    if v.is_empty() {
        return Ok(());
    }
    let rest = v
        .strip_prefix("http://")
        .or_else(|| v.strip_prefix("https://"))
        .ok_or_else(|| format!("{label} must start with http:// or https://"))?;
    if rest.is_empty()
        || rest.contains([' ', '\n'])
        || (!allow_path && rest.trim_end_matches('/').contains('/'))
    {
        return Err(format!(
            "{label} looks wrong{}",
            if allow_path {
                ""
            } else {
                " (use just the address and port)"
            }
        ));
    }
    Ok(())
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            auth_user: String::new(),
            auth_password_hash: String::new(),
            convert_threads: 2,
            god_layout: "name_titleid".into(),
            igdb_client_id: String::new(),
            igdb_client_secret: String::new(),
            igdb_auto: true,
            scan_interval_minutes: 0,
            notify_url: String::new(),
            notify_on: "failures".into(),
        }
    }
}

impl Settings {
    pub fn validate(mut self) -> Result<Settings, String> {
        if !(1..=16).contains(&self.convert_threads) {
            return Err("Use between 1 and 16 conversion threads".into());
        }
        if !LAYOUTS.contains(&self.god_layout.as_str()) {
            return Err(format!("Unknown GOD layout '{}'", self.god_layout));
        }
        if self.scan_interval_minutes != 0 && !(5..=10_080).contains(&self.scan_interval_minutes) {
            return Err("Scan every 5 minutes to a week, or 0 for never".into());
        }
        if !NOTIFY_MODES.contains(&self.notify_on.as_str()) {
            return Err(format!("Unknown notification setting '{}'", self.notify_on));
        }
        self.notify_url = self.notify_url.trim().to_string();
        web_url("The notification address", &self.notify_url, true)?;
        self.auth_user = self.auth_user.trim().to_string();
        self.igdb_client_id = self.igdb_client_id.trim().to_string();
        self.igdb_client_secret = self.igdb_client_secret.trim().to_string();
        for (what, v) in [
            ("Client ID", &self.igdb_client_id),
            ("Client Secret", &self.igdb_client_secret),
        ] {
            if v.len() > 100 || v.chars().any(|c| c.is_whitespace() || c.is_control()) {
                return Err(format!("That doesn't look like an IGDB {what}"));
            }
        }
        Ok(self)
    }

    /// The settings as shown to the browser: no secrets.
    pub fn public(&self) -> serde_json::Value {
        let mut v = serde_json::to_value(self).unwrap_or_default();
        if let Some(o) = v.as_object_mut() {
            o.remove("auth_password_hash");
            o.remove("auth_user");
            o.remove("igdb_client_secret");
            o.remove("notify_url");
            o.insert(
                "notify_url_set".into(),
                serde_json::json!(!self.notify_url.is_empty()),
            );
            let host = self
                .notify_url
                .split("//")
                .nth(1)
                .and_then(|r| r.split('/').next())
                .unwrap_or("");
            o.insert("notify_url_host".into(), serde_json::json!(host));
            let key = &self.igdb_client_secret;
            o.insert(
                "igdb_client_secret_set".into(),
                serde_json::json!(!key.is_empty()),
            );
            let hint = if key.is_empty() {
                String::new()
            } else {
                tail_hint(key)
            };
            o.insert("igdb_client_secret_hint".into(), serde_json::json!(hint));
        }
        v
    }
}

pub struct Store {
    path: PathBuf,
    current: RwLock<Settings>,
}

impl Store {
    /// Load `dir/settings.json`; a missing or unreadable file gives the defaults.
    pub fn load(dir: &Path) -> Store {
        let path = dir.join("settings.json");
        let current = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Settings>(&b).ok())
            .and_then(|s| s.validate().ok())
            .unwrap_or_default();
        Store {
            path,
            current: RwLock::new(current),
        }
    }

    pub fn get(&self) -> Settings {
        self.current
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn set(&self, new: Settings) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Private from the first byte (it holds secrets), and a crash can't leave half a file.
        crate::fsops::write_private(&self.path, &serde_json::to_vec_pretty(&new)?)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        *self.current.write().unwrap_or_else(|e| e.into_inner()) = new;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// `••••` and the last four characters of a longer value, to recognise it by; a short one shows
/// nothing of itself. (Counts characters, not bytes, so a non-ASCII value can't panic.)
pub fn tail_hint(v: &str) -> String {
    let n = v.chars().count();
    if n >= 12 {
        let tail: String = v.chars().skip(n - 4).collect();
        format!("••••{tail}")
    } else {
        "••••".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("rustybox_settings_{}_{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn defaults_when_no_file() {
        assert_eq!(Store::load(&tmpdir("none")).get(), Settings::default());
    }

    #[test]
    fn saves_reloads_and_hides_secrets() {
        let d = tmpdir("roundtrip");
        let store = Store::load(&d);
        let s = Settings {
            auth_user: "me".into(),
            auth_password_hash: "$argon2id$x".into(),
            ..Default::default()
        };
        store.set(s.clone()).unwrap();
        assert_eq!(Store::load(&d).get(), s);
        let public = s.public();
        assert!(public.get("auth_password_hash").is_none() && public.get("auth_user").is_none());
        let with_key = Settings {
            igdb_client_secret: "abcdef1234567890".into(),
            ..s.clone()
        }
        .public();
        assert!(with_key.get("igdb_client_secret").is_none());
        assert_eq!(
            (
                with_key["igdb_client_secret_set"].as_bool(),
                with_key["igdb_client_secret_hint"].as_str()
            ),
            (Some(true), Some("••••7890"))
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn validation_rejects_bad_values() {
        assert!(
            Settings {
                convert_threads: 0,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            Settings {
                convert_threads: 99,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            Settings {
                god_layout: "nope".into(),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            Settings {
                convert_threads: 4,
                god_layout: "titleid".into(),
                ..Default::default()
            }
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn corrupt_file_falls_back_to_defaults() {
        let d = tmpdir("corrupt");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("settings.json"), b"{ not json").unwrap();
        assert_eq!(Store::load(&d).get(), Settings::default());
        let _ = std::fs::remove_dir_all(&d);
    }
}
