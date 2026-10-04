# Installation

RustyBox is one small program (a Rust binary with the web interface built in). The easiest way to run it is the Docker image; it needs no database server and keeps everything in one `/config` folder.

## What you need

- A computer or server that is on whenever you want to use it (a Raspberry Pi 4 or better, a NAS, a mini PC, a VM or an LXC all work).
- Docker with Compose.
- Your games on disks that server can reach (local disks, or network shares mounted on it first).
- Optional: an [IGDB](https://api-docs.igdb.com) (Twitch) application for cover art and game details (free), SABnzbd and/or qBittorrent for downloads, an Xbox 360 running Aurora for sending games over FTP.

## 1. Run it with Docker Compose

Create a folder (for example `rustybox/`) with this `compose.yaml`:

```yaml
services:
  rustybox:
    image: wb20244/rustybox:latest
    container_name: rustybox
    restart: unless-stopped
    init: true
    ports:
      - "8088:8080"                 # the web interface: http://<server>:8088
      - "8443:8443"                 # the same over HTTPS: https://<server>:8443 (a certificate RustyBox makes itself)
    volumes:
      - ./config:/config            # settings, the index, cached covers
      - /path/to/your/games:/data/games     # change the left side; add one line per disk
    environment:
      RUSTYBOX_PUID: "1000"         # who owns the files RustyBox creates (id -u / id -g)
      RUSTYBOX_PGID: "1000"
      RUSTYBOX_UMASK: "002"
```

Start it and open the interface:

```bash
docker compose up -d
```

Then browse to `http://<server>:8088`. RustyBox only sees the folders you mount into the container, so mount every disk or share you want to use (network shares are mounted on the host first, then bind-mounted in; RustyBox has no network-share code of its own).

## 2. First-run checklist

1. **Libraries → New library.** Choose a type (ISO games, GOD games, mods, …), give it a name and add one or more folders from any disk. Press **Scan now**.
2. **Games** shows everything found, matched by title ID, with every place a copy lives.
3. **Settings → Game information**: paste your Twitch client ID and secret (steps are on the page) and press *Fetch game info* on the Games page to get cover art and details.
4. **Optional:** turn on a login (Settings → Security), add your console (Console), connect the Xbox's hard drive (Xbox drive), or set up downloads (Wanted → Setup).

## 3. Sending games to your console

On the Xbox 360, run Aurora (or any dashboard with an FTP server) and switch its FTP server on. In RustyBox, open **Console → Add** and enter the console's address. **Self-test** checks every step against a throwaway folder. Details: [Console](console.md).

## 4. The Xbox's external hard drive

A drive plugged into another computer is reached through a small **drive agent** on that computer; see [Drives and import](drives-and-import.md). The easy way to run it is Docker, with the drive mounted into the container:

```bash
docker run -d --name rustybox-agent --restart unless-stopped \
  -p 8099:8099 -e RUSTYBOX_AGENT_TOKEN='choose-a-long-secret-here' \
  -v /media/you/XBOXDRIVE:/drive \
  wb20244/rustybox:latest agent --root /drive
```

Then **Xbox drive** in RustyBox asks for that computer's address (`http://<pc>:8099`) and the token.

## 5. Downloads

- **Usenet:** indexers, SABnzbd and where finished games go are all in **Wanted → Setup**. See [Wanted](wanted.md).
- **Torrents:** qBittorrent plus torrent indexers, or folders of `.torrent` files on the **Torrents** page. See [Torrents](torrents.md), which includes a ready-made qBittorrent compose file.

## Updating

```bash
docker compose pull && docker compose up -d
```

Your settings and index are in `./config` and are upgraded automatically. Don't restart it while a job (conversion, transfer, import) is running; the Jobs page shows what is running.

## Backups

Everything RustyBox keeps is in `/config` (`rustybox.db`, `settings.json`, `grabber.json`, covers). Copy that folder to back it up. It contains secrets (API keys, drive tokens), so keep the copy private. Your games are never stored there.

## Building from source

```bash
git clone https://github.com/WB2024/RustyBox.git
cd RustyBox
cargo run --release -- serve --config-dir ./config --roots /path/to/your/games
```

(Rust 2024 edition, a recent stable toolchain.) `cargo run -- serve --mock` runs the whole interface with a pretend console, drives and network, which is a safe way to look around. For the optional ISO checker (`abgx360`) and USB tools, use the Docker image, which includes them.

## Security

By default anyone who can reach the port can use RustyBox. Keep it on your home network, or turn on **Settings → Security** (a user name and password, stored only as an Argon2 hash), or put it behind a reverse proxy that does its own login. Don't expose it to the internet without one of those.
