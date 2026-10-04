# Converting games

All conversions are done by RustyBox itself, in Rust, except checking ISOs, which uses the separate `abgx360` program. Every action shows a **plan** first: what will be written where, how much space it needs, and anything that already exists. A plan with problems can't be started.

| Action | From | To |
|---|---|---|
| **Convert to GOD** | ISO items | a Games on Demand folder in a GOD library (`Game Name/TitleID/00007000/<container>`) |
| **Convert to ISO** | GOD items | an ISO file in an ISO library |
| **Unpack to folder** | ISO items | the game's files in a folder (`Game Name (Disc N)/`) |
| **Create ISO from a folder** (Tools) | a game folder | an ISO file in an ISO library |
| **Tidy folders** | a GOD library | renames folders to the chosen layout |
| **Check ISO** | one ISO item | a report from `abgx360`, optionally fixing the image |

Select games on a library page (tick the boxes) and use the buttons that appear. The destination must be a **writable** folder of a library; GOD output goes to a GOD library, ISO output to an ISO library.

## Safety

- **Staging.** Work happens in a hidden `.rustybox-staging` folder inside the destination folder and is moved into place only when finished, so a failed or cancelled run leaves nothing half-written among your games. The staging folder is removed afterwards (leftovers from a crash are removed after a day).
- **Nothing is overwritten by accident.** Existing output is a problem in the plan unless you tick *Replace existing*. When replacing, the old item is set aside first and put back if anything fails. A folder from *Unpack* is never replaced.
- **Space.** The plan adds up what will be written and refuses if the destination can't hold it (with a margin).
- **Plans are re-checked at start**, so what runs is what was checked, not what was shown earlier.
- **One writer per destination.** Two conversions into the same folder run one after the other.
- After a job the destination library is rescanned, so new games appear straight away, named and identified.

## Details

- **Layout.** *Game Name / TitleID* (default) or *TitleID only*; set the default in Settings, or per conversion. Multi-disc games go in the same title folder, one container per disc.
- **Threads.** ISO to GOD can use several threads (Settings, default 2). More helps on an SSD; use 1 on a spinning disk.
- **Names** come from the title list; a title that isn't in it is named from the file.
- **ISO to GOD** uses the `iso2god` library (MIT). Unused space at the end of an image is trimmed.
- **GOD to ISO** rebuilds the disc image from the package. The image is written as the package holds it when it already contains a whole disc; only a package whose data starts at the volume descriptor gets the standard 64 KB image start put in front. (The Python/Rust tool this came from always added that header; here it is decided from what is actually in the data.)
- **Unpack / Create ISO** use the `xdvdfs` library. Names inside an image can't escape the destination folder.
- **Tidy** renames folders in place. It never merges into an existing folder, leaves read-only folders alone, and removes only the old folders it empties.

## Checking ISOs with abgx360

`abgx360` is GPL software, so RustyBox runs it only as a separate program (built into the Docker image) and never links it. It is run offline, and:

- **Check only** passes `--nowrite`: it cannot change anything.
- **Check and fix** lets it repair the image in place (headers, stealth sectors, video padding). It needs a writable folder, and by default a backup copy (`<name>.iso.rustybox-backup`) is made first. An existing backup is never overwritten.
- It is always told not to rebuild or delete the image and not to create side files.
- abgx360 exits with success even when it reports an `ERROR`, so RustyBox treats its error lines as a failure too. The job log shows its full output.

Checking offline means abgx360 can't compare against its online database of verified images, which limits what it can confirm.

## How well this has been checked

- ISO to GOD to ISO, unpack and pack, replacing, tidy, staging cleanup, cancel and the refusal cases are covered by automated tests through the real HTTP API, using synthetic games.
- The release Docker image was run and converted a game to GOD; `abgx360` runs inside it and left a test file untouched.
- **No real game ISO or GOD package has been converted yet.** The format code follows the libraries' behaviour and was checked on synthetic images, so run a first conversion on a game you can spare and check it on the console.
