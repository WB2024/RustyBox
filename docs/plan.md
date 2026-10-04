# RustyBox plan

A self-hosted web app (Rust, Docker, LXC-friendly) for managing everything around a modded Xbox 360: game libraries, ISO/GOD conversion, transfers to the console, mods, title updates and Bad Avatar USB sticks.

**Status key:** ⬜ not started · 🔧 in progress · ✅ complete

## Decisions

| Topic | Decision | Why |
|---|---|---|
| Storage | RustyBox only ever sees **paths inside the container**. Host disks are bind-mounted; NFS shares are mounted on the Proxmox host (or LXC) and bind-mounted in. No SMB/NFS code in the app. | Simplest, fastest, keeps the container unprivileged. A "path" is a path, wherever it really lives. |
| Index | **SQLite** (`rusqlite`, bundled, WAL mode) in `/config/rustybox.db`. | One file, no server, fast queries across tens of thousands of files, crash-safe. Settings stay in `settings.json` like DiscCTL. |
| Frontend | **No build step.** Vanilla JS ES modules + CSS, one file per page, embedded in the binary (`rust-embed`), served with caching. Same look as RustyDisc. | DiscCTL's 3k-line single HTML won't scale to this many pages; a framework adds a toolchain for little gain. |
| Jobs | DiscCTL's model (replayable event log, SSE, cancel, awaiting-input) but **in-process tokio tasks** for Rust-native work, child processes only for external tools. Locks are **per resource** (a console, a USB device, a library path), not one global drive lock. | Native work (GOD conversion, FTP) needs no process boundary; several unrelated jobs can run at once. |
| Rust-native first | `iso2god-rs` (MIT) as a crate, `god2iso` ported, STFS/title-ID parsing ported, FTP via a Rust crate. Shell out only to `extract-xiso`, `partclone`/`zstd`, `mkfs.vfat` and similar. `abgx360` (GPL) is **only ever a separate binary**. | Fewer moving parts in the image, real progress reporting, no licence problems. |
| Auth, errors, mock mode, PUID/PGID/UMASK | Carried over from DiscCTL unchanged in spirit. | Proven. |

## Libraries

A **library** has a name, a **kind**, and one or more **paths**. Paths can be on different disks or machines; the app presents them as one.

- Kinds: `iso`, `god`, `mods`, `cheats`, `trainers`, `homebrew`, `saves`, `patches` (title updates), `emulators`, `custom`. A kind picks the scanner (what counts as an item, how its title ID is found) and the default layout. Everything is configurable, and `custom` is plain files.
- Each path has a label, a **read-only/writable** flag and a priority. Writes pick a writable path by the user's choice (default: most free space). Paths that disappear (an unmounted NFS share) show as **offline**; items stay in the index, marked unavailable, and nothing is deleted.
- Scans are incremental (path + size + mtime), run as jobs, and are triggered manually, on a schedule, and after RustyBox's own writes. NFS has no reliable inotify, so we don't depend on it.
- Items resolve to a Xbox 360 **title ID / media ID / name** using the bundled `gamelist_xbox360.csv`, STFS/XEX headers, and GOD headers. Duplicates across paths are shown once with every location listed.

## Client-side operations

The browser is the "client PC". All of these stream, so multi-GB ISOs never sit in memory:

- **Upload** into a chosen library and path: chunked, resumable uploads (`PUT` with offsets, written to a `.part` file then renamed), progress per file, drag and drop of files and folders.
- **Download** from any library with HTTP Range support.
- **Pick from the client** for tasks that need a file: the same upload, into a temporary staging area that is cleaned up.
- **Console transfers** and USB writes always run on the server, since the server is on the same LAN as the console. The browser only watches progress.

## Milestones

| # | Milestone | Status |
|---|---|---|
| 0 | **Skeleton:** crate layout, axum server, embedded UI shell, settings, auth, error model, jobs + SSE, `--mock`, Dockerfile, compose, CI-style `cargo test/clippy/fmt` | ✅ |
| 1 | **Libraries:** model, settings UI, path health, SQLite index, scanner for `iso`/`god`, library browser, upload/download | ✅ |
| 2 | **Title IDs:** embedded CSV, XEX/XISO/GOD header reading, name matching, duplicate grouping | ✅ |
| 3 | **Conversion:** ISO→GOD (`iso2god-rs`), GOD→ISO, ISO extract/create (`extract-xiso`), verify/fix (`abgx360` binary), multi-disc, tidy/rename, output into a chosen library | ✅ |
| 3b | **IGDB:** cover art and game details from IGDB, cached, with manual match choice and settings | ✅ |
| 3c | **Drives and import:** drive agent for disks on other computers, remote library folders, import (copy/move/link/symlink, convert, also-send), compare, damaged-game detection, Xbox drive page (sync, replace, copy either way, remove) | ✅ |
| 3d | **Drive management:** job queue, Replace the game, shared compare view (copy/move/delete/mirror), duplicates, tidy on drives | ✅ |
| 4 | **Console:** Aurora FTP client (written for its quirks), connection profiles, console file browser, transfer plan (what is already there, FAT32 limits, replace), resumable transfer job, scan and compare with a library. See `docs/console.md` | ✅ |
| 5 | **Other content:** the Arisen Studio database (mods, trainers, homebrew, cheats, saves, patches), install to the console with safe path handling, keep copies in libraries, install from libraries. See `docs/content-and-updates.md` | ✅ |
| 6 | **USB:** safe device detection (removable sticks only), FAT32 format, Bad Avatar stick builder (package supplied by you), tested on a real stick. See `docs/usb-and-extras.md` | ✅ |
| 7 | **Title updates:** STFS parsing, XboxUnity search and download, compatibility check, install to the console | ✅ |
| 8 | **Extras:** USB backup/restore (partclone + zstd), scheduled scans, notifications | ✅ |
| 9 | **Wanted (Usenet):** wanted games from IGDB, Newznab indexers (with Prowlarr import), release matching and scoring, SABnzbd hand-off and tracking, automatic import, blocklist. See `docs/wanted.md` | ✅ |
| 10 | **Discover:** browse Xbox 360 games on IGDB (genre, years, sort, search), game pages with similar games, one-click Usenet download into the library. See `docs/wanted.md` | ✅ |

