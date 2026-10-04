# Xbox formats and title IDs

RustyBox reads Xbox 360 files itself, in Rust, with no external tools. Scans use this to give every game a **title ID**, **media ID**, **disc number** and a **name**.

## What is read

| Item | Where the information comes from |
|---|---|
| **ISO** | The disc image's XDVDFS file system is found (the game partition can start at 0, `0x02080000`, `0x0FD90000` or `0x18300000`, depending on the disc generation). `default.xex` is read from the root folder and its XEX2 *execution info* gives title ID, media ID, disc number and disc count. |
| **GOD folder** | The container file in `<TitleID>/<content type>/` is an STFS package (`LIVE`/`CON `/`PIRS`). Its header holds the title ID, media ID, disc numbers and the game's name. `00007000` is preferred when several content types exist. |
| **Name** | From the bundled title list (`data/gamelist_xbox360.csv`, about 3,000 titles). A matching media ID picks the right regional variant; otherwise the first listed name. If the title isn't in the list, a GOD package's own name is used. |

Headers are read only for **new or changed** files (and unreadable ones are retried), so rescans of big libraries stay quick, even over NFS.

An item that can't be read is kept in the library with a plain explanation (for example "Not an Xbox disc image", "The disc has no default.xex", or "Original Xbox disc image").

## Games page

**Games** shows every ISO and GOD game across all libraries, one row per title ID, with every place a copy lives. Filters: duplicates, both ISO and GOD, ISO only, GOD only, and titles not in the list.

A **duplicate** means more than one copy of the same kind *and disc* (for example the same ISO on two disks). Two discs of one game, or an ISO plus a GOD copy, are not duplicates.

## How well this has been checked

- **STFS headers:** run against 40 real packages from your Xbox 360 backup (33 title updates, saves, dashboard files). Title IDs matched the folders they sit in, and names read correctly (for example "Skyrim", "Far Cry® 2").
- **XEX2 headers:** real files parse (for example the BadUpdate payload reports the dashboard title ID `FFFE07D1`), and homebrew with no title information gets a clear message. A real **game** `default.xex` has not been available to test.
- **XDVDFS (ISO) reading:** tested on synthetic images only, at all four partition offsets. No real game ISO has been tried yet, so treat ISO name matching as unproven until you run a scan on real ISOs.

Real files can be checked with the ignored tests: `RUSTYBOX_TEST_STFS=<folder> cargo test real_stfs -- --ignored --nocapture` and `RUSTYBOX_TEST_XEX=<folder> cargo test real_xex -- --ignored --nocapture`.
