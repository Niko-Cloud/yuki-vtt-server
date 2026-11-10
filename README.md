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

## Building

Needs Rust 1.85 or newer.

```sh
cargo test
cargo run -p yuki-vtt-server -- --data ./data --origin https://your-website.example
```

On its first start it makes the DM's key and keeps it in `data/owner-key`. It listens on
this computer only (port 4610); players reach it through the tunnel, not the port.

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
