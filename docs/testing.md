# What has been tested for real, and what only in simulation

Last updated for v0.8.1. "Real" means real hardware or the real internet. "Simulated" means a stand-in
(a pretend Aurora server, an image file, a local web server). Simulated tests prove RustyBox's logic and
protocol handling; they can't prove a real device behaves the same.

## Real

| What | How |
|---|---|
| Building a Bad Avatar stick | Your real **ABadAvatar v1.3-beta** package + the real **XeUnshackle v1.03** payload (fetched from GitHub), built onto a real FAT32 USB stick (`/dev/sdd`, SanDisk Cruzer Blade) without formatting. Every file checked; the package files are byte-identical to your download. The stick is left built. |
| USB device detection | Real `lsblk` on this PC and on the server: removable sticks eligible, hard disks and system disks refused with a reason. |
| Real exploit package layout | The v1.3 package has `BadUpdatePayload/` + `Content/` and **no payload, no Aurora, no launch.ini**. RustyBox now handles that (it says the payload is missing, and can fetch XeUnshackle). |
| Arisen Studio database | Real download of all files (583 trainers, 70 mods, 55 homebrew, ...), parsed; every mod/trainer/homebrew entry resolved to a safe console path (test: `real_network`, `--ignored`). |
| A real trainer | Downloaded, unzipped, and its real `.xex` (XEX2 header) sent to a pretend console at the right path. |
| XboxUnity | Real search and update listing; a real title update (Halo 3, TU3) downloaded, its header read: media ID `1CDE207A`, base version `0000002C` and TU number 3 all match XboxUnity's own listing. (Found and fixed: real TUs keep their number in their name, not a header field.) |
| FAT32 formatting, backup, restore | `mkfs.vfat` and `partclone.vfat` + `zstd` on image files; the restored image has its files back (checked with mtools). |
| The Docker image | Built, pushed and running on the server with all the tools (lsblk, mkfs.vfat, partclone, zstd). |
| Earlier milestones | Real ISOs converted ISO to GOD and back byte-identically; the real Xbox drive scanned read-only (175 GB) before it was unplugged. |

## Wanted (Usenet) tests

| What | How |
|---|---|
| Real Prowlarr import | The real Prowlarr on the services server: its five usenet indexers imported through its Newznab addresses; three carry the Xbox 360 category. |
| Real searches and ranking | Real IGDB games (Gears of War 2, Fable II, Halo 3, Forza 4, The Simpsons Game) searched on the real indexers; the right region ranks first; sequels and spin-offs rejected. Two real gaps found and fixed: "Fable II" is posted as "Fable 2", and IGDB names like "Halo 3: Legendary Edition" are never in release names. |
| Real SABnzbd | Version, categories and **creating the `xbox360` category** on the real SABnzbd 5.0.4; one real download (a 2.2 GB arcade game) grabbed through Prowlarr's NZB link, followed through the queue to completion, with the finished folder reported as predicted. Cleaned up afterwards. |
| Real importer input | The real completed download's structure (`58411256/000D0000/<package>`) is recognised by the importer (title ID and name read from the real header). |
| Real IGDB for Discover | The browse, genre list, sort orders, decade filter, search, paging, game detail (screenshots, modes, storyline) and similar-games queries all run against the real IGDB with real data. The grab-from-Discover step uses scripted indexer/SABnzbd stand-ins (`tests/grabber.rs`). |
| Simulated | Failed downloads and blocklisting, Prowlarr/indexer error responses, and the full import into a library, with scripted stand-ins for the indexer and SABnzbd (`tests/grabber.rs`). The import of a real finished download into a real library has not been run yet. |

## Simulated only (to do on real hardware)

| What | Simulated with | What could differ for real |
|---|---|---|
| **Everything over FTP to the console** (scan, send, replace, fetch, delete, browse, installs, update installs) | A pretend Aurora server with Aurora's quirks (LIST ignores its argument, no MLSD/SIZE, STOR won't make folders), also with DOS-style listings and a wrong PASV address. | The exact LIST format, how Aurora answers an early-closed transfer, rename of files on its filesystem, timeouts. **Run the Self-test on the Console page first**: it tries every operation in a throwaway folder and says which step fails. |
| Formatting and mounting a real stick | `mkfs.vfat` on an image file. | Needs root: not possible on this PC (no sudo). Needs a privileged container or device passthrough on the server. |
| Restoring a backup onto a real stick | An image file. | Same root requirement. |
| Whether the built Bad Avatar stick actually triggers the exploit | Nothing can simulate this. | Needs the console, dashboard 17559 and a profile. Not something RustyBox can verify. |
| `launch.ini` handling for Aurora | Text edits checked on the real XeUnshackle `launch.ini` layout and synthetic files. | Whether DashLaunch honours the `Default =` line the way RustyBox sets it. |
| qBittorrent (login, add stopped, file priorities, start, progress, delete) and torrent indexers | Pretend servers with qBittorrent's behaviour (`tests/torrents.rs`). | Run once for real against qBittorrent 5.2.4 (login, stopped add, priorities, start, finish, import, removal). Older 4.x versions and downloads from real peers are untested. |
| Notifications | A local receiver. | Delivery to ntfy/Discord/Slack. |
| Scheduled rescans | Not run end to end (the shortest interval is 5 minutes). | Should be exercised once by setting 5 minutes. |

## Tomorrow's real tests, in order

1. **Console page → Add** your console (Aurora's FTP on) → **Self-test**. All steps should pass. Anything that fails shows exactly what the console said.
2. **Scan** the console, then **Send** one small game, then **Replace** it, then **Copy it back** to a library and compare.
3. **Updates**: install a title update for a game you have; check it in Aurora.
4. **Content**: install one trainer; check it in Aurora's trainers menu.
5. **USB**: the stick built today is ready. Try it on the console (Avatar exploit; see the package notes). Remember it is not persistent.

## Automatic safety nets in `cargo test`

- **Garbage in** (`tests/fuzz.rs`): random and mangled bytes and text go through the Xbox file
  parsers, the FTP listing parser, the indexer date parser and the release-name parser, which
  must never panic.
- **The interface** (`tests/web.rs`): every UI script must parse (checked with Node when it is
  installed), every import must point at a real file, and no event handler may return `false` by
  accident (it cancels typing).
- **Moving and removing**: Tidy and Fix misplaced never touch the console's `Content` folder
  layout, saves, or trainer/mod/homebrew/emulator folders; removing a game keeps saves, add-ons
  and updates (see the tests in `src/library/tidy.rs`, `src/library/misplaced.rs`, `tests/import.rs`).
- **Jobs**: a job that panics ends as failed and frees its folders; job history and numbers survive
  a restart (`src/jobs.rs`).