Each milestone ends with tests, docs updated, and a Docker build that runs in `--mock`.

## Proposed layout

```
src/
├── main.rs, lib.rs, error.rs, perms.rs
├── commands/        serve, scan, convert, ... (CLI mirrors the web actions)
├── web/             axum routes per area, auth, SSE, static assets
├── jobs/            registry, event log, resource locks
├── settings/        settings.json
├── db/              SQLite schema, migrations, queries
├── library/         library model, path health, scanners, upload/download
├── xbox/            title ids, xex/xiso/stfs/god parsing, god<->iso, console paths
├── console/         Aurora FTP client, profiles, transfer planner
├── usb/             detection, format, mount, Bad Avatar builder, backup
├── content/         Arisen DB, local mod/cheat/patch folders, installer
└── mock/            fake console, fake paths, fake USB
web/                 embedded UI (index.html, css/, js/ one module per page)
data/                gamelist_xbox360.csv
docs/
```

## To verify early (M0–M2)

- `iso2god-rs` works as a library dependency (otherwise vendor or shell out to the binary).
- Whether a maintained Rust XDVDFS crate can replace `extract-xiso`.
- Aurora FTP quirks to keep from `ftp_client.py` (path format, rename, large-file behaviour).
- Where the Bad Avatar / XeUnshackle files come from. They are **not** committed; the app downloads them from the projects' releases, or you supply them in `/config`.
- Unprivileged-LXC uid/gid mapping for bind-mounted and NFS paths (PUID/PGID handling plus docs).

## Update (v0.11.0)

- Drive folder roles now include Console content, Trainers, Mods, Homebrew and Emulators.
- **Fix misplaced items…** on the Xbox drive page (`src/library/misplaced.rs`): lists add-ons/updates in `Games` or strays in `Content`, and trainers inside profile save folders, with a previewed per-run move. Never deletes; spare copies are only reported.

## Update (v0.12.0): bug hunt, speed and interface pass

Fixed: a panicking job no longer stays "running" (and holds its folders) for ever; job history and
numbers survive a restart (database table `job_history`); a download stuck on "importing" after a
restart is released; removing a game keeps saves, add-ons and updates in its title folder; the
scan no longer lists the console's `Content`, trainers, mods, homebrew or emulators folders as
games; tidy ignores a folder called Content and any saves or other non-game content; text from
indexers, IGDB keys and library names can no longer cause a panic (UTF-8 slicing); graceful stop
on SIGTERM (Docker); case-only renames on FAT; secrets files are private from the first byte.
Faster: folder health is cached for a few seconds (a dead share no longer piles up blocked
threads), the job banner polls adaptively, the agent connection is shared.
Interface: grouped sidebar, quick find (Ctrl+K), mobile drawer, light/dark switch, accessible
dialogs, dashboard "needs attention" and storage, games sort/filter chips/cover view with the
view kept in the address, sortable, paged library items, grouped libraries, Jobs filters and Stop.
See `docs/interface.md`.

## Update (v0.13.0): torrents

qBittorrent support in two ways (`docs/torrents.md`): torrent indexers (Torznab) searched and graded by seeders alongside Usenet, and a **Torrents** page that lists `.torrent` files in folders you choose, shows every file inside one (the Redump Xbox 360 collection is one torrent of 4,459 zipped games), and downloads only the ticked files. Finished downloads are imported by hardlink/copy/move (the torrent keeps seeding by default); zipped disc images are unpacked first. Not yet run against a real qBittorrent.

## Update (v0.13.7 / v0.14.0): search inside torrent files, drive from the browser

- Wanted/Discover search windows have a **My torrent files** tab: find a game inside the files listed in your `.torrent` folders and grab just that file (`docs/wanted.md`).
- **Xbox drive from the browser** (`docs/drives-and-import.md`): pick the mounted drive's folder on the page (File System Access API, Chrome/Edge/Brave), no agent program or token typing. RustyBox serves HTTPS on :8443 with a self-signed certificate (the API needs a secure page). The page answers the server's jobs over a long-poll relay (`src/web/browser_agent.rs`) speaking the agent's own `/agent/v1` calls, so everything else is unchanged; scanning is done server-side by reading headers in ranges (`src/library/scan_remote.rs`). Verified: relay and scan equal to the real agent in tests, and the page's agent against a real Chromium file handle (OPFS). Not verified: a real picked folder on the user's disk (a browser needs a human to pick one), and folder renames (Tidy) may be refused where the browser can't move a directory.
