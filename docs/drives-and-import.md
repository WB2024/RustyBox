# Drives on other computers, importing and comparing

## A drive attached to another computer

RustyBox only sees folders inside its own container. A drive plugged into your PC (for example the Xbox's hard drive) isn't one of them, so a small program, the **drive agent**, runs on that PC and shares one folder with RustyBox over your network.

### Running the agent

On the computer the drive is attached to (it is the same `rustybox` program, so build or copy it there):

```bash
rustybox agent --root /media/you/XBOXDRIVE
```

It prints the address and a **token** (a secret, made once and kept in `~/.config/rustybox-agent/token`):

```
RustyBox drive agent "my-pc" sharing /media/you/XBOXDRIVE
  vfat filesystem, 1796.2 GB free of 2000.2 GB

In RustyBox, add a Remote drive with:
  Address: http://192.168.1.48:8099
  Token:   0123…
```

- **Firewall:** the computer must allow RustyBox in. With `ufw`: `sudo ufw allow from 192.168.1.0/24 to any port 8099 proto tcp` (use your network and port).
- **Read-only:** add `--read-only` to let RustyBox look but never change anything. Use `--bind`, `--name` and `--token` (or `RUSTYBOX_AGENT_*` variables) to change the defaults.
- **Keep it running:** a user service does it, for example `~/.config/systemd/user/rustybox-agent.service`:

  ```ini
  [Unit]
  Description=RustyBox drive agent
  [Service]
  ExecStart=/path/to/rustybox agent --root /media/you/XBOXDRIVE
  Restart=on-failure
  [Install]
  WantedBy=default.target
  ```

  then `systemctl --user enable --now rustybox-agent`. When the drive isn't plugged in the agent just reports it is missing, and RustyBox shows that library folder as offline and keeps its list.

### Adding it in RustyBox

**Libraries → New library (or Add folder) → A drive on another computer.** Enter the address and token, press **Test connection**, and optionally a **folder on the drive** to start from, such as `Games`, so profile and save folders aren't listed. The folder behaves like any other: it is scanned (on the drive's side, where the files are), shows up in Games and Compare, and takes uploads and downloads.

### Security

- Every request needs the token; wrong guesses are slowed down. The token is stored by RustyBox, and is never sent to the browser.
- The agent only ever touches files inside its folder: `..`, absolute paths and symlinks that lead outside are refused.
- It speaks plain HTTP, so keep it on your own network (the firewall rule above) and don't expose the port to the internet.

### What works with a remote folder

Scanning, browsing, downloads and uploads from the browser, **Import**, **Compare**, **Send to…** and copying games onto it. **Not yet:** converting from or into it directly (convert into a local library, then send the result to the drive), tidying, and checking ISOs with abgx360.

### FAT32 and other filesystems

RustyBox asks the drive what it is. A **FAT32** drive can't hold a single file of 4 GiB or more, so an ISO that big is refused up front (convert it to GOD, whose files are small), and it can't hold hard links or symlinks. The plan says so before anything is written.

### Damaged games

When a scan reads Games on Demand folders, it checks they are complete: an empty or tiny container file, missing or empty data files, or fewer data files than the container says there should be. Interrupted copies are flagged ("17 of 42 data files are missing", "copied incompletely") in the library, in Games, and in Compare, instead of failing when you try to play them. Single-file packages (Xbox Live Arcade games, add-ons) are recognised and not flagged for having no `.data` folder. Add-on (DLC) and update content is kept out of the games lists.

## Import

**Import** brings games in from a folder: on this server, or inside any library folder or remote drive.

