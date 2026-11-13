# Aaru

**English** · [Русский](README.ru.md)

> Aaru is a fork of [konkers/irminsul](https://github.com/konkers/irminsul), the original packet-capture exporter for Genshin Impact. The original project is licensed under the MIT License, and the license file is kept in this repository.

The program (still named Irminsul inside: `irminsul.exe`) reads Genshin Impact's network traffic (packet capture, no screen scanning) and turns it into data you can use: a [Genshin Optimizer](https://frzyc.github.io/genshin-optimizer/) export in the GOOD format, a character and material browser, a farming planner, and a live map that knows what you have already collected.

Nothing is injected into the game and no game files are modified.

## Features

### Inherited from the original

- Export of artifacts, weapons, materials and characters in the [GOOD](https://frzyc.github.io/genshin-optimizer/#/doc) format, to the clipboard or to a file, with settings for what to export
- Achievement export and an achievement tracker

### Added in this fork

**Account data**

- Characters and Materials tabs with the data of the captured account, in English or Russian
- Characters and weapons that are not yet in the built-in data are filled in from live game data
- One profile per game account, always holding the freshest capture, with switching between accounts
- Starting the game through HoYoPlay in a way that lets Irminsul see its traffic, optional start with Windows, tray mode, and a progress popup while the capture runs
- A "what changed" popup showing the mora and items you gained or spent while playing
- Resuming the capture after restarting the program, without restarting the game; when the key of the connection can not be found, the map over the game says so instead of showing wrong data
- A background log that is always written and thrown away when the program closes, unless you press the button in Power Tools to keep it (so a problem that cannot be repeated is still on record)

**Planner**

- Pick characters and weapons, set target levels and talents, and see in total what has to be farmed and how much of it you already have
- Domains with the days they are open, the materials and artifacts they give, and their one-time first-clear rewards

**Interactive map**

- The official interactive map: all regions and underground places, floors, and filters for every kind of marker
- The collected state is taken from the game itself for oculi, chests, puzzles, seelies, world quests and challenges, plus manual marks for what the game does not report
- Your position and floor, and a map overlay that can be shown over the game with a hotkey
- **Show on the map** for any material, from the Materials tab, the planner and the right-click menu: the plants, ores, enemies, ley lines and domains where it comes from
- Weekly bosses, old one-time domains and artifact domains, with their rewards shown on click
- Resin bosses are marked as such instead of showing a respawn time
- **Markers over the game's own big map**: when the big map of the game is open, the chosen markers are drawn over it where they belong, follow it as it is moved and zoomed, and are shown only once the map is recognized
- **Enemies**: the game sends the enemies that are alive near you, so the list of those of the chosen kinds is shown at the right of the game with the distance, the height difference and an arrow to walk to them (the list can be dragged); when the last enemy of a camp is killed, its marker is marked done for a day

**Route planner**

- Choose materials or kinds of markers to farm, optionally counting teleport waypoints, and generate a route over all of them
- The route is split into numbered stages (teleport, collect, then the next stage), drawn as colored lines with arrows that stay readable at any zoom, with the jumps between stages shown dashed
- Step through the route with Start, Next step and Back; the map follows the current stage
- Nearby points are collected together before the route moves on, and the floors of underground places are taken into account
- Add a material to the route from its right-click menu; items that cannot be farmed in the open world are not offered

**Events timeline**

- Current and upcoming events from the game's official announcements, game versions, and periodic resets such as the Spiral Abyss and the Imaginarium Theater

## Requirements

- Windows 10 (version 1809 or newer) or Windows 11, 64-bit
- Administrator rights. Irminsul asks for them on start; they are needed to observe the game's traffic
- [Microsoft Visual C++ Redistributable (x64)](https://aka.ms/vs/17/release/vc_redist.x64.exe). The program needs it, and most systems already have it. [Official page](https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist)
- Internet access. Map tiles, icons, marker data and live game data are downloaded and cached
- Optional: [Npcap](https://npcap.com/#download), only if you run the program with `--capture-backend pcap`

The program is a single `irminsul.exe`: all other data is built in, and the profiles and cache are stored in `%APPDATA%\Irminsul`.

## Getting started

1. Download `irminsul.exe` from the [latest release](https://github.com/Neiroromo/Aaru/releases/latest).
2. Run it and accept the administrator prompt.
3. Start the packet capture, then start the game and enter the door (or use the built-in game launcher). Irminsul has to be running before the game reaches the login handshake.
4. When the checkmarks appear, the tabs fill with your data.

## Command line options

- `--capture-backend <pktmon|pcap>` (`-b`): `pktmon` is the default and needs no driver; `pcap` needs Npcap and a build with the `pcap` feature
- `--no-admin`: skip the automatic elevation prompt

## Building

```
cargo build --release
```

The build needs a network connection, because it fetches some game data.

## License

MIT License, as in the original project. See `LICENSE` and `THIRD_PARTY_NOTICES.md`.
