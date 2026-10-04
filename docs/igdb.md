# Game information (IGDB)

RustyBox can show cover art, release year, genres, developer, publisher, rating and a description for each game, from [IGDB](https://www.igdb.com). IGDB is owned by Twitch and is free to use with a Twitch application.

## Setting it up

1. Enable two-factor authentication on your Twitch account (IGDB requires it).
2. Create an application at <https://dev.twitch.tv/console/apps/create>:
   - **OAuth Redirect URL:** `https://localhost`. The form insists on an HTTPS address, but RustyBox never uses it: IGDB's *client credentials* flow has no user login and no redirect.
   - **Client Type:** **Confidential** (the type that gets a Client Secret; RustyBox is a server, so it can keep the secret private).
   - **Category:** anything sensible, such as *Application Integration*.
3. Open the application and click **New Secret**. Twitch shows the secret once.
4. Enter the **Client ID** and **Client Secret** in **Settings → Game information**, and press **Test connection**. You can test before saving.

For development, copy `.env.example` to `.env` (git-ignored) and fill it in. The development server reads it from its working directory. You can also set `IGDB_CLIENT_ID` and `IGDB_CLIENT_SECRET` (or `RUSTYBOX_IGDB_CLIENT_ID` / `RUSTYBOX_IGDB_CLIENT_SECRET`) on the container. The environment, or `.env`, takes priority over Settings, and Settings then shows the credentials as set by the environment.

The Client Secret is **never sent back to the browser**: Settings shows only whether it is set and its last four characters. It is stored in `settings.json` (mode 600).

## How it works

- RustyBox swaps the credentials for an access token (about two months) at `id.twitch.tv`, keeps it in memory and renews it when it runs short. A refused token is renewed once and the request retried.
- Requests are spaced out to stay under IGDB's limit of four a second.
- **Matching:** each game's name from the title list (with regional tags and edition words such as "Limited Collector's Edition" or "Bonus Disc" removed if needed) is searched on IGDB among Xbox 360 games of type *main game, remake, remaster, expanded game* or *port*, so DLC and map packs are not matched. The best result by name wins. Names that don't match closely are left unmatched, never guessed.
- **Cache:** the details are stored in the database and covers in `covers/` next to the settings, so each game is fetched once. Games with no match are tried again after 30 days (or when you press **Fetch game info**).
- **Automatic:** by default, new games are looked up after a scan or a conversion (switch off in Settings). **Fetch game info** on the Games page runs it by hand, and retries games that found nothing.
- **Your say:** on the Games page, open a game and use **Choose a match…** to pick the right IGDB entry yourself. A match you choose is never replaced by automatic lookups. **Look up again** and **Remove info** are there too.
- Only the game's name is sent to IGDB, never file names or paths.

In `--mock` mode, sample details and covers are generated and nothing leaves the machine.

IGDB's terms ask for attribution, so the Games page says where the data comes from.
