# Platter

A minimal two-deck DJ player in Rust, built to feel like a pair of CDJs and a two-channel mixer. Designed for the Steam Deck's 1280×800 touchscreen and an AlphaTheta (Pioneer DJ) DDJ-GRV6 controller. Works with touch and keyboard alone.

## Features

- CDJ transport: PLAY, CUE (set while paused, return-and-stop while playing, hold to preview, hold CUE + PLAY to keep playing), SHIFT+CUE to track start, auto-cue to first sound.
- Tracks are playable about a second after loading; the rest decodes while you listen.
- 8 hot cues and 32 memory cues per deck, with CALL ‹ › to step through memory cues. Cues you set are saved and come back next time.
- Quantize (on by default): cues, loops and beat jumps snap to the grid, and jumps keep the beat phase.
- Slip mode (the track carries on underneath scratches, loops and reverse; you drop back in where it would have been) and reverse play.
- Vinyl-mode jog: touch the top to stop and scratch, side to pitch-bend, SHIFT+jog to search. CDJ mode (tap the jog centre) makes the top bend instead.
- Tempo fader with ±6 / ±10 / ±16 % / WIDE ranges, BPM detection with a beat grid, SYNC (tempo match plus a gentle phase lock that keeps beats aligned). Turning SYNC off holds the tempo until the fader is moved past it, so nothing jumps.
- Fix a half- or double-tempo detection with SHIFT → BPM ÷2 / ×2; the fix is remembered.
- Loops: IN / OUT / RELOOP-EXIT, 4-beat auto loop, halve/double, beat-loop and beat-jump pads.
- Mixer: trim, 3-band isolator EQ with full kill, colour filter (LPF ↔ HPF), channel faders, crossfader, headphone CUE with cue/master mix.
- Headphone cueing on the controller's 4-channel sound card (master 1/2, phones 3/4), or split-cue on a stereo output.
- Master Tempo (key lock) per deck. Both decks with key lock use about 2 % of one CPU core.
- Soft takeover: after you move something on screen, the controller's knob or fader takes over only once it reaches the on-screen value.
- Multi-touch: hold CUE with one finger and hit PLAY with another, or ride two faders at once.
- Steam Deck controls work as a mini controller (see below), and the screen won't dim or sleep while Platter runs.
- Keeps running without an audio device (silent), and reconnects when one appears.
- Reads rekordbox USB sticks: browse playlists, history and the full collection, and load tracks with their rekordbox beat grid, hot cues (with colours, comments and saved loops), memory cues, cover art and waveforms. Plugging the stick in applies its **My Settings**, like a CDJ.
- Waveforms in Blue, RGB or 3-Band, whole-track from the moment a track loads (rekordbox's own for stick tracks; plain files are analysed in the background and cached).
- Beat FX: delay, echo, ping pong, reverb, flanger, phaser, trans and roll, beat-synced, on channel 1, 2 or the master, with echo and reverb tails.
- Play history: tracks you've played are ticked and greyed in the browser, and every session is saved under **Sources → History**.
- Records the mix to 24-bit WAV with a tracklist and CUE sheet (see *Recording*).
- Library search (`/`), sortable columns (title, artist, BPM, key, time), tags read from the files, and BPM analysis of your music folder in the background.
- Decodes MP3, FLAC, WAV, AIFF, OGG/Vorbis and AAC/ALAC in M4A. MP3 timing matches rekordbox, so rekordbox grids and cues line up.

## Accessibility

Every control has a screen-reader name (AccessKit), can be reached with Tab, and works from the keyboard. Text meets WCAG 4.5:1 contrast on its background. On a screen that's too small for the layout at your scaling (for example 1280 × 800 at 150 %), everything shrinks to fit instead of being cut off.

## Run on the Steam Deck

1. Switch to Desktop Mode and copy `platter` to e.g. `~/Applications/platter`, then `chmod +x` it.
2. Put music in `~/Music` (subfolders are fine). Name files `Artist - Title.ext` for nicer display.
3. Plug in the DDJ-GRV6 (a powered USB-C hub helps) and run `./platter`.
4. To use it from Game Mode, add it in Steam with *Add a Non-Steam Game*. Use launch options for any flags.

The prebuilt binary needs only glibc 2.35+, `libasound` and `libudev`, all present on SteamOS.

### Quitting

Press **EXIT** at the top right twice (or Ctrl+Q twice); the second press has to come within 3 seconds, so a stray tap can't end a set. In Game Mode the Steam button → *Exit game* works too. Quitting stops playback, saves settings and cues, and finishes any recording properly.

### Start automatically when the controller is plugged in

```sh
./platter --install-autolaunch
```

From then on, plugging in the DDJ-GRV6 opens Platter. Nothing else changes, and no root or password is needed: this installs a small background service for your user (`~/.config/systemd/user/platter-watch.service`) that checks for the controller every two seconds and uses next to no CPU.

- If Platter has been added to Steam (*Add a Non-Steam Game*), it opens through Steam, so it works in **Game Mode** as well as Desktop Mode. Without that, it only opens in Desktop Mode.
- It opens when the controller *arrives*. If you quit Platter with the controller still plugged in, it won't reopen until you unplug and replug it (or log in again).
- It looks for the same names as the MIDI connection (`port_hints` in the `[midi]` settings: GRV6, DDJ, AlphaTheta, Pioneer), so other Pioneer DJ gear triggers it too. Narrow the list to `["GRV6"]` if that's not what you want.
- If you move the `platter` file, run `--install-autolaunch` again. `./platter --uninstall-autolaunch` turns it off.

### rekordbox USB sticks

Plug in a stick exported from rekordbox. Platter watches `/run/media/$USER` (where SteamOS mounts drives in Desktop Mode) and `/media`, so it shows up under **Sources** within a couple of seconds. Use `--usb <folder>` for a drive mounted elsewhere, or for a copy of an export on disk.

What's read from the export:

| From | Used for |
|---|---|
| `PIONEER/rekordbox/export.pdb` | Tracks, artists, keys, BPM, durations, playlists and folders, history |
| `PIONEER/USBANLZ/…/ANLZ0000.DAT` | Beat grid (including variable-tempo grids and bar positions), hot cues A–C, memory cues |
| `PIONEER/USBANLZ/…/ANLZ0000.EXT` | Hot cues A–H with colours and comments, saved loops (preferred over `.DAT`), Blue and RGB waveforms |
| `PIONEER/USBANLZ/…/ANLZ0000.2EX` | 3-Band waveform (CDJ-3000 era) |
| `PIONEER/Artwork/…` | Cover art (the larger `_m` version when present) |
| `PIONEER/DEVSETTING.DAT`, `MYSETTING.DAT`, `MYSETTING2.DAT`, `DJMMYSETTING.DAT` | My Settings (below) |

On load, hot cues go on the pads, the first memory cue becomes the CUE point, and the rekordbox grid replaces Platter's own BPM detection. Plain-folder tracks still get detected grids.

#### My Settings

When a stick is plugged in, Platter takes on the settings you made in rekordbox's *My Settings* (a banner lists them). They stay until another stick changes them, and you can still change any of them on screen:

| Setting | Effect in Platter |
|---|---|
| Waveform colour (Blue / RGB / 3Band) | Colour of the scrolling and overview waveforms |
| Overview waveform type (half / full) | Overview drawn from the bottom or mirrored |
| Waveform current position (centre / left) | Playhead centred or at 30 % |
| Quantize, quantize beat value | Quantize on/off; snapping to 1, ½, ¼ or ⅛ beat |
| Auto cue, auto cue level | CUE starts at the first memory cue, or at the first sound above the level, or at 0:00 |
| Hot cue auto load | Whether the stick's hot cues go on the pads |
| Time mode | Big readout shows remaining or elapsed time |
| Jog mode | Vinyl or CDJ |
| Master Tempo, tempo range | Both decks |
| Beat jump beat value | The on-screen « » jump size (SHIFT: ×4) |
| Channel fader curve, crossfader curve | Mixer curves (DJM settings) |
| Beat FX quantize | Roll starts and Trans gates on the beat grid |

Settings Platter has no equivalent for (LCD brightness, language, eject lock and so on) are ignored. Turn the whole thing off with `apply_usb_settings = false`.

The stick must contain the classic **Device Library** (`export.pdb`). Most exports include it; a stick exported only in the newer Device Library Plus / OneLibrary format (`exportLibrary.db`, an encrypted SQLite database) isn't readable yet. Platter never writes to the stick.

### Audio routing

Platter talks to PipeWire through its PulseAudio interface and picks the controller's sound card automatically. For headphone cueing, the card must expose 4 channels: in *System Settings → Audio* (or `pavucontrol`), set the DDJ-GRV6 profile to **Analog Surround 4.0 Output**. The top bar shows `phones on 3/4` when this works.

No controller sound card? Use `--split-cue` with a stereo output and a splitter cable: master plays in the left ear, cue in the right.

## Recording

Press **REC** in the top bar (or R, or ≡ on the Steam Deck) to start. The button shows the elapsed time. To stop, press it twice within 3 seconds, so a stray tap can't end a set.

- Files go to `~/Music/Platter Recordings/` (change with `recording_dir` in the settings file), named like `2026-10-18 21-30 Platter mix.wav`.
- The recording is the master mix **before the MASTER knob**, like a DJM's REC OUT: turning the room down doesn't make the recording quieter. It keeps going if the audio device drops out.
- 24-bit stereo WAV, about 1 GB per hour at 48 kHz. Past about 3¾ hours (or if the sample rate changes mid-set) it carries on in `… part 2.wav`.
- The file is kept playable while recording, so a crash or power cut loses at most the last second.
- Next to it: `… tracklist.txt` (`00:00 Artist - Title`, ready to paste into a mix description) and a `.cue` sheet with track markers. A track goes on the list once it has been audible (playing, fader and crossfader open) for 5 seconds, timed from when it came in.
- Platter warns when less than 1 GB is free and stops cleanly before the disk fills.
- The library skips the recordings folder.

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
| SHIFT + PLAY | Reverse (while held) |
| Pads: HOT CUE | Set or jump; SHIFT + pad deletes |
| Pads: BEAT LOOP / BEAT JUMP | Loop ¼–32 beats / jump ±1–8 beats |
| Mixer ch 1–2 | Trim, EQ, CFX (filter), fader, CUE |
| Crossfader, headphone MIX | As labelled |
| BROWSE rotate / press | Move in the library / open a folder or playlist |
| SHIFT + BROWSE press | Back up one level |
| LOAD 1·2 | Load the selected track |
| BEAT FX SELECT / BEAT ◀ ▶ | Next effect / beat value |
| FX CH SELECT lever | Effect on channel 1, 2 or master |
| LEVEL/DEPTH, BEAT FX ON | As labelled (ON blinks while active) |

Deck 3/4 layers control decks 1/2. Mixer channels 3/4 are ignored.

### Steam Deck controls

Use Steam's *Gamepad* controller layout for Platter.

| Control | Action |
|---|---|
| L1 / R1 | PLAY deck 1 / 2 |
| L2 / R2 (hold) | CUE deck 1 / 2 |
| View + L1 / R1 | SYNC deck 1 / 2 |
| D-pad ↑ ↓ | Move in the library (hold to repeat) |
| D-pad → or A | Open folder or playlist |
| D-pad ← or Y | Back |
| X / B | Load deck 1 / 2 |
| Left / right stick ← → | Nudge deck 1 / 2 |
| View + left stick ← → | Crossfader |
| ≡ (Menu) | Record; press twice to stop |

Set `gamepad = false` in the settings file to turn this off.

### Keyboard

| Deck 1 | Deck 2 | Action |
|---|---|---|
| Z | M | Play |
| X (hold) | N (hold) | Cue |
| A | K | Sync |
| 1–4 | 7–0 | Hot cues A–D |
| S | J | Master Tempo |
| Q | P | Load selected track |

Ctrl+Q (twice) quits; G toggles quantize; F turns beat FX on/off; W changes the waveform colour; R records (press twice to stop); − / = zoom the waveforms out / in; `/` or Ctrl+F searches; ↑ ↓ / Page Up / Page Down move; Enter or → opens; Backspace, Esc or ← goes back; F11 toggles fullscreen.

Tab moves keyboard focus through every on-screen control (shown with an orange ring). Enter or Space presses the focused button. ← → turn a focused knob, jump a beat on a focused waveform, 16 beats on the overview, and search on the jog. Hover a button for its shortcut.

### Touch

Everything on screen is touchable. Drag knobs up/down (double-tap to centre); drag faders; tap the track overview to seek; drag a scrolling waveform or the on-screen jog to scratch; tap SHIFT then a pad to delete a hot cue; tap the ±10 chip to change tempo range; tap the time to switch remaining/elapsed; tap − / + over the waveforms to zoom and BLUE / RGB / 3BAND to change their colour; use the BEAT FX bar under the decks (effect, beat ‹ ›, channel 1 / 2 / M, LEVEL, ON); tap a folder or playlist to open it, ‹ BACK to go up, and double-tap a track to load it on the deck that isn't playing.

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
  --install-autolaunch / --uninstall-autolaunch
                     start Platter whenever the controller is plugged in
```

Command-line options override the settings file for that run.

## Settings and saved data

`~/.config/platter/config.toml` is created when you change something. It remembers quantize, Master Tempo, vinyl mode, tempo ranges, levels and waveform zoom, and also holds options you can only set by editing it:

```toml
music_dir = "/home/deck/Music"
usb_paths = []                # extra places to look for rekordbox exports
audio_device = "GRV6"         # name substring, like --audio
buffer_frames = 256
split_cue = false
bpm_range = [70.0, 180.0]     # detected tempos are folded into this range, e.g. [88.0, 176.0] for drum & bass
background_analysis = true    # detect BPM of music-folder tracks while idle
gamepad = true                # use the Steam Deck's own buttons
keep_awake = true             # stop the screen dimming and the Deck sleeping
recording_dir = "/home/deck/Music/Platter Recordings"
apply_usb_settings = true     # take on a stick's My Settings when it's plugged in

# My Settings (a stick's settings overwrite these):
waveform_color = "Rgb"        # "Blue", "Rgb" or "ThreeBand"
overview_full = true
playhead_left = false
time_remain = true
beat_jump = 4.0
quantize_beats = 1.0
auto_cue = true
# auto_cue_db = -60.0         # unset: cue to the first memory cue
hotcue_autoload = true
fader_curve = "Linear"        # "SteepTop", "Linear", "SteepBottom"
crossfader_curve = "SlowCut"  # "ConstantPower", "SlowCut", "FastCut"
beat_fx_quantize = true

[grid_offset_ms]              # shift rekordbox grids and cues per file type, if they're off
# mp3 = 0.0

[midi]                        # override any note/CC number; see `platter --midi-monitor`
invert_tempo = false
jog_ticks_per_rev = 720.0
port_hints = ["GRV6", "DDJ", "AlphaTheta", "Pioneer"]
```

If the file can't be read, Platter starts with defaults and keeps the broken copy as `config.toml.broken`.

`~/.local/share/platter/tracks.json` stores, per file: cues and the CUE point you set, BPM fixes, tags and analysis. An entry is ignored if the file's size or date changes. Cues are saved straight away.

`~/.local/share/platter/waves/` caches waveforms of plain files (about 1 MB per hour of audio). `~/.local/share/platter/history/` holds one JSON file per session.

## Beat FX

The bar under the decks works like a DJM's BEAT FX section. The effect follows the tempo of the deck that's leading (the sync master if it's playing, otherwise a playing deck), and is applied after the channel fader, so an echo or reverb keeps ringing when you pull the fader down or switch the effect off.

| Effect | Beat value sets | LEVEL sets |
|---|---|---|
| DELAY | Delay time (one repeat) | Repeat level |
| ECHO | Echo time (repeats fading out) | Echo level |
| PING PONG | Echo time, bouncing left and right | Echo level |
| REVERB | Room size (shown as %) | Reverb level |
| FLANGER, PHASER | Length of one sweep | Depth |
| TRANS | Gate length (cuts on the beat grid) | How far it cuts |
| ROLL | Length of the repeated slice | Roll level |

Beat values run from 1/8 to 16 beats. Effects are part of the recording.

## About the MIDI mapping

The mapping follows the DDJ-400/FLX layout that the DDJ-GRV6 shares, cross-checked against a community Mixxx mapping for the GRV6. It has not been verified on a physical unit. If a control does nothing or behaves oddly, run `platter --midi-monitor`, move the control, and note the bytes. Then set the matching number in the `[midi]` section of the settings file (the names are listed in `MidiMap` in `src/midi.rs`). If the tempo fader runs backwards, set `invert_tempo = true`.

## Build from source

Requires Rust 1.95+ and ALSA and udev headers.

```sh
# Debian/Ubuntu: sudo apt install libasound2-dev libudev-dev
cargo build --release
cargo test --release
```

The release binary is built against glibc 2.35 so it runs on older SteamOS too:

```sh
pip install ziglang cargo-zigbuild
RUSTFLAGS="-C link-arg=-Wl,--allow-shlib-undefined" cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.35
```

On SteamOS itself, build inside a `distrobox` container (Arch or Ubuntu) rather than unlocking the read-only root filesystem.

## Not included

More than two decks, sampler, mic input, building playlists in Platter, key detection for plain files, saving cues back to the stick, and rekordbox's OneLibrary format. Detected grids (non-rekordbox tracks) assume a constant tempo.

Not yet tested on hardware: the DDJ-GRV6 mapping (including the BEAT FX section), the Steam Deck's own audio, touchscreen and controls, and the screen-sleep inhibit. Each was built against documentation and tested in software only.

## Licences

Code: MIT. Test fixtures in `tests/fixtures/rekordbox` come from the rekordcrate project (MPL-2.0). Fonts in `assets/fonts` (Barlow Condensed, JetBrains Mono): SIL Open Font License 1.1, see the `OFL-*.txt` files.
