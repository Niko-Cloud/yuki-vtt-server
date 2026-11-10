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

## Building

Needs Rust 1.85 or newer.

```sh
cargo test
```

## License

AGPL-3.0-only. See [LICENSE](LICENSE).
