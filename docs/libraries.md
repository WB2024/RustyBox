# Libraries

A **library** gathers one or more folders, on any disk or share, into one place in RustyBox.

## Storage model

RustyBox only ever sees paths **inside its container**. Host disks are bind-mounted, and network shares are mounted on the host first and then bind-mounted into the container. There is no network-filesystem code in the app: a path is a path, wherever it really lives.

```yaml
volumes:
  - /mnt/storage:/data/storage
  - /mnt/nas:/data/nas   # a network share, mounted on the host first
```

Library folders must sit under an allowed root: `RUSTYBOX_ROOTS` (default `/data:/mnt:/media:/srv`, separated by `:`). The folder picker only shows those, and the API refuses anything outside them.

## Kinds

| Kind | Items found by the scanner |
|---|---|
| ISO games | every `.iso` file |
| GOD games | every title folder (`<TitleID>/<content type>/…`); named from the folder above it (`Game Name/TitleID/…`) |
| Mods, Cheats, Trainers, Homebrew, Saves, Title updates, Emulators, Custom | every file |

Game libraries are also read for title IDs, names and disc numbers: see [Xbox formats](xbox-formats.md).

## Safety rules

- **Removing a library or a folder from it only forgets the index.** Files on disk are never touched.
- A folder that is **offline** (not mounted, unreadable, not responding) keeps its items, marked *unavailable*.
- A folder that suddenly looks **empty** but had items before is treated as offline too, because an NFS share that failed to mount leaves an empty folder behind. Nothing is removed.
- Hidden files and `*.part` files are ignored.
- Folders are only written to if you tick **Writable**.

## Scanning

**Scan now** runs a job per library. Unchanged items are left as they are; items whose files are gone are removed. A scan also runs after an upload. Network shares have no reliable change notification, so scans are explicit.

## Upload and download

- **Upload** from the library page: choose a writable folder, then drop files or pick them. Files are sent in 16 MB pieces to `<name>.part` and renamed when complete, so a half-sent file is never visible under its real name. A dropped connection resumes from where the server got to. Existing files are only replaced after you confirm. Free space is checked first.
- **Download** supports HTTP Range (resumable). Symlinks that lead outside the library folder are refused. Folders (GOD titles) can't be downloaded yet.

## API

| | |
|---|---|
| `GET/POST /api/libraries` | list (with folder health and totals) / create |
| `GET/PUT/DELETE /api/libraries/{id}` | one library / rename / remove (index only) |
| `POST /api/libraries/{id}/paths`, `PUT/DELETE …/paths/{pid}` | add / change / remove a folder |
| `POST /api/libraries/{id}/scan` | start a scan job |
| `GET /api/libraries/{id}/items?q=&path_id=&limit=&offset=` | search items |
| `GET/PUT /api/libraries/{id}/upload?path_id=&rel=&offset=&total=` | upload status / send a piece |
| `GET /api/libraries/{id}/items/{item}/download` | download (Range supported) |
| `GET /api/games?q=&only=` | every game across all libraries, grouped by title ID |
| `GET /api/fs/browse?path=` | folder picker (folders only, inside the roots) |
