# Discover

Browse every Xbox 360 game on [IGDB](https://www.igdb.com) (set up in `docs/igdb.md`), open one for everything IGDB knows, and add it to your wanted list or download it straight away.

## Finding games

- **Search** by name (results are ordered by how well they match), and **sort** by popularity, player rating, critics' score, newest, oldest or name.
- **Shelves** are one-click starting points: most popular, critics' picks, hidden gems, couch co-op, online co-op, horror, open world, racing, fighting, family friendly, newest, classics 2005–2007.
- **Filters** (the button next to the search box) combine freely:
  - *Genre, theme, game mode, perspective, age rating* (ESRB and PEGI): tick any number, each shows how many Xbox 360 games it has. With two or more ticked in one list, **must have all** switches from "any of" to "all of".
  - *Series, franchise, developer or publisher, game engine, keyword*: type to search IGDB, pick a name.
  - *Released* (from and to year), *rating* (60, 70, 80, 90 and up), *only games lots of people rated*, *hidden gems* (a good rating with few votes).
  - *Multiplayer*: online co-op, local co-op or split screen, online multiplayer.
  - *Hide* the games you already have, or have on your wanted list.
- The chosen filters show as chips above the results with how many games match; click one to remove it. The view (covers or list) and the filters are remembered for the session.
- **Surprise me** opens a random game that fits the filters.

## A game's page

Players' and critics' scores, how long it takes to finish, age rating (with what it is rated for), the Xbox 360 release dates by region, multiplayer details, description and storyline, trailers (linked to YouTube), screenshots and artwork, DLC, expansions, remasters and other related content, similar games, more from the same series, franchise and developer, languages, keywords, links and other names. Genres, themes, modes, developers, series and so on are clickable: they filter the list to that. **Find and download**, **Search releases…** (including searching your own torrent files, `docs/wanted.md`) and **Add to wanted list** work from here.

## Notes

- Only the game's id and the filters you pick are sent to IGDB, in the same few requests the page needs. Counts are asked ten at a time and kept for twelve hours.
- Trailer pictures are loaded from YouTube's image server when a game's page is open, and links to videos and websites open in a new tab.
- In `--mock` mode a built-in catalogue answers every filter, so everything can be tried without IGDB.
