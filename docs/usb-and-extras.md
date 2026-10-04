# USB sticks and the extras

## Safety first

RustyBox only ever offers **removable USB sticks**. A hard disk (such as the Xbox's own drive), a system disk, anything bigger than 1 TB, and anything holding part of the running system or RustyBox's own data is listed as *off limits* with the reason. Every action re-checks the device against the live list, and anything that erases a stick needs you to type its device path. Hardware tests run only with `--features hardware_tests` against a stick you name.

## Bad Avatar stick

**USB → Build a Bad Avatar stick…** (optionally formats the stick first, as one FAT32 volume with no partition table, named `BADUPDATE`), copies the exploit package, renames `Apps/Aurora 0.7b.2` to `Apps/Aurora` if present, optionally sets `Default =` in `launch.ini` to start Aurora (keeping the file's Windows line endings, and creating a minimal `launch.ini` if the package has none), writes `info.txt`, and flushes. The plan shows every step first and refuses a non-FAT32 stick or a file over 4 GB.

**RustyBox never ships the exploit files.** You supply them from your browser on the USB page:

1. **Add the package folder…**: the unzipped **ABadAvatar** folder (it holds `BadUpdatePayload/` and `Content/`). Files are uploaded to the server's `/config/badavatar/`.
2. **Fetch XeUnshackle (v1.03)**: the packages don't include the program the exploit runs (`BadUpdatePayload/default.xex`). This downloads the pinned XeUnshackle release from GitHub and merges it in (its `default.xex`, `launch.ini`, `Xbdm.xex`, `JRPC2.xex`...). Or **Add my own payload** (for example FreeMyXe).
3. **Add an Aurora folder…** (optional): goes to `Apps/Aurora`. Needed for "Make Aurora start automatically".

A stick can't be built without a payload; the page says what is missing. The exploit is **not persistent**: re-run it each time the console boots.

Formatting and mounting need root and the host's USB device: run the container privileged or pass the stick through (see `compose.yaml`). On a desktop where the stick is already mounted as FAT32, building onto it needs no root.

## Backups

**USB → Back up…** saves the stick's filesystem with `partclone.vfat` (only the used blocks) compressed with `zstd` into `/config/backups/`. **Restore…** puts a backup back on a stick (erasing it; the stick must be at least as big). Nothing is ever overwritten by a new backup.

## Loading files from a stick

Mount the stick on the host and bind-mount that folder into the container (`compose.yaml`), then use **Import** with the folder, or copy games straight on to the console from the library.

## Automation (Settings)

- **Rescan all libraries every N minutes**.
- **Notifications**: a message when jobs finish (only failures, or everything). Give any web address that takes JSON (ntfy, Discord, Slack). The body has `title`, `message`, `status` and `job`, plus `text` and `content` for Slack and Discord, and ntfy gets `Title` and `Tags` headers. The address is a secret and is never sent back to the browser.
