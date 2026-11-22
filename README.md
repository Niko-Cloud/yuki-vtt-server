# Yuki VTT Server

A small program that hosts a Yuki Char Maker game table on the DM's own computer. Players and the DM keep playing in the Yuki Char Maker website; the table
itself (maps, tokens, fog, initiative, rolls, the log, pictures, music) lives on the DM's
machine and reaches the players through an encrypted tunnel.

It is open source (AGPL-3.0) so that anyone can read exactly what runs on their computer,
what it sends out and to whom. [What it sends over the internet](#what-it-sends-over-the-internet)
lists every connection it makes, with the file each one is written in, so you can check it
yourself.

**Status:** feature complete for a first release, being tested. Installers are not signed yet.

## Contents

1. [In one minute](#in-one-minute)
2. [What it sends over the internet](#what-it-sends-over-the-internet)
   - [Every connection it opens](#every-connection-it-opens)
   - [What can reach it](#what-can-reach-it)
   - [What the paired website knows and can do](#what-the-paired-website-knows-and-can-do)
   - [What Cloudflare can see](#what-cloudflare-can-see)
   - [What players receive, and what they never do](#what-players-receive-and-what-they-never-do)
   - [What it keeps on your disk](#what-it-keeps-on-your-disk)
   - [Programs it starts](#programs-it-starts)
   - [What it never does](#what-it-never-does)
   - [Check it yourself](#check-it-yourself)
3. [Features](#features)
4. [How a game night goes](#how-a-game-night-goes)
5. [How it works](#how-it-works)
6. [Run it](#run-it)
   - [The window app](#the-window-app)
   - [From a terminal](#from-a-terminal)
   - [Command line reference](#command-line-reference)
   - [Backups](#backups)
   - [The picture library folder](#the-picture-library-folder)
7. [Project layout](#project-layout)
8. [The table API](#the-table-api)
9. [Development](#development)
10. [Roadmap](#roadmap)
11. [License](#license)

---

## In one minute

- **What it is:** the DM's computer becomes the game server for their tables. The website stays
  the screen everyone plays on.
- **Why:** pictures, maps and the live game go out over the DM's own connection, not through a
  central server, so running tables costs nobody anything and the DM keeps their own files.
- **Who it is for:** a DM who runs Yuki Char Maker tables for friends and wants the table on
  their own computer. It works only with the Yuki Char Maker website and holds no rule books.
- **What it needs:** a Yuki Char Maker account to pair with, and an internet connection. No
  router setup, no open ports, no Cloudflare account.
- **What leaves your computer:** the table, to the players at it, through a Cloudflare tunnel;
  a "here is my address" note to the paired website once a minute; and three small things
  described below (a one-time download, a 2 MB speed test, pictures you ask it to fetch).
  Nothing else.

---

## What it sends over the internet

### Every connection it opens

The host makes outgoing connections in exactly these places. There is no other networking code
in the program: every `reqwest` client is in one of the four files named here, and the tunnel is
the only program it starts that talks to the network.

| # | To | When | What is sent | What comes back | Code |
|---|---|---|---|---|---|
| 1 | **Cloudflare's tunnel network** (through the official `cloudflared` program) | While hosting | The table traffic of the people at your tables (see [What players receive](#what-players-receive-and-what-they-never-do)) | Their requests | `crates/server/src/tunnel.rs` |
| 2 | **The paired website**, `POST /api/hosts/pair` | Once, when you pair | This computer's name (you can change it) and the DM key the host made for you | A pairing code to type on the website, then the host key and the shared secret | `crates/server/src/website.rs` (`pair`, `pair_with_link`) |
| 3 | **The paired website**, `PUT /api/hosts/me` | When the tunnel gets an address, then once a minute | The tunnel address (`https://....trycloudflare.com`), signed in with the host key | OK | `website.rs` (`check_in`) |
| 4 | **Its own tunnel address**, `GET /api/health` | When a new tunnel address appears, until it answers (at most about 90 seconds) | Nothing but the request | The host's own name and version | `website.rs` (`answers`) |
| 5 | **The paired website's front page**, and `https://speed.cloudflare.com/__up` | Once each time hosting starts in the window app (the connection check; the terminal program doesn't run it) | A plain request to the website; **2 MB of zeros** to Cloudflare's public speed test | Whether each answered, and how long the 2 MB took | `crates/server/src/connection.rs` |
| 6 | **GitHub**, `github.com/cloudflare/cloudflared/releases/download/2026.11.1/...` | Only if `cloudflared` isn't installed: once, the first time you host | Nothing but the download request | The official `cloudflared` file, checked against the SHA-256 written in the code before it is kept | `crates/server/src/cloudflared.rs` |
| 7 | **A web address you paste** into the picture library | Only when the DM asks for a picture from a web address | Nothing but the request | The picture (12 MB at most, 3 redirects at most, ports 80 and 443 only) | `crates/server/src/web_picture.rs` |

Every request it makes says `yuki-vtt-server/<version>` as its user agent. No cookies, no
account details, no identifiers of you or your players are added to any of them.

Number 7 only fetches addresses on the open internet. Every name and every redirect is checked
and the connection goes to the exact address that was checked, so a pasted link can never make
the host reach your router, your other computers or anything else on your home network.

The website address in 2 and 3 must be `https://`. Plain `http://` is only accepted for a
website on this computer or your home network (for development), because the host key and the
shared secret travel over it. An older pairing with a plain `http://` website on the internet is
ignored and you are asked to pair again.

### What can reach it

- **It listens on this computer only:** `127.0.0.1`, port 4610 by default. It does not open a
  port on your network or your router, and nothing on your wifi can reach it directly.
- **Players reach it only through the tunnel.** `cloudflared` makes an outgoing connection to
  Cloudflare and carries the players' requests back to `127.0.0.1:4610`.
- **Only the paired website's pages may call it from a browser** (CORS, and the live channel
  checks the page's `Origin` too). `--origin` adds another website, for development.
- **Every request is checked for who sends it:**

  | Who | How they prove it | What they may do |
  |---|---|---|
  | The DM | The DM key, as `Authorization: Bearer ...` | Everything at their tables |
  | A co-DM | Their seat token, with the co-DM role | What the DM gave co-DMs |
  | A player | Their seat token in the `x-seat-token` header (never in the address) | Their own token and what the DM allows |
  | A picture or live-channel request | A pass made from the seat, signed by the host, good for one hour | Only that table's pictures or live channel |
  | Anyone else | Nothing | Read `/api/health`, ask to join with an invite (the DM can make the table ask first, or keep out people without an account) |

- **The host keeps no copy of seat tokens:** only their SHA-256 hashes are in the database, so a
  copied database lets no one sit down at a table. The DM key is in `owner-key` (readable by your
  user only) and only its hash is used to check requests.
- **Characters can't be faked:** the website signs every character it hands over (HMAC with the
  shared secret), and the host checks the signature, the table, the seat and the time (10
  minutes), so a player can't edit their sheet on the way in or replay an old one.
- **Limits per visitor, per minute:** 30 uploads, 20 joins, 300 changes, 1,200 ruler and pointer
  marks, 900 reads. Over that the visitor gets `429` for the rest of the minute. Visitors are
  told apart by Cloudflare's `cf-connecting-ip`; the counts live in memory for one minute and are
  never written down.
- **Size limits:** 2 MB for a request, 8 MB for a map picture, 4 MB for a library picture, 25 MB
  for a video map, 10 MB for a video token or music, 1,000 pictures per library. Backups are
  size-checked (256 MB per table) before anything is unpacked.

### What the paired website knows and can do

Pairing joins the host to one Yuki Char Maker account. The website then knows:

- the host's name and its current tunnel address (from the check-ins), and when it last checked
  in;
- the party for each hosted table: its name and its invite;
- **the DM key and the shared secret**, kept sealed (encrypted) in its database. It gives the DM
  key only to the signed-in DM's own browser, so the DM's pages can run the table. This means
  **the website can act as the DM on your host.** If you don't trust the website, don't pair.

The website's own server calls your host through the tunnel, with the DM key, for a few party
chores only (`apps/api/src/party/hosted.service.ts` and `table-file.service.ts` in the website):

| Call | When | What it sends | What it gets back |
|---|---|---|---|
| `POST /api/tables` | The DM makes a party on this host | The party's id, name, edition and the DM's display name | The table's invite token |
| `PATCH /api/tables/{id}`, `DELETE /api/tables/{id}` | The DM renames or deletes the party | The new name | Nothing |
| `POST /api/tables/{id}/invite` | The DM renews the invite link | Nothing | The new invite token |
| `GET /api/tables` | The DM moves a website table onto this host | Nothing | The list of tables: ids, names, invite tokens |

The website does **not** receive the table's contents: maps, tokens, fog, the log, chat, rolls,
handouts and pictures go from your computer to the players' browsers through the tunnel and are
never sent to the website by the host. The website computes each character's sheet (it holds the
books) and the player's browser hands that signed sheet to the host when they sit down.

To unpair, use **Unpair** in the window app or delete `website`, `host-key` and
`website-secret` from the data folder, then remove the host in the website's Settings > My hosts.

### What Cloudflare can see

The tunnel is Cloudflare's free "quick tunnel". The connection from a player to Cloudflare is
https, and so is the one from Cloudflare to your computer, but **a quick tunnel ends at
Cloudflare**: Cloudflare's servers decrypt the traffic and pass it on. Cloudflare can therefore
see what goes through it: the map, rolls, chat, pictures and characters, and the players' IP
addresses. This is the same as for any website behind Cloudflare. Cloudflare's own terms and
privacy policy apply to the tunnel. Nothing else goes through any third party.

### What players receive, and what they never do

The host makes a separate view of the table for each person and filters it **on your computer,
before it is sent**. A player never receives:

- hidden tokens and props, hidden token names, and token descriptions;
- tokens they can't see right now on a map with dynamic fog (walls, closed doors, darkness and
  their vision decide; their own and shared tokens are always sent so they can move them);
- a monster's carried light while the monster itself is out of sight;
- secret doors, DM notes, hidden areas, hidden drawings, light markers;
- hidden rolls (until the DM reveals them) and whispers to someone else;
- handouts not yet shown, prefabs, pictures from the DM's other tables;
- monsters' hit points and armour class, and the names of hidden monsters in the initiative list.

The test `never_lets_a_dm_only_thing_reach_a_player` in `crates/server/tests/maps.rs` builds a
table holding every one of these, marked `SECRET-...`, and fails if any of them shows up in a
player's download, their live updates or the older log.

**Pictures** go out as WebP no wider than they need to be, with everything a camera or editor
wrote into the file (such as where a photo was taken) left out. The original in your library
folder is never changed; the smaller copy is kept in `cache/`.

### What it keeps on your disk

Everything is in one data folder: `./data` when run from a terminal (or `--data <folder>`), and
the app's own folder for the window app (on Linux `~/.local/share/app.yukicharmaker.vttserver`,
on Windows `%APPDATA%\app.yukicharmaker.vttserver`, on macOS
`~/Library/Application Support/app.yukicharmaker.vttserver`). The window app has an **Open the
data folder** button.

| Path | What it is |
|---|---|
| `tables.db` | One SQLite file: tables, seats (token hashes only), maps, tokens, fog, the log, initiative, characters as the website signed them |
| `Library/Tokens/...`, `Library/Props/...`, `Library/Maps/...` | Your pictures and music, one folder per item, editable by hand |
| `pictures/` | Pictures from tables before the library existed |
| `cache/` | The smaller WebP copies sent to players; safe to delete |
| `backups/<table>/` | Automatic table copies, see [Backups](#backups) |
| `bin/cloudflared` | The downloaded tunnel program (only if it wasn't installed) |
| `owner-key` | The DM key (readable by your user only) |
| `website`, `host-key`, `website-secret` | The pairing (readable by your user only) |
| `start-on-open` | Whether the window app starts hosting when it opens |

Nothing is written anywhere else, except the `yukivtt://` link handler the installer registers.
Logs go to standard output (the terminal) and are never saved to a file by the host.

### Programs it starts

| Program | Why | When |
|---|---|---|
| `cloudflared tunnel --no-autoupdate --url http://127.0.0.1:<port>` | The tunnel. `--no-autoupdate` keeps it from updating itself | While hosting |
| `systemd-inhibit` (Linux), `caffeinate` (macOS); a system call on Windows | Keep the computer from sleeping mid-game | While hosting; let go when you stop |
| Your file manager | Only when you press an "Open the ... folder" button | On request |

### What it never does

- No analytics, tracking, telemetry, crash reports or "phone home".
- No automatic updates: it changes only when you install a new version.
- No ads, no accounts of its own, no payments.
- It never opens a port on your router or your network.
- It never sends the table to the website, and never sends anything about you or your players
  to anyone but the people at your table (and Cloudflare, which carries the tunnel).

### Check it yourself

- **Read the code that talks to the network:**
  `grep -rn "reqwest\|Command::new" crates apps` lists every place, all in the files in the table
  above.
- **Watch it:** while hosting, `ss -tnp | grep -E "yuki|cloudflared"` (Linux) or `netstat -anb`
  (Windows, as admin) shows its connections: the tunnel to Cloudflare, and the website once a
  minute.
- **Build it from source** instead of using an installer (see [Run it](#run-it)); the program is
  the same.
- **Check cloudflared:** the SHA-256 of each `cloudflared` file is written in
  `crates/server/src/cloudflared.rs` and compared before the file is kept. Or install
  `cloudflared` yourself and the host uses yours.

---

## Features

The host stores and checks everything a live table needs; the website draws it.

| Area | What the host keeps and checks |
|---|---|
| Tables | Create, rename, lock, delete; one invite per table, renewable; a TV screen link that follows the DM |
| Seats | Join with the invite; ask-to-join (Let in / Turn away); keep out guests without an account; roles (player, co-DM), permissions, remove, ban |
| Boards and maps | Boards in folders, one shown to players at a time, copy a board. A board holds map pictures from the library, each scaled so its own lined-up grid matches the board's squares; the board grows on any side to hold them. Walls, doors, fog, lights, notes, drawings, props and passages attached to a map picture move, turn and resize with it, and go (and come back on undo) with it. Grid size, style and offset, diagonal rules; a table background |
| Tokens and props | Add, move, resize and rotate freely, flip, hide, lock, number, markers, layers, attach to another token, GIF and video tokens, prefabs |
| Drawing and text | Pen, lines and shapes (filled, dashed, curved), polygons, text with fonts and sizes, a rubbing eraser |
| Areas and rulers | Spell templates, shared rulers with waypoints, pointer trails and pings |
| Fog and light | Walls, doors (locked, secret, windows), painted fog, placed and carried lights, vision from the sheet, explored memory per map, shared party sight |
| Pictures and music | A library per DM as a folder on disk, folders, tags, favourites, upload from a web address, handouts, looping music, rain and snow |
| Dice and log | Rolls with advantage, hidden DM rolls revealed later, chat, whispers, framed NPC lines, sheet change notes for the DM |
| Initiative | Encounters, monsters and players, rolls and rerolls, turns, condition timers, feet moved this turn |
| Table extras | X-card (anonymous), a DM countdown timer, shared loot list and journal, follow-me view |
| Hosting | Pairing, the tunnel, check-ins, rate limits, connection check, keep-awake, automatic and manual backups |

## How a game night goes

1. **Once:** install the app, press **Connect**, and type the code it shows into the website's
   Settings > My hosts (or press "Connect this computer" on the website, which opens the app and
   asks you to confirm).
2. **Each session:** press **Start hosting**. The app opens the tunnel, checks your connection
   (and warns if your upload is under 5 Mbit/s) and tells the website its address.
3. **On the website**, as the DM, make a party hosted on your computer (or move an existing
   website table onto it). Send the invite link. It keeps working even though the tunnel address changes each start.
4. **Players** open the link, pick a character, and sit down (straight away, or after you let
   them in).
5. **Play.** Everything happens in the website's map room; the app shows who is waiting and
   stays in the tray.
6. **After:** press **Stop hosting**, or just quit. The table is saved as you play, and a copy is
   kept every 20 minutes.

## How it works

```
 Player's browser ──┐                       ┌── DM's computer ─────────────────────┐
 (website page)     │   https + WebSocket   │  yuki-vtt-server                     │
                    ├──── Cloudflare ──────►│   API on 127.0.0.1:4610              │
 DM's browser   ────┘    quick tunnel       │   tables.db + Library/ in the data   │
 (website page)                             │   folder                             │
        │                                   └──────────────┬───────────────────────┘
        │  sheets, sign-in, party pages                    │ pairing; "my address is ..."
        └────────────── Yuki Char Maker website ◄──────────┘ once a minute
```

1. **Pairing, once.** The host asks the website for a code; the DM types it on the website. The
   host keeps the website's address, its host key and the shared secret in the data folder.
2. **Hosting.** The host listens on this computer only and opens a Cloudflare quick tunnel. Once
   the tunnel answers, it tells the website the address, and again every minute.
3. **Playing.** The website's pages call the host straight from the browser, under `/api`. The
   website works out each character's sheet and hands the browser a signed copy to give the
   host; the host has no books and no character rules, only the table rules in `crates/rules`
   (dice, initiative, hit points, the grid, sight).
4. **Live updates.** Each open table screen holds a WebSocket. When something changes, the host
   says "change", and each screen asks for its own view, getting only what changed since the
   version it has (a few hundred bytes after a move). Answers are compressed (brotli or gzip).
   (A quick tunnel holds back server-sent events, so the host uses WebSocket.)
5. **Storage.** One SQLite file, upgraded in place by numbered steps, and the pictures as plain
   files in `Library/`.

## Run it

### The window app

Download the installer for your system from the
[releases page](https://github.com/Niko-Cloud/yuki-vtt-server/releases), or build it:

```sh
cargo run -p yuki-vtt-desktop                      # run it from source
cargo install tauri-cli --version '^2' --locked    # once, to make installers
cd apps/desktop && cargo tauri build               # the installer for this system
```

- **Connect** to your account with the code it shows.
- **Start hosting** / **Stop hosting**, from the window or the tray; optionally start hosting
  whenever the app opens.
- Shows the tunnel address, the connection check, and people waiting to join (with a
  notification), with **Let in** and **Turn away**.
- **Back up** and **Restore** a table; open the library, backups or data folder.
- Closing the window keeps it in the tray; **Quit** stops hosting. Opening the app again shows
  the one already running.
- The website's "Connect this computer" and "Start hosting" buttons open `yukivtt://` links.
  Pairing by link always asks you to confirm in the window first.

The window is plain HTML, CSS and JavaScript in `apps/desktop/ui` (no npm, no bundler), in Tauri
2.

**Installers** for Windows, macOS and Linux are built on GitHub by the **Installers** workflow
(Actions > Installers > Run workflow, or a tag like `v0.1.0`). They are not signed yet, so
Windows and macOS warn before the first start.

**Linux:** the `.deb`, `.rpm` and plain program use the system's WebKitGTK 4.1 and are a few
megabytes; the AppImage carries its own copy and is over 100 MB. The tray icon needs
`libayatana-appindicator` and a desktop that shows tray icons; without them the app works from
its window.

### From a terminal

Needs Rust 1.85 or newer. `cloudflared` is downloaded by itself if it isn't installed.

```sh
cargo test
# Once: connect to your account. It shows a code to type in the website's Settings > My hosts.
cargo run -p yuki-vtt-server -- pair --website https://your-website.example
# Each game night:
cargo run --release -p yuki-vtt-server -- --tunnel
```

It prints the `https://....trycloudflare.com` address. A fresh tunnel can take up to a minute
to answer; the host tells the website only once it does.

To try it against a website running on your own computer (`pnpm dev` in Yuki Char Maker):

```sh
cargo run -p yuki-vtt-server -- pair --website http://localhost:5173 --data ./dev-data
cargo run -p yuki-vtt-server -- --data ./dev-data --tunnel
```

### Command line reference

```
yuki-vtt-server [options]                      host tables
yuki-vtt-server pair --website <url> [options] connect this host to your account, once
yuki-vtt-server backup <table id> <file>       keep a table in one file
yuki-vtt-server restore <file> [--replace]     bring a backed-up table onto this computer
```

| Option | Meaning |
|---|---|
| `--data <folder>` | Where tables and pictures are kept (default `./data`) |
| `--port <number>` | The port on this computer (default `4610`) |
| `--tunnel` | Open a Cloudflare quick tunnel so players anywhere can reach this host |
| `--origin <url>` | Another website allowed to use this host; can be given more than once |
| `--website <url>` | `pair`: the Yuki Char Maker website |
| `--name <text>` | `pair`: what the website calls this host (default: this computer's name) |
| `--replace` | `restore`: overwrite a table with the same id |

| Environment variable | Meaning |
|---|---|
| `YUKI_VTT_ORIGINS` | Extra allowed websites, comma separated (same as `--origin`) |
| `RUST_LOG` | Log level, default `info` (for example `RUST_LOG=yuki_vtt_server=debug`) |

### Backups

- **Automatic:** every 20 minutes, each table that changed is saved as a `.yukitable` in
  `backups/<table>/`, keeping the newest 10.
- **By hand:** `backup` and `restore` (or the app's buttons). A `.yukitable` is a gzipped tar
  with `table.json` (every row the table owns) and its pictures; it opens on this or another
  computer.

```sh
cargo run --release -p yuki-vtt-server -- backup <table id> my-campaign.yukitable
cargo run --release -p yuki-vtt-server -- restore my-campaign.yukitable
```

### The picture library folder

`Library/<Kind>/<item>/<file>`, for example `Library/Tokens/Goblin/goblin.png` or
`Library/Maps/Goblin cave/map.png`. Copy pictures in, rename or delete folders by hand: the host
notices while it runs and on the next start. Uploads from the website land in the same folders.

## Project layout

| Path | What it is |
|---|---|
| `crates/rules` | The table rules: dice, initiative, hit points, the grid, sheet change notes. No I/O |
| `crates/server` | The host, also the `yuki-vtt-server` program for running without a window |
| `crates/server/src/lib.rs` | The router: every route under `/api`, CORS, compression, body and rate limits |
| `crates/server/src/db.rs` | The SQLite schema and its upgrade steps |
| `tables.rs`, `seats.rs`, `requests.rs` | Tables, invites, joining, roles, permissions, bans, join approval, guests |
| `play.rs` | The log, rolls, chat, the encounter, and each person's view of the table |
| `maps.rs`, `areas.rs`, `rulers.rs`, `fog.rs`, `sight.rs` | Maps, tokens, props, drawings, templates, rulers, walls, doors, light, what a player can see |
| `pictures.rs`, `library.rs`, `served.rs`, `web_picture.rs` | Uploads, the library folder, the WebP copies, pictures from a web address |
| `events.rs`, `delta.rs` | The WebSocket live channel, and sending only what changed |
| `access.rs`, `secrets.rs`, `snapshot.rs`, `limits.rs` | Who may do what, secret files, signed characters, rate limits |
| `website.rs`, `tunnel.rs`, `cloudflared.rs`, `connection.rs` | Pairing and check-ins, the tunnel, downloading cloudflared, the connection check |
| `backup.rs`, `awake.rs`, `host.rs` | Backups, keep-awake, starting and stopping |
| `crates/server/tests` | Integration tests over the real router, including the leak test and a stress test |
| `apps/desktop` | The window and tray app (Tauri 2) |
| `.github/workflows` | `ci.yml` (format, clippy, tests) and `installers.yml` |

## The table API

Everything is under `/api`. The route list in `crates/server/src/lib.rs` is the source of truth;
the website speaks the same shapes to its own built-in tables, so a route changes on both sides.

| Routes | What they cover |
|---|---|
| `GET /health` | Name and version |
| `/tables`, `/tables/{id}`, `/lock`, `/guests`, `/ask` | Tables, locking, guests, join approval |
| `/tables/{id}/events` | The WebSocket live channel |
| `/tables/{id}/party`, `/invite`, `/seat-pass`, `/media-token`, `/sight` | The DM's party view, invites, short passes, shared sight |
| `/tables/{id}/seats/{seat}/...`, `/bans/{player}` | A seat's sheet, changes, role, permissions, ban |
| `/tables/{id}/log`, `/roll`, `/ping`, `/mark`, `/view`, `/xcard`, `/timer` | Chat, rolls, pings, live marks, follow-me, X-card, timer |
| `/tables/{id}/encounter/...` | Initiative |
| `/tables/{id}/scenes/{scene}/...` | Boards: their map pictures (laying, moving, attaching and detaching things, bringing one back), growing a board, tokens, props, drawings, templates, rulers, lighting, doors, notes, explored areas |
| `/tables/{id}/assets`, `/web-picture`, `/library/open`, `/handout`, `/music`, `/background` | Pictures, music, handouts |
| `/tables/{id}/loot`, `/journal`, `/prefabs` | Loot, journal, prefabs |
| `/join/{invite}`, `/join/{invite}/screen`, `/seat`, `/seat/status` | Joining, the TV screen, a player's own seat |

## Development

Checks (CI runs these on every pull request):

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --release --test fuzz -- --ignored   # the stress test, now and then
```

On Linux the window app needs its libraries to build, even for `cargo test`:

```sh
sudo apt-get install -y libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev
```

Conventions:

- Names come from the game (table, seat, scene, token, encounter); comments explain why.
- Rules that need no database go in `crates/rules`, with unit tests.
- A schema change is a new step at the end of `STEPS` in `db.rs`; never edit an old step.
- SQL only sees column names from fixed lists, never from a request.
- **Any new outgoing connection must be added to the table in
  [Every connection it opens](#every-connection-it-opens)** in the same pull request.
- Commit messages are short and imperative, with a scope: `feat(server): ...`.

Adding a table feature:

1. The rule (if any) in `crates/rules`, with tests.
2. The column or table as a new step in `db.rs`.
3. The handler in its module and the route in `lib.rs`; check access with `access.rs` and tell
   the live channel.
4. If players must not see it, leave it out of their view in `play.rs`, and add it to the leak
   test.
5. Add it to `backup.rs` if the table owns new rows.
6. An integration test in `crates/server/tests`.
7. The same change on the website, so built-in and hosted tables behave alike.

Branches: `main` is released work; `develop` is the next release, and `feat/...` branches open
pull requests into it.

## Roadmap

- Signed installers for Windows and macOS.
- A stable address per DM (a named Cloudflare tunnel) instead of a new one each start.
- Signed automatic updates, always with the DM's say-so.
- Hex grids.

## License

AGPL-3.0-only. See [LICENSE](LICENSE).
