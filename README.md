# BingBU: Mech Royale bot

A bot for the Mech Royale track of the Builders Buildathon. It speaks the organisers'
`mechfight/1` protocol: JSON lines in on stdin (one observation about ten times a second), JSON
lines out on stdout (move, aim, fire), one answer per observation within 50 ms.

Written in Rust: one small static-ish binary, no runtime dependencies, a few hundred microseconds
per decision on one CPU.

## What it does

In order of priority:

1. **Protocol safety.** Answers only the newest queued observation, always echoes its tick, never
   prints anything else on stdout, catches panics and answers with a neutral action, exits on `end`.
2. **Zone.** Knows where the circle will be at any time in the current phase, heads for a spot
   inside the next circle early, and boosts when it is late.
3. **Contact.** Faces whoever is shooting (rear armor), leads the gun, charges and releases the
   laser on target, fires lightning inside its cone, strafes with random reversals at medium range,
   and breaks line of sight to regenerate when it is losing.
   - Against the practice squad (house bots that never fight each other) every hunter is fought.
   - A free-for-all is recognised within seconds: a death it did not cause in the first 30 s (two
     later), or two of three softer signs (gunfire all around, other mechs losing health to each
     other, two different attackers before its first kill). From then on it stays out of sight of
     every mech it knows about, runs to the nearest cover by walking distance when seen, loots only
     where nobody is looking, and fights only with a clear advantage or when one mech has kept
     after it for 10 s.
   - For the first 20 s of any match, before the mode is known, it behaves the careful way.
4. **Loot.** Wants lightning plus laser (the gun gives way to the second of them) and closes to
   30 m when it holds lightning; collects cubes,
   shoots crates and drones when no mech is near, and goes for a supply drop only when nobody is
   near it. Upgrades are picked from a fixed order, damage first.
5. **Position.** Otherwise holds a covered spot near the edge of the next circle, out of sight of
   every mech it knows about, and keeps its torso scanning.

Behaviour switches live in `Policy` in `src/brain.rs`. Two that were tried and scored worse in
practice are kept switched off: fighting from roofs, and fighting at long range.

Navigation uses the organisers' map (`v2/world.json`, `v2/terrain.i16`, mounted at `/arena`):
buildings and bridges are rasterised onto a 2 m grid inflated by the mech radius, paths come from
A*. Walkability is checked metre by metre on the terrain grid. When the mech is pressed against
something, or three seconds bring it no closer to its goal, it jumps and jets over the obstacle
(this is how it leaves the stadium bowl or a courtyard).

## Layout

| Path | What it is |
| --- | --- |
| `src/main.rs` | stdin/stdout loop |
| `src/map.rs` | map loading, navigation grid, line of sight, A* |
| `src/brain.rs` | memory, zone timing, fight / evade / loot / hold decisions, aiming |
| `src/bin/sim.rs` | a rough local simulator used only to catch gross bugs (not in the image) |
| `src/bin/mapcheck.rs` | dev tool: compares the map model with positions taken from public replays (not in the image) |
| `src/bin/poltest.rs` | dev tool: feeds one synthetic situation to a named policy (not in the image) |
| `src/bin/feed.rs` | dev tool: runs the brain over a recorded observation stream and prints its decisions (not in the image) |
| `Dockerfile` | the submission image |

## Build, check, run

```sh
cargo build --release --locked
# conformance check from the organisers' kit (kit path is yours):
python3 check/check.py -- /path/to/target/release/bot
docker build --platform linux/amd64 -t bingbu-mech .
python3 check/check.py --map-dir /arena -- docker run --rm -i --network none --cpus 1 --memory 512m \
  --read-only --tmpfs /tmp:rw,size=64m --user 65534:65534 -v "$PWD/arena:/arena:ro" bingbu-mech
# local simulator: <map dir> [matches] [seed] [practice|ffa] [copies of this bot] [verbose]
./target/release/sim /path/to/arena 6 11 practice 1
```

## Pre-existing code and libraries

- The Rust standard library.
- [`serde_json`](https://crates.io/crates/serde_json) 1.0.140 (MIT OR Apache-2.0) and the crates it
  pulls in (`serde`, `serde_core`, `itoa`, `ryu`, `memchr`), all pinned by `Cargo.lock`.
- Base images `rust:1.100.0-slim-bookworm` and `debian:bookworm-slim` from Docker Hub, pinned by digest.
- The organisers' kit is not included here. The bot follows its `protocol.md` and reads its map
  files at run time; none of the kit's wrapper code is used.
- Written with AI assistance (Claude Code).

## License

MIT, see `LICENSE`.
