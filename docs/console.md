# The console (Aurora FTP)

RustyBox talks to the Xbox 360 over **Aurora's FTP server**: connect the console to your network, start Aurora, and switch on **FTP** in Aurora's settings (Settings → Network). The default login is `xbox` / `xbox`.

## Set up

**Console** in the sidebar → enter the console's IP address (shown in Aurora's network settings), the login, and the games folders Aurora scans (for example `Hdd1/Games`, `Usb0/Games`). **Test connection** lists the drives the console shows (`Hdd1`, `Usb0`, `Usb1`, ...). Passwords are kept on the server (`consoles.json`, owner-only) and are never sent back to the browser.

## What you can do

- **Scan** the console: RustyBox looks for title folders (a folder named by eight hex digits) under the games folders. The result is kept, so the page opens instantly.
- **Compare** with a library: *missing from the console*, *on both*, *only on the console*. Tick games and **Send**, **Replace the console's copy**, **Copy to the library** (fetches them back, byte for byte), or **Delete from the console** (always previewed, with sizes, and needs a confirmation).
- **Send** a game from any library (GOD or ISO; a library folder on another computer works too). The plan shows where each game ends up (`Game Name/TitleID` or `TitleID`, the same layouts as the libraries), whether it is already there, and anything wrong: a file over 4 GB going to a **USB stick** (FAT32) is refused before anything is sent, and an ISO is steered towards conversion to GOD.
- **Replace the game** works as in libraries: the old copy is found by title ID and disc, wherever it is and whatever it's called, and only the game's own files are removed. Saved games, add-ons and title updates are kept.
- **Send from a folder…**: games in a folder on the server (a mounted USB stick, a download folder) go straight to the console without importing them into a library first.
- **Self-test**: tries every operation RustyBox relies on (connect, list, make folders, send, rename, read back, resume from the middle, delete) in a temporary `RustyBoxTest` folder that it removes, and reports each step with what the console said. Run it first on a real console.
- **Browse files**: look around, make folders, rename and delete (never a whole drive).

Sending is resumable in the sense that matters: a file already on the console with the right size is skipped, a file is written under a `.part` name and renamed when whole, and every file is checked to have arrived at the right size. Several jobs for one console wait their turn.

## Aurora's quirks, handled

The FTP client is written for them: `LIST` ignores its path argument (RustyBox changes folder first, then lists), there is no `MLSD`/`SIZE` (sizes come from listings), folders are made one level at a time, and an interrupted file is simply sent again from the start. RustyBox **can't see the console's free space** over FTP, so make sure there is room.

## Testing without a console

`rustybox serve --mock` starts a pretend Aurora (a folder served over a real FTP connection with the same quirks) and adds it as **Mock Xbox 360**. The automated tests use the same pretend server.
