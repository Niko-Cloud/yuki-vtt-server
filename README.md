# Yuki VTT Server

A small program that hosts a Yuki Char Maker game table on the DM's own computer. Players
and the DM keep playing in the Yuki Char Maker website; the table's live data (the map,
tokens, initiative, rolls, the log, pictures) lives here, on the DM's machine, and goes to
the players through a secure tunnel.

It is open source so anyone can read exactly what runs on their computer and what it
receives.

**Status: early work.** Not ready to use yet.

## What it stores and receives

- The table: maps, tokens, props, drawings, initiative, the log, chat and rolls.
- Pictures the DM uploads for maps, tokens and handouts.
- For each player: the name and the character sheet snapshot the website sends when they
  join, and the play state (hit points, conditions, spell slots used) during the game.

It never receives passwords, website cookies or anything from the player's account.

## Layout

| Path | What it is |
|---|---|
| `crates/rules` | The table rules: dice, initiative, hit points, the map grid |
| `crates/server` | The host itself, also the `yuki-vtt-server` program for running without a window |
| `apps/desktop` | The window and tray app (Tauri) that starts and stops the host |

## Building

Needs Rust 1.85 or newer.

```sh
cargo test
# Once: connect the host to your Yuki Char Maker account. It shows a code to type in the
# website's Settings > My hosts.
cargo run -p yuki-vtt-server -- pair --website https://your-website.example
# Then, each game night:
cargo run --release -p yuki-vtt-server -- --tunnel
```

Pairing keeps the website's address, the host's key and the shared secret in the data
folder. From then on the host trusts only that website's pages, and tells the website its
tunnel address every minute, so the DM's invite links keep working.

Add `--tunnel` to open a Cloudflare quick tunnel (it needs the
[cloudflared](https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/)
program): the host prints the public `https://....trycloudflare.com` address players reach it
on. The address is new each time it starts.

On its first start it makes the DM's key and keeps it in `data/owner-key`. It listens on
this computer only (port 4610); players reach it through the tunnel, not the port.

## The window app

`apps/desktop` is the same host with a small window and a tray icon, for DMs who would
rather not use a terminal:

- **Connect** to your account with the code it shows (the same pairing as above).
- **Start hosting** and **Stop hosting**, from the window or the tray.
- It gets the cloudflared program by itself on the first start.
- Closing the window keeps it running in the tray; **Quit** in the tray menu stops hosting
  and closes it. Opening the app again shows the one already running.

```sh
cargo run -p yuki-vtt-desktop                      # run it
cargo install tauri-cli --version '^2' --locked    # once, to make installers
cd apps/desktop && cargo tauri build               # the installer for this system
```

Installers for Windows, macOS and Linux are built on GitHub by the **Installers** workflow
(Actions tab > Installers > Run workflow, or push a tag like `v0.1.0`); they are attached to
the run. They are not signed yet, so Windows and macOS warn before the first start.

On Linux, the `.deb`, `.rpm` and plain program are a few megabytes because they use the
system's web engine (WebKitGTK 4.1). The AppImage is over 100 MB because it carries its own
copy, so it runs where that isn't installed.

On Linux the tray icon needs `libayatana-appindicator` (or `libappindicator`), and a desktop
that shows tray icons. Without them the app still works from its window, which then
minimizes when closed.

## How it keeps a table safe

- Only pages from the paired website may call it from a browser, and the live channel
  checks the page's origin too.
- The DM holds the host's key. Players hold a seat token from joining; the host keeps only
  hashes of both, so a copied database lets no one in.
- The website signs every character it hands over, so a player can't change their sheet
  on the way to the table.
- Hidden tokens and hidden rolls never leave the host for players who may not see them.

## License

AGPL-3.0-only. See [LICENSE](LICENSE).
