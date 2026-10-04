# Content and title updates

## Content (mods, homebrew, trainers, saves, cheats, patches)

**Content** in the sidebar uses the public **Arisen Studio** database (`db.arisen.studio`). Press **Download the database** once; it is cached on the server and parsed from there (the files sometimes start with a byte-order mark, which is handled). Search by name, game or title ID, and tick **Only games I have** to see only items for games in your libraries.

- **Install…** on the console: the plan shows the files and where each goes (placeholders such as `{AURORAPATH}`, `{CATEGORYID}` and `{NAME}` are filled in; `Hdd:\\` becomes `/Hdd1/`). Then a job downloads the file, unpacks a zip, and sends each file. When an install path names one file but the zip has several (a readme beside the trainer), the file with that name is sent.
- **Keep a copy…** downloads the files into a library of the same kind as `TitleID/Name/file`, so you can install from your own library later (**Install on console…** on any non-game library item; the destination is suggested from the kind of library and can be changed).
- Cheats are data (nothing to install) and patches are for the Xenia emulator, so those two can be browsed and kept but not installed on the console.

Safety: install paths come from the internet, so they are never trusted. They must stay inside a console drive (`Hdd1`, `Usb0`, `Usb1`), `..` is refused, zip entries that try to escape their folder are skipped, and files are only ever downloaded from the database's own site, whatever an entry says.

## Title updates

**Updates** in the sidebar works with a library of kind **Title updates**.

- **In your library**: every file with an STFS header of content type `000B0000` is listed with its game, TU number and media ID, and whether it **fits** (its media ID is one of the game's known discs, or a disc you own), is for **another release?**, or is **unknown**.
- **Find updates on XboxUnity**: search by name or title ID, see every update per media ID, and download the ones you tick (a download must really be a title update package, not an error page).
- **Install on the console**: updates go to `Content/0000000000000000/<TitleID>/000B0000/` on the drive of your first games folder.
- **On the console** shows which games already have updates installed.
