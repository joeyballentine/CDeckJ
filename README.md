# Platter

A minimal two-deck DJ player in Rust, built to feel like a pair of CDJs and a two-channel mixer. Designed for the Steam Deck's 1280×800 touchscreen and an AlphaTheta (Pioneer DJ) DDJ-GRV6 controller. Works with touch and keyboard alone.

## Features

- CDJ transport: PLAY, CUE (set while paused, return-and-stop while playing, hold to preview, hold CUE + PLAY to keep playing), SHIFT+CUE to track start, auto-cue to first sound.
- 8 hot cues per deck (press to set or jump, hold while paused to preview).
- Vinyl-mode jog: touch the top to stop and scratch, side to pitch-bend, SHIFT+jog to search. CDJ mode (tap the jog centre) makes the top bend instead.
- Tempo fader with ±6 / ±10 / ±16 % / WIDE ranges, BPM detection with a beat grid, SYNC (tempo match and beat-phase align).
- Loops: IN / OUT / RELOOP-EXIT, 4-beat auto loop, halve/double, beat-loop and beat-jump pads.
- Mixer: trim, 3-band isolator EQ with full kill, colour filter (LPF ↔ HPF), channel faders, crossfader, headphone CUE with cue/master mix.
- Headphone cueing on the controller's 4-channel sound card (master 1/2, phones 3/4), or split-cue on a stereo output.
- Master Tempo (key lock) per deck.
- Reads rekordbox USB sticks: browse playlists and the full collection, and load tracks with their rekordbox beat grid, hot cues (with colours, comments and saved loops) and memory cues.
- Decodes MP3, FLAC, WAV, AIFF, OGG/Vorbis and AAC/ALAC in M4A.

## Run on the Steam Deck

1. Switch to Desktop Mode and copy `platter` to e.g. `~/Applications/platter`, then `chmod +x` it.
2. Put music in `~/Music` (subfolders are fine). Name files `Artist - Title.ext` for nicer display.
3. Plug in the DDJ-GRV6 (a powered USB-C hub helps) and run `./platter`.
4. To use it from Game Mode, add it in Steam with *Add a Non-Steam Game*. Use launch options for any flags.

The prebuilt binary needs only glibc 2.35+ and `libasound`, both present on SteamOS.

### rekordbox USB sticks

Plug in a stick exported from rekordbox. Platter watches `/run/media/$USER` (where SteamOS mounts drives in Desktop Mode) and `/media`, so it shows up under **Sources** within a couple of seconds. Use `--usb <folder>` for a drive mounted elsewhere, or for a copy of an export on disk.

What's read from the export:

| From | Used for |
|---|---|
| `PIONEER/rekordbox/export.pdb` | Tracks, artists, keys, BPM, durations, playlists and folders, history |
| `PIONEER/USBANLZ/…/ANLZ0000.DAT` | Beat grid (including variable-tempo grids and bar positions), hot cues A–C, memory cues |
| `PIONEER/USBANLZ/…/ANLZ0000.EXT` | Hot cues A–H with colours and comments, saved loops (preferred over `.DAT`) |

On load, hot cues go on the pads, the first memory cue becomes the CUE point, and the rekordbox grid replaces Platter's own BPM detection. Plain-folder tracks still get detected grids.

The stick must contain the classic **Device Library** (`export.pdb`). Most exports include it; a stick exported only in the newer Device Library Plus / OneLibrary format (`exportLibrary.db`, an encrypted SQLite database) isn't readable yet. Platter never writes to the stick.

### Audio routing

Platter talks to PipeWire through its PulseAudio interface and picks the controller's sound card automatically. For headphone cueing, the card must expose 4 channels: in *System Settings → Audio* (or `pavucontrol`), set the DDJ-GRV6 profile to **Analog Surround 4.0 Output**. The top bar shows `phones on 3/4` when this works.

No controller sound card? Use `--split-cue` with a stereo output and a splitter cable: master plays in the left ear, cue in the right.

