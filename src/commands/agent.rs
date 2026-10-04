use std::{net::SocketAddr, path::PathBuf};

use argon2::password_hash::rand_core::{OsRng, RngCore};
use clap::Args;

use crate::{agent, error::Error, fsops};

#[derive(Args, Debug)]
pub struct AgentArgs {
    /// The folder to share: usually the mount point of the drive, e.g. /media/you/XBOX
    #[arg(long)]
    pub root: PathBuf,

    /// Address to listen on
    #[arg(long, env = "RUSTYBOX_AGENT_BIND", default_value = "0.0.0.0:8099")]
    pub bind: SocketAddr,

    /// The secret RustyBox must present. Default: one is made once and kept in ~/.config/rustybox-agent/token
    #[arg(long, env = "RUSTYBOX_AGENT_TOKEN", hide_env_values = true)]
    pub token: Option<String>,

    /// A name to show in RustyBox (default: this computer's name)
    #[arg(long)]
    pub name: Option<String>,

    /// Allow RustyBox to read but never change anything
    #[arg(long)]
    pub read_only: bool,
}

fn token_file() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("rustybox-agent").join("token"))
}

/// The saved token, or a new random one saved for next time.
fn load_or_make_token() -> Result<String, Error> {
    let path = token_file()
        .ok_or_else(|| Error::validation("Can't find a place to keep the token; pass --token"))?;
    if let Ok(t) = std::fs::read_to_string(&path) {
        let t = t.trim().to_string();
        if t.len() >= 16 {
            return Ok(t);
        }
    }
    let mut bytes = [0u8; 24];
    OsRng.fill_bytes(&mut bytes);
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, format!("{token}\n"))?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(token)
}

/// This machine's address on the local network (no packet is sent).
fn lan_ip() -> Option<String> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    Some(s.local_addr().ok()?.ip().to_string())
}

fn host_name() -> String {
    std::fs::read_to_string("/etc/hostname")
        .map(|h| h.trim().to_string())
        .ok()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "this computer".into())
}

pub fn run(args: AgentArgs) -> Result<(), Error> {
    let token = match args.token {
        Some(t) => t,
        None => load_or_make_token()?,
    };
    let name = args.name.unwrap_or_else(host_name);
    let state = agent::AgentState::new(&args.root, token.clone(), name.clone(), args.read_only)?;
    let info = fsops::fs_info(&state.root);
    println!(
        "RustyBox drive agent \"{name}\" sharing {}",
        state.root.display()
    );
    println!(
        "  {} filesystem, {:.1} GB free of {:.1} GB{}",
        info.fs_type,
        info.free as f64 / 1e9,
        info.total as f64 / 1e9,
        if args.read_only { ", read-only" } else { "" }
    );
    println!();
    println!("In RustyBox, add a Remote drive with:");
    println!(
        "  Address: http://{}:{}",
        lan_ip().unwrap_or_else(|| "<this computer's address>".into()),
        args.bind.port()
    );
    println!("  Token:   {token}");
    println!();
    println!(
        "If RustyBox can't connect, allow the port through this computer's firewall, for example:"
    );
    println!(
        "  sudo ufw allow from 192.168.1.0/24 to any port {} proto tcp",
        args.bind.port()
    );
    println!();
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(agent::serve(state, args.bind))
}