1. **Choose the source** and press **Look inside**. ISOs and Games on Demand folders are listed with their names, title IDs, sizes and any damage.
2. **Tick what to import** and choose where it goes (a writable ISO or GOD library folder, which can be a remote drive).
3. **Choose how**:

   | | |
   |---|---|
   | **Copy** | Keeps the original. Works between any two places. A copy that was interrupted carries on where it stopped, and files already there with the right size are skipped. |
   | **Move** | On one filesystem it is a rename, instant. Otherwise everything is copied, every file is checked to have arrived at the right size, and only then is the original removed. |
   | **Hard link** | The same data in a second place, using no extra space. Same filesystem only; not for FAT32 or exFAT. |
   | **Symlink** | A pointer to the original, made per file so the library still sees a normal folder. It points at the original's place inside RustyBox, so it only works there (not for Samba, Aurora or the host) and breaks if the original moves. |

   Options: **Convert ISOs to GOD** (for a GOD library; with Move the ISO is removed only after the converted game is in place), the GOD folder **layout**, **Replace existing**, and **Also copy to another place**, such as straight on to the Xbox's drive, in the same job.
4. Read the **plan** and press **Start**. It says what will happen to each game and where it ends up, and refuses to start if anything is wrong: the destination is read-only or offline, there isn't enough free space on a destination, a file is too big for FAT32, a link isn't possible, or something different is already in the way. It also warns when the game is already in a destination under another name, and when the source looks damaged.

Games already in libraries can be sent the same way: tick them on a library page and press **Send to…**.

A multi-disc game goes into one folder; moving into a folder that already exists merges (identical files are skipped, a clashing file is a problem). Nothing is overwritten unless you ask.

## Compare

**Compare** lays two libraries side by side, for example your GOD library against the Xbox's drive: games in both, only in one, and **damaged copies** with the reason. Games are matched by title ID and media ID/disc, not by folder name. From the results, tick games and **Send selected…** (or **Replace from…** for damaged copies) opens the same import dialog with the destination filled in.

## The Xbox drive page

**Xbox drive** in the sidebar is the one place to manage the console's external drive. The first time, it asks for the agent's address and token and the drive's `Games` folder; after that it shows the drive's space and compares it with a library of your choice (GOD by default). The same comparison is on the **Compare** page for any two libraries. Tick games in a section, then:

- **Only in the library** (missing from the drive): **Copy**, **Move** or **Delete**.
- **Only on the drive**: **Copy** or **Move** them back to the library, or **Delete** them from the drive.
- **In both**: **Replace** either side's copy from the other, or delete from either side.
- **Damaged on the drive**: **Replace from the library**, or delete.
- **Mirror**: makes the drive match the library. It copies what is missing, replaces damaged copies, and (only if you tick it, and after you confirm the list) deletes what the library doesn't have. Both libraries should be tidied to the same layout first, so they end up as mirror images.
- **Add games…** picks any games from any library to put on the drive. **Tidy folders…** renames the drive's game folders to the standard layout, exactly as on a library.
- **Duplicates**: a game that is in a library more than once is flagged, with **Review duplicates…**. The copy to keep is chosen by fixed rules, and each removal says why: a reachable copy, then one that isn't damaged, then the larger one, then the one already in the tidy place (`Game Name/TitleID`), then the folder higher in the list, then the newer files. You can untick any copy to keep it too, and deleting always asks for confirmation.

Every action builds a plan first and runs as a normal job. When it finishes the drive is rescanned, so the page always matches what is really there.

### Fix misplaced items

**Fix misplaced items…** on the Xbox drive page looks for things in the wrong folder. The console only uses add-ons (DLC) and title updates found in `Content/0000000000000000/<TitleID>/<type>/`, so an add-on kept in `Games`, or a package in a stray folder inside `Content`, is never used. It lists each one with where it would go, and moves it (a rename on the same disk, never overwriting) only when you press the button. It needs a folder with the role **Console content** (your `Content` folder). Trainers found inside a profile's save folder go to a folder with the role **Trainers**. Spare copies already in Content are shown but never deleted; saves and games are never touched.

#### Fix misplaced items on a library

Every library page has **Fix misplaced items…**. It looks through the library's folders, and the folders of other libraries that sit inside them (so a library pointing at the root of all your Xbox files covers everything below it), and recognises by the Xbox folder structure:

