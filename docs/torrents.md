# Torrents

RustyBox can fetch games two ways with a torrent client (qBittorrent):

1. **From torrent indexers**, the same way as Usenet: **Wanted → Setup** takes Torznab indexers (or imports Prowlarr's), the search lists releases with their seeders, **Grab** sends the best to qBittorrent, and the finished download is imported into your library. See `docs/wanted.md`.
2. **From torrent files you already have**: the **Torrents** page lists the `.torrent` files in folders you choose, opens one to show every file inside it, and downloads **only the files you tick**. This is how you take a few games out of a huge collection (Minerva's Redump Xbox 360 torrent is one torrent holding 4,459 zipped games).

## Setting up

In **Wanted → Setup → qBittorrent** enter its address (`http://192.168.1.110:8080`), user name and password, and a category (**Create the category** makes it in qBittorrent). Switch on the Web UI in qBittorrent first (Tools → Options → Web UI). The password and the login session are never shown in the browser or in error messages. **Test connection** checks the address and login (both qBittorrent 4 and 5 are supported; if it runs without a login on your network, leave the user name and password blank).

Choose **Import into** a library folder (in *Finished downloads*) so finished torrents are imported, and set **path mapping** if qBittorrent runs in a container and reports folders differently from how RustyBox sees them (`/downloads → /data/storage/Download`).

Two torrent settings:

| Setting | Choices |
|---|---|
| *When a torrent finishes, put it in the library by* | **Hardlink** (the default: instant, and the torrent keeps seeding; it copies instead when the library is on another disk or on a drive agent), **Copy** (keeps seeding), **Move** (the torrent stops seeding: qBittorrent will report its files missing). |
| *Then* | **Leave it in qBittorrent** (keep seeding), or **Remove it from qBittorrent, with its files** (after a Move it only removes the torrent). |

### Don't have qBittorrent yet?

A Dockge/Compose stack for it, with downloads on the same disk your libraries live on (so a hardlink import is instant):

```yaml
services:
  qbittorrent:
    image: lscr.io/linuxserver/qbittorrent:latest
    container_name: qbittorrent
    restart: unless-stopped
    environment:
      PUID: "1000"
      PGID: "100"
      TZ: Europe/London
      WEBUI_PORT: "8080"
    ports:
      - "8080:8080"
      - "6881:6881"
      - "6881:6881/udp"
    volumes:
      - ./config:/config
      # The same host folder RustyBox sees as /data/storage, so both agree where files are.
      - /mnt/storage:/data/storage
```

With that, qBittorrent's *Default Save Path* can be `/data/storage/Download/Torrents` and no path mapping is needed. (The first start prints a temporary Web UI password in the container's log; set your own under Tools → Options → Web UI.)

## Folders of torrent files (the Torrents page)

1. **Add folder** (any folder RustyBox is allowed to use). Folders inside it are searched too, a few levels down.
2. Search the list (words in any order), then **Choose files…** on a torrent.
3. The dialog lists every file with its size. Filter it, tick files (**Tick all matching** ticks everything the filter shows, not only what is on screen), and press **Download**.
4. RustyBox adds the torrent to qBittorrent *stopped*, switches off every file you didn't tick, then starts it, so nothing you didn't choose is fetched. Ticking nothing is refused; ticking everything adds it normally. A torrent that is already in qBittorrent is not added a second time (change its files there).
5. It appears under *Torrent downloads* and in **Wanted → Activity**, and is imported when it finishes.

## Zipped games

Redump's collection (and many others) is one **zip per game with the disc image inside**. When a finished download holds no game folders or ISOs but does hold zips with ISOs inside, RustyBox opens them first: a job called *Unpack …* writes the ISO files into a hidden folder (`.rustybox-unpack-<n>`) next to the download, then the usual import runs on those (and converts to GOD if the library is GOD). When the import is done the hidden folder is removed. The zips themselves are never changed, so the torrent keeps seeding. Not enough free space is refused before anything is written. Only `.iso` entries are taken out of a zip, and only by file name, so a hostile zip can't write anywhere else. 7z and RAR files aren't opened (a message says so).

A download that is one file (an ISO) is imported from a folder of its own made of a hardlink to it, so other files next to it in the download folder are never swept up.

## Safety

Torrent files, magnet links and zips come from the internet. They are parsed with limits (size, nesting depth, number of files); names inside a torrent are shown and handed to qBittorrent but never used as paths by RustyBox; magnet links (which can carry a tracker passkey) and indexer download links (which carry the indexer's key) never reach the browser; a torrent file is only read from inside a folder you added.

## What is tested, and what isn't

Tested with pretend servers that behave like qBittorrent (login cookie, stopped adds, file priorities, 4 and 5 style names) and a pretend Torznab indexer: the whole chain from search or folder to the library, including unpacking, hardlinking, one-file torrents, removal afterwards, hostile torrent and zip files, and a real 4,459-file Redump torrent file. It has also been run for real against qBittorrent 5.2.4 with a small made-up torrent: stopped add, only the chosen file switched on, started, followed by RustyBox, then a completed one imported (unpack, import, removal). That run found two differences from older versions that are now handled: login answers 204 with no body, and the session cookie is called `QBT_SID_<port>` rather than `SID`. A real download from the internet hasn't been run.
