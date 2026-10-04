# Wanted: finding games on Usenet and as torrents

**Wanted** works like Radarr or Sonarr, for Xbox 360 games. You build a list of games you want (from IGDB, so each has its cover, year and description); RustyBox searches your Usenet indexers, picks the right release, hands it to SABnzbd, follows the download, and imports the finished game into a library (converting an ISO to GOD on the way if you like).

## Discover

**Discover** browses every Xbox 360 game on IGDB (and only Xbox 360 games, and only real games: no DLC or bundles). Filter by genre and release years, sort by most popular, best rated, newest, oldest or name, or search by name (a search is ordered by relevance, which is how IGDB ranks it). Each card shows the cover, year, rating and where the game stands: **in your library**, **downloading** (with progress) or **on your wanted list**.

Open a game for its details: description and storyline, developer and publisher, rating and how many people rated it, modes and perspectives, screenshots, and **similar games** (limited to ones that exist on the Xbox 360; click one to jump to it). From there:

- **Download from Usenet** adds it to the wanted list, searches your indexers and sends the best acceptable release to SABnzbd. From then on it is the normal flow below: tracked under Wanted → Activity, imported into your library when it finishes, converted to Games on Demand if that is how Setup is set. If nothing acceptable is found, it stays on the wanted list and says so.
- **Search releases…** shows every release with its verdict so you can choose.
- **Add to wanted list** just adds it.

## Set up (Wanted → Setup)

1. **Indexers.** Add Newznab indexers by hand (address and API key), or **import from Prowlarr**: enter Prowlarr's address and API key (Settings → General in Prowlarr) and every enabled usenet indexer is added through Prowlarr's own Newznab address for it, so only Prowlarr's key is stored. Press **Test** on an indexer: only those carrying the Xbox 360 category (1050) are searched; the result is remembered.
2. **SABnzbd.** Address, API key and a category (default `xbox360`). **Test connection** shows its version and categories; **Create the category** makes it in SABnzbd with its own folder.
3. **What to look for.** Preferred region, whether other regions are acceptable, formats (disc image, Games on Demand, arcade packages), disc size limits, the score needed to grab automatically, and words that reject a release.
4. **Finished downloads.** Which writable ISO/GOD library folder games go to, move or copy, and whether to convert ISOs to GOD (for a GOD library). Optionally also **copy to** another library folder (the Xbox's drive, even on another computer through the agent: done in the same job, from the converted game) and **send to a console** over FTP (a follow-up job, shown as a **Console job** link in Activity). **Path mapping**: SABnzbd reports where it saved a download *as it sees it*; map that to where RustyBox sees the same place (for example `/data → /data/main20tb`).
5. **Automation.** Search every N hours, grab the best release automatically, import finished downloads automatically. All off by default except import.

API keys are stored on the server (`grabber.json`, owner-only) and are never sent to the browser. NZB links contain the indexer's key, so they never leave the server either, and keys are scrubbed from error messages.

## Torrents too

Torrent indexers (Torznab, or Prowlarr's torrent indexers) sit alongside the Usenet ones: **Setup → Indexers** has a Usenet/Torrent choice, and the Prowlarr import brings in both kinds. Torrent results show their seeders; **a release nobody is sharing is rejected**, and a well-seeded one scores higher. Torrent indexers are searched only once qBittorrent is set up (the search says when some were left out). A grab fetches the `.torrent` itself (following a redirect to a magnet link if that is what the indexer does) and hands it to qBittorrent. Everything after that is the same as for Usenet: follow it in **Activity**, import it when it finishes. How torrents are set up, imported and cleaned up is in `docs/torrents.md`.

## Day to day

- **Add a game** searches IGDB (Xbox 360 games only). Pick one and it joins the list with its cover.
- **Search…** asks every indexer and shows each result with a verdict. Releases that are the wrong game (a sequel, a spin-off, another platform), DLC, updates, demos, fragments of multi-part posts, discs that are too small or too big, regions you refuse, and anything on the blocklist are **rejected with the reason**. The rest are scored (region and format preference, proper releases, popularity, age) and listed best first. **Grab** sends one to SABnzbd; grabbing a rejected one asks first.
- **Grab best** does the search and grabs the top acceptable release.
- **Search all** and the schedule do this for every monitored game that isn't already downloading or in a library.
- **Activity** follows each download (queued, downloading with progress, downloaded, importing, in your library, or failed). A failed download goes on the **Blocklist** and the game goes back to Wanted, so the next search skips that release. **Import** retries an import by hand.
- The whole chain can be one press: download, convert to GOD, library, copy to the Xbox drive, send to the console. If the drive or console is offline when it's time, the import (or the console job) fails with the reason, and **Import** on the Activity tab retries.
- A game counts as **in your library** when a library holds it (matched through IGDB or the title ID).

## How releases are matched

Indexers search loosely ("Halo 3" also returns Halo Reach and ODST), so a release counts only when its name, with the platform, region, language and format words taken out, is the game's name, give or take edition words such as "GOTY". Roman and Arabic numerals are the same ("Fable II" is "Fable 2"), "The" is ignored, and searches also try the name without edition words ("Halo 3: Legendary Edition" is searched as "Halo 3"). Real release names such as `Gears.of.War.3.XGD3.PAL.SPANiSH.XBOX360-FBi` and `Gears of War 3 READNFO XGD3 0800 USA RF-XBOX360-RRoD` are in the tests.

## Limits to know about

- An Xbox Live Arcade release is imported only if its download is a Content-style folder (`<TitleID>/000D0000/...`), which is what real ones are; other layouts are left for a manual import.
- A game with several discs is one grab per disc release; only single releases are handled automatically.
- Searches go through your indexers' API limits: an indexer that says "too many requests" is reported and skipped for that search.