| Found | Goes to the library of kind |
|---|---|
| `…/<TitleID>/00007000` (also 5000, 4000, D0000) | GOD games |
| `…/<TitleID>/00000002` | Add-ons (DLC) |
| `…/<TitleID>/000B0000` | Title updates |
| `…/<TitleID>/00000001` | Game saves |
| a folder holding only `.iso` files, or a loose `.iso` | ISO games |

Each item goes into the **primary** folder of that library, which is the first folder in its list; use **Make primary** next to a folder on the library page to change it. Whole folders are renamed into place (never copied or overwritten, only on the same disk). Archives (`.zip`, `.7z`), folders mixing several kinds, and anything it doesn't recognise are listed or left alone, never guessed at. If you have no library of the needed kind (for example Add-ons (DLC)), the item is listed with that as the reason. Nothing moves until you press the button.

On the Xbox drive, add-ons and updates live together in the single **Console content** folder (`Content`), so the drive page has one option for it, not separate DLC and update folders.

### Folders on the drive, and tidy

A drive can have several folders, each with a role: **Games** (and anything else), **Add-ons (DLC)**, **Title updates**, **Console content** (the console's own `Content` folder, which holds DLC, updates and saves in the console's layout, so tidy leaves it alone), **Trainers**, **Mods**, **Homebrew** or **Emulators**. Don't give `Content` the DLC or Updates role: tidy would file add-ons there as `Game Name/TitleID`, which the console can't read. Open **Folders on this drive** on the Xbox drive page to add one (for example a `DLC` folder next to `Games`; it is created on the drive if it isn't there, and the drive's address and token are reused, never sent to your browser) or to change a role. **Tidy folders…** then:

- renames each game's folder to the standard layout (`Game Name/TitleID`, or just `TitleID`; the layout is in Settings);
- moves each add-on and title update into the folder with the matching role, also as `Game Name/TitleID`, when there is one (otherwise it stays where it is);
- lets an add-on or update join a title folder that already exists (a game has many), file by file, and refuses if any file would be overwritten. A game never goes into a folder that already exists;
- works the same on a library on this server and on the drive, so using the same layout on both gives mirror images after a sync.

Moving between two folders is only possible on the same disk (or the same drive agent); across disks the plan says so and points to Move on the Compare page.

### Replace the game

Ticking **Replace the game if it is already there** in the Send dialog swaps an old copy for a new one. The old copy is found by title ID and disc, whatever its folder is called (so `Simpsons Game, The (USA)` is found when the new copy goes to `The Simpsons Game`), and the plan lists exactly what will be removed. Only the game's own files are removed (the `00007000`, `00005000`, `000D0000` and `00004000` folders inside the title folder). Saved games (`00000001`), add-ons (`00000002`), title updates (`000B0000`) and anything else are left alone, and the title folder is only removed if nothing else is left in it. If the old copy is in the folder the new one goes to, it is removed first; otherwise it is removed only after the new copy is in place. Replacing one disc of a multi-disc game isn't supported yet.

Saves on the console live in its profile area, not in the `Games` folder, so replacing a game never touches them either way.

### Jobs wait their turn

When a job needs a disk or drive that another job is using, it doesn't fail: it waits in the queue ("Waiting for job #9") and starts by itself when that job finishes. Queued jobs can be cancelled from the Jobs page.

## How well this has been checked

- On a real 2 TB FAT32 Xbox drive: a read-only agent scanned its 175 GB `Games` folder in about 5 seconds. 38 of 41 headers read; the 5 problems it reported are real (checked by hand: for example Grand Theft Auto: Episodes from Liberty City has 25 of the 42 data files its header expects).
- On real ISOs: 15 discs were read correctly (title IDs, media IDs, multi-disc numbering for GTA V and L.A. Noire). A real 7.5 GB ISO was converted to GOD and back, and unpacking the original and the rebuilt ISO gave **7,968 files, all byte-identical**.
- The agent, remote folders, transfers (copy, resume, links, moves on and across filesystems), import and compare are covered by automated tests, including real HTTP between a RustyBox and an agent.
- Not yet checked: writing to the real drive through the agent (only read-only runs were done), and the whole thing across the network (the PC's firewall has to allow the port first).
