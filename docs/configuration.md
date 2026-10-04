# Configuration reference

Most things are set in the web interface (Settings, Wanted → Setup, Console, Libraries). This page lists the container-level options.

## Environment variables and flags

| Variable | Flag | Default | What it does |
|---|---|---|---|
| `RUSTYBOX_BIND` | `--bind` | `0.0.0.0:8080` | Address the web interface listens on (inside the container). |
| `RUSTYBOX_CONFIG_DIR` | `--config-dir` | `/config` in Docker, `./config` otherwise | Where settings, the index (`rustybox.db`) and cached covers live. Mount a volume here. |
| `RUSTYBOX_ROOTS` | `--roots` | `/data:/mnt:/media:/srv` | Folders library paths may be under, separated by `:`. Anything outside is refused. |
| `RUSTYBOX_PUID` / `RUSTYBOX_PGID` | | not set | Owner and group given to files RustyBox creates (`PUID`/`PGID` also work). |
| `RUSTYBOX_UMASK` | | `002` once an owner is set | Permission bits withheld from new files. |
| `RUSTYBOX_AUTH_USER` / `RUSTYBOX_AUTH_PASSWORD` | `--auth-user` / `--auth-password` | not set | Require a login (set both, or neither). Takes priority over the login set in Settings. |
| `IGDB_CLIENT_ID` / `IGDB_CLIENT_SECRET` | | not set | Twitch application for cover art and game details, instead of entering them in Settings. |
| `RUSTYBOX_ABGX360` | `--abgx360` | found on `PATH` | Where the `abgx360` ISO checker is. |
| `RUSTYBOX_MOCK` | `--mock` | off | Pretend console, USB drives and network, for trying things out. |

## The drive agent (`rustybox agent`)

| Variable | Flag | Default | What it does |
|---|---|---|---|
| | `--root` | required | The folder to share (an Xbox drive's mount point). |
| `RUSTYBOX_AGENT_BIND` | `--bind` | `0.0.0.0:8099` | Address to listen on. |
| `RUSTYBOX_AGENT_TOKEN` | `--token` | made once, kept in `~/.config/rustybox-agent/token` | The secret RustyBox presents. At least 16 characters. |
| | `--name` | this computer's name | Name shown in RustyBox. |
| | `--read-only` | off | Allow reading but never changing anything. |

## Files in `/config`

| File | Holds |
|---|---|
| `rustybox.db` | The index of your libraries, wanted list, downloads, job history (SQLite). |
| `settings.json` | General settings and secrets (owner-only permissions). |
| `grabber.json` | Indexers, SABnzbd, qBittorrent, the quality profile (owner-only; holds API keys). |
| `consoles.json` | Your consoles, including FTP passwords (owner-only). |
| `covers/` | Cached cover art. |

## Ports

| Port | What |
|---|---|
| `8080` (container) | The web interface (map it, for example `8088:8080`). |
| `8099` | The drive agent, on the computer the Xbox drive is plugged into. |

## USB tools (Bad Avatar sticks, formatting, backups)

These need the host's USB devices. Either run the container privileged (`privileged: true`), or pass one stick through (`devices: ["/dev/sdb:/dev/sdb"]`). RustyBox only ever offers removable USB sticks it detects, asks you to type the device path back, and never touches the Xbox's own drive. See [USB and extras](usb-and-extras.md).

## Health check

The image includes a Docker health check (`rustybox healthcheck`) that succeeds when the local server answers.
