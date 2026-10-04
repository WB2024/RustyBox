# The interface

RustyBox's UI is plain JavaScript modules served from the binary (no build step). This page is
about how it is laid out and how to get around quickly.

## Navigation

The sidebar is grouped by what you are doing:

| Group | Pages |
|---|---|
| (top) | Dashboard |
| Library | Games, Libraries, Discover, Wanted, Torrents |
| Xbox 360 | Console, Xbox drive, Updates, Content, USB |
| Move and fix | Import, Compare, Tools |
| System | Jobs, Settings |

- **Quick find** (`Ctrl+K`, `⌘K` or `/`): jump to a page, a library or a game from anywhere. Arrow keys
  and Enter work; Esc closes it.
- On a phone or a narrow window the sidebar becomes a drawer behind the ☰ button, with the same
  quick find beside it.
- **Theme**: follows your system by default; the 🌓 button in the sidebar switches and remembers it.
- Every dialog closes with Esc, keeps Tab inside itself and gives focus back when it closes.
- A page that is slow to load never paints over the one you have moved on to; a page that fails
  shows why with a **Try again** button.

## Running jobs

A banner at the top shows the running job (and how many more). It checks for jobs every 1.5 seconds
while something runs, every 6 seconds when nothing does, and not at all in a hidden tab. Job history
(with each job's log) is kept in the database, so it survives a restart: a job that was running
when RustyBox stopped comes back as *failed: cut off by a restart*, and job numbers are never reused.

## Dashboard

- Counts of games, libraries, consoles and running jobs.
- **Needs attention**: offline folders, jobs that failed in the last day, games that are in a
  library twice, games without cover art, features not set up yet.
- **Storage**: one bar per disk (folders on the same disk are shown once).

## Games

Search, **sort** (name, largest, newest), **filter chips with counts** (duplicates, ISO and GOD, ISO
only, GOD only, unknown title), and a **List** or **Covers** view. The search, filter, sort and view
are in the address (`#games?only=duplicates&view=grid`), so a view can be bookmarked or linked from
the dashboard. A game is a *duplicate* only when the same disc is in one library more than once; a
copy in a library plus another on the Xbox's drive is not.

## Libraries

Libraries are grouped by purpose (Games; Add-ons, updates and saves; Tools and extras; Other). A
library with several folders marks its **primary** one (where *Fix misplaced items* puts things;
change it with **Make primary**). Its items can be sorted by any column and are loaded 200 at a time.
