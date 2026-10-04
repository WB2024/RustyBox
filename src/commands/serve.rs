use std::{net::SocketAddr, path::PathBuf};

use clap::Args;

use crate::{error::Error, web};

#[derive(Args, Debug)]
pub struct ServeArgs {
    /// Address to listen on
    #[arg(long, env = "RUSTYBOX_BIND", default_value = "0.0.0.0:8080")]
    pub bind: SocketAddr,

    /// Also answer over HTTPS here (self-signed certificate; a browser needs HTTPS to share a
    /// folder of this computer, such as the Xbox drive). `off` turns it off.
    #[arg(long, env = "RUSTYBOX_TLS_BIND", default_value = "0.0.0.0:8443")]
    pub tls_bind: String,

    /// Where settings and the index are stored (mount a volume here in Docker)
    #[arg(long, env = "RUSTYBOX_CONFIG_DIR", default_value = "./config")]
    pub config_dir: PathBuf,

    /// Folders library paths may live under, separated by ':' (mounted disks and shares)
    #[arg(
        long,
        env = "RUSTYBOX_ROOTS",
        value_delimiter = ':',
        default_value = "/data:/mnt:/media:/srv"
    )]
    pub roots: Vec<PathBuf>,

    /// Where the abgx360 program is (default: found on the PATH)
    #[arg(long, env = "RUSTYBOX_ABGX360")]
    pub abgx360: Option<PathBuf>,

    /// Require a login (set together with --auth-password). Otherwise the login can be set in Settings.
    #[arg(long, env = "RUSTYBOX_AUTH_USER")]
    pub auth_user: Option<String>,

    /// Password for --auth-user
    #[arg(long, env = "RUSTYBOX_AUTH_PASSWORD", hide_env_values = true)]
    pub auth_password: Option<String>,

    /// Simulate the console, USB drives and network, for trying the UI without hardware
    #[arg(long, env = "RUSTYBOX_MOCK", value_parser = clap::builder::BoolishValueParser::new())]
    pub mock: bool,
}

/// Read `KEY=VALUE` lines from a `.env` file (development). Comments and blank lines are skipped, and
/// quotes around a value are removed. A missing or unreadable file gives an empty map.
pub fn read_dotenv(path: &std::path::Path) -> std::collections::HashMap<String, String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Default::default();
    };
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.is_empty() || l.starts_with('#') {
                return None;
            }
            let (k, v) = l.strip_prefix("export ").unwrap_or(l).split_once('=')?;
            let v = v.trim().trim_matches(|c| c == '"' || c == '\'');
            Some((k.trim().to_string(), v.to_string()))
        })
        .collect()
}

pub fn run(args: ServeArgs) -> Result<(), Error> {
    std::fs::create_dir_all(&args.config_dir)?;
    let cfg = web::Config {
        config_dir: args.config_dir,
        abgx360: args.abgx360,
        dotenv: read_dotenv(std::path::Path::new(".env")),
        igdb_urls: None,
        roots: args
            .roots
            .into_iter()
            .filter(|r| !r.as_os_str().is_empty())
            .collect(),
        mock: args.mock,
        auth: match (args.auth_user, args.auth_password) {
            (None, None) => None,
            (u, p) => Some((u.unwrap_or_default(), p.unwrap_or_default())),
        },
    };
    let rt = tokio::runtime::Runtime::new()?;
    let tls = match args.tls_bind.trim() {
        "" | "off" | "false" | "0" => None,
        a => Some(a.parse::<SocketAddr>().map_err(|e| {
            Error::validation(format!(
                "--tls-bind must look like 0.0.0.0:8443 or off: {e}"
            ))
        })?),
    };
    rt.block_on(web::serve_with_tls(cfg, args.bind, tls))
}

#[cfg(test)]
mod tests {
    use super::read_dotenv;

    #[test]
    fn reads_simple_env_files() {
        let d = std::env::temp_dir().join(format!("rustybox_env_{}", std::process::id()));
        std::fs::write(&d, "# comment\n\nclientid=abc123\nexport clientsecret = \"s e\"\nbroken line\nquoted='x'\n").unwrap();
        let m = read_dotenv(&d);
        assert_eq!(
            (
                m.get("clientid").map(String::as_str),
                m.get("clientsecret").map(String::as_str),
                m.get("quoted").map(String::as_str)
            ),
            (Some("abc123"), Some("s e"), Some("x"))
        );
        assert_eq!(m.len(), 3);
        assert!(read_dotenv(std::path::Path::new("/nope/.env")).is_empty());
        let _ = std::fs::remove_file(&d);
    }
}