## Controls

### DDJ-GRV6

| Control | Action |
|---|---|
| PLAY / CUE | CDJ transport |
| SHIFT + CUE | Back to start |
| SYNC / SHIFT + SYNC | Sync on/off / cycle tempo range |
| Jog top / side / SHIFT + jog | Scratch / bend / search |
| Tempo fader | Tempo (bottom = faster) |
| LOOP IN / OUT / RELOOP | Loop in, loop out, reloop or exit |
| SHIFT + IN / OUT / RELOOP | 4-beat loop / halve / double |
| Pads: HOT CUE | Set or jump; SHIFT + pad deletes |
| Pads: BEAT LOOP / BEAT JUMP | Loop ¼–32 beats / jump ±1–8 beats |
| Mixer ch 1–2 | Trim, EQ, CFX (filter), fader, CUE |
| Crossfader, headphone MIX | As labelled |
| BROWSE rotate / press | Move in the library / open a folder or playlist |
| SHIFT + BROWSE press | Back up one level |
| LOAD 1·2 | Load the selected track |

Deck 3/4 layers control decks 1/2. Mixer channels 3/4 are ignored.

### Keyboard

| Deck 1 | Deck 2 | Action |
|---|---|---|
| Z | M | Play |
| X (hold) | N (hold) | Cue |
| A | K | Sync |
| 1–4 | 7–0 | Hot cues A–D |
| S | J | Master Tempo |
| Q | P | Load selected track |

↑ ↓ / Page Up / Page Down move; Enter or → opens; Backspace or ← goes back; F11 toggles fullscreen.

### Touch

Everything on screen is touchable. Drag knobs up/down (double-tap to centre); drag faders; tap the track overview to seek; drag a scrolling waveform or the on-screen jog to scratch; tap SHIFT then a pad to delete a hot cue; tap the ±10 chip to change tempo range; tap a folder or playlist to open it, ‹ BACK to go up, and double-tap a track to load it on the deck that isn't playing.

## Options

```
platter [OPTIONS] [TRACK1] [TRACK2]
  --music <DIR>      music folder (default ~/Music)
  --usb <DIR>        extra place to look for a rekordbox export (repeatable)
  --audio <NAME>     output device by name/id substring
  --alsa             bypass PipeWire and use ALSA directly
  --buffer <FRAMES>  audio buffer size (default 256)
  --split-cue        master left / cue right on stereo outputs
  --midi <NAME>      MIDI port by name substring
  --windowed         don't start fullscreen
  --list             list audio devices and MIDI ports
  --midi-monitor     print incoming MIDI and how it is mapped
```

## About the MIDI mapping

The mapping in `src/midi.rs` follows the DDJ-400/FLX layout that the DDJ-GRV6 shares, cross-checked against a community Mixxx mapping for the GRV6. It has not been verified on a physical unit. If a control does nothing or behaves oddly, run `platter --midi-monitor`, move the control, and compare the bytes with the constants at the top of `src/midi.rs`. If the tempo fader runs backwards, set `INVERT_TEMPO_FADER = true`.

## Build from source

Requires Rust 1.95+ and ALSA headers.

```sh
# Debian/Ubuntu: sudo apt install libasound2-dev
cargo build --release
cargo test --release
```

On SteamOS itself, build inside a `distrobox` container (Arch or Ubuntu) rather than unlocking the read-only root filesystem.

## Not included

More than two decks, beat FX, sampler, recording, artwork, ID3 tag reading for plain files, saving cues back to the stick, and rekordbox's OneLibrary format. Detected grids (non-rekordbox tracks) assume a constant tempo.

## Licences

Code: MIT. Test fixtures in `tests/fixtures/rekordbox` come from the rekordcrate project (MPL-2.0). Fonts in `assets/fonts` (Barlow Condensed, JetBrains Mono): SIL Open Font License 1.1, see the `OFL-*.txt` files.
