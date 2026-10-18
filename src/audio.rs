//! Audio output via cpal. Prefers PipeWire's PulseAudio interface (SteamOS), falls back to ALSA.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, ErrorKind, FromSample, SampleFormat, SizedSample, StreamConfig};

use crate::engine::{CueRouting, SharedEngine};

/// Buffer underruns/overruns reported by the audio backend since start.
pub static XRUNS: AtomicU32 = AtomicU32::new(0);
/// Set when the stream died (device unplugged, server restarted): the UI reconnects.
pub static STREAM_LOST: AtomicBool = AtomicBool::new(false);

#[derive(Clone)]
pub struct AudioOptions {
    /// Case-insensitive substring of the device name or id.
    pub device: Option<String>,
    pub force_alsa: bool,
    pub buffer_frames: u32,
    pub split_cue: bool,
    /// Switch a matching card (controller) to its 4-channel profile first, for the cue.
    pub cue_profile_hints: Option<Vec<String>>,
}

pub struct AudioOut {
    _stream: cpal::Stream,
    pub description: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub routing: CueRouting,
}

/// Runs the engine against a silent clock while no audio device is available, so decks
/// still load, play and show time (and MIDI LEDs keep working). Stops when dropped.
pub struct SilentClock {
    run: std::sync::Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SilentClock {
    pub fn start(engine: &SharedEngine) -> SilentClock {
        const RATE: u32 = 48000;
        const BLOCK: usize = 480;
        engine.lock().set_output(RATE, 2, CueRouting::MasterOnly);
        let run = std::sync::Arc::new(AtomicBool::new(true));
        let r = run.clone();
        let engine = engine.clone();
        let thread = std::thread::Builder::new()
            .name("silent-clock".into())
            .spawn(move || {
                let mut buf = vec![0f32; BLOCK * 2];
                let start = std::time::Instant::now();
                let mut done: u64 = 0;
                while r.load(Ordering::Relaxed) {
                    let due = (start.elapsed().as_secs_f64() * RATE as f64) as u64;
                    // Never try to catch up more than 100 ms after a stall.
                    if due > done + RATE as u64 / 10 {
                        done = due - RATE as u64 / 10;
                    }
                    while done + BLOCK as u64 <= due {
                        engine.lock().process(&mut buf);
                        done += BLOCK as u64;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            })
            .ok();
        SilentClock { run, thread }
    }
}

impl Drop for SilentClock {
    fn drop(&mut self) {
        self.run.store(false, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Names that identify the controller's built-in sound card.
const CONTROLLER_HINTS: &[&str] = &["grv6", "ddj", "alphatheta", "pioneer"];

fn hosts(force_alsa: bool) -> Vec<cpal::Host> {
    let mut v = Vec::new();
    if !force_alsa {
        if let Ok(h) = cpal::host_from_id(cpal::HostId::PulseAudio) {
            v.push(h);
        }
    }
    v.push(cpal::default_host());
    v
}

fn describe(d: &cpal::Device) -> (String, String) {
    let id = d.id().map(|i| i.to_string()).unwrap_or_default();
    let desc = d.description().map(|x| x.to_string()).unwrap_or_else(|_| id.clone());
    (id, desc)
}

fn output_devices(host: &cpal::Host) -> Vec<cpal::Device> {
    host.devices()
        .map(|it| it.filter(|d| d.default_output_config().is_ok()).collect())
        .unwrap_or_default()
}

/// Output devices as (host, description, id) for the SETTINGS view. Probes hardware, so
/// call it off the UI thread.
pub fn output_device_list(force_alsa: bool) -> Vec<(String, String, String)> {
    let mut v = Vec::new();
    for host in hosts(force_alsa) {
        for d in output_devices(&host) {
            let (id, desc) = describe(&d);
            v.push((host.id().name().to_string(), desc, id));
        }
    }
    v
}

pub fn list_devices(force_alsa: bool) {
    for host in hosts(force_alsa) {
        println!("{}:", host.id().name());
        for d in output_devices(&host) {
            let (id, desc) = describe(&d);
            let cfg = d.default_output_config().ok();
            println!(
                "  {desc}\n      id: {id}\n      default: {} ch @ {} Hz",
                cfg.as_ref().map(|c| c.channels()).unwrap_or(0),
                cfg.as_ref().map(|c| c.sample_rate()).unwrap_or(0),
            );
        }
    }
}

fn pick_device(host: &cpal::Host, wanted: Option<&str>) -> Option<cpal::Device> {
    let devices = output_devices(host);
    let matches = |d: &cpal::Device, needle: &str| {
        let (id, desc) = describe(d);
        let n = needle.to_lowercase();
        id.to_lowercase().contains(&n) || desc.to_lowercase().contains(&n)
    };
    if let Some(w) = wanted {
        return devices.into_iter().find(|d| matches(d, w));
    }
    // Prefer the controller's sound card, choosing the variant with the most channels.
    let mut best: Option<(u16, cpal::Device)> = None;
    for d in devices {
        if CONTROLLER_HINTS.iter().any(|h| matches(&d, h)) {
            let ch = d.default_output_config().map(|c| c.channels()).unwrap_or(0);
            if best.as_ref().map(|(c, _)| ch > *c).unwrap_or(true) {
                best = Some((ch, d));
            }
        }
    }
    best.map(|(_, d)| d).or_else(|| host.default_output_device())
}

pub fn start(opts: &AudioOptions, engine: &SharedEngine) -> Result<AudioOut, String> {
    STREAM_LOST.store(false, Ordering::Relaxed);
    if let (Some(h), false) = (&opts.cue_profile_hints, opts.force_alsa) {
        crate::cardprofile::prepare(h);
    }
    // One error per host, so a PipeWire failure isn't hidden behind the ALSA fallback's.
    let mut errors: Vec<String> = Vec::new();
    for host in hosts(opts.force_alsa) {
        let hname = host.id().name();
        let Some(device) = pick_device(&host, opts.device.as_deref()) else {
            errors.push(format!("{hname}: no output device"));
            continue;
        };
        let mut last_err = String::new();
        let (_, desc) = describe(&device);
        let def = match device.default_output_config() {
            Ok(c) => c,
            Err(e) => {
                errors.push(format!("{hname} · {desc}: {e}"));
                continue;
            }
        };
        let channels: u16 = if def.channels() >= 4 { def.channels() } else { def.channels().max(1) };
        let routing = if channels >= 4 {
            CueRouting::Channels34
        } else if opts.split_cue && channels >= 2 {
            CueRouting::SplitMono
        } else {
            CueRouting::MasterOnly
        };
        let sample_rate = def.sample_rate();
        let supported: Vec<(u16, SampleFormat, u32, u32)> = device
            .supported_output_configs()
            .map(|it| it.map(|c| (c.channels(), c.sample_format(), c.min_sample_rate(), c.max_sample_rate())).collect())
            .unwrap_or_default();
        let format = pick_format(&supported, channels, sample_rate, def.sample_format(), host.id() == cpal::HostId::PulseAudio);
        for buffer in [BufferSize::Fixed(opts.buffer_frames), BufferSize::Default] {
            let config = StreamConfig { channels, sample_rate, buffer_size: buffer };
            engine.lock().set_output(sample_rate, channels, routing);
            let e = engine.clone();
            let res = match format {
                SampleFormat::F32 => build::<f32>(&device, &config, e),
                SampleFormat::F64 => build::<f64>(&device, &config, e),
                SampleFormat::I32 => build::<i32>(&device, &config, e),
                SampleFormat::I24 => build::<cpal::I24>(&device, &config, e),
                SampleFormat::I16 => build::<i16>(&device, &config, e),
                SampleFormat::I8 => build::<i8>(&device, &config, e),
                SampleFormat::I64 => build::<i64>(&device, &config, e),
                SampleFormat::U32 => build::<u32>(&device, &config, e),
                SampleFormat::U24 => build::<cpal::U24>(&device, &config, e),
                SampleFormat::U16 => build::<u16>(&device, &config, e),
                SampleFormat::U8 => build::<u8>(&device, &config, e),
                SampleFormat::U64 => build::<u64>(&device, &config, e),
                f => Err(format!("the device only offers {f:?}, which isn't PCM audio")),
            };
            match res {
                Ok(stream) => {
                    return Ok(AudioOut {
                        _stream: stream,
                        description: format!("{} · {}", host.id().name(), desc),
                        sample_rate,
                        channels,
                        routing,
                    });
                }
                Err(e) => last_err = format!("{hname} · {desc} ({format:?}, {channels} ch, {sample_rate} Hz): {e}"),
            }
        }
        errors.push(last_err);
    }
    Err(if errors.is_empty() { "no audio output device found".into() } else { errors.join(" — ") })
}

/// Formats Platter can write, best first.
const FORMATS: [SampleFormat; 12] = [
    SampleFormat::F32,
    SampleFormat::I32,
    SampleFormat::I24,
    SampleFormat::I16,
    SampleFormat::F64,
    SampleFormat::U32,
    SampleFormat::U24,
    SampleFormat::U16,
    SampleFormat::I64,
    SampleFormat::U64,
    SampleFormat::I8,
    SampleFormat::U8,
];

/// Pick a sample format the device accepts at this channel count and rate. The device's
/// default can be one Platter has no writer for (e.g. 24-bit after switching a card to a
/// surround profile), so look through everything it supports. PulseAudio/PipeWire convert
/// any format, so float is always fine there.
pub fn pick_format(supported: &[(u16, SampleFormat, u32, u32)], channels: u16, rate: u32, default: SampleFormat, pulse: bool) -> SampleFormat {
    if pulse {
        return SampleFormat::F32;
    }
    let ok = |f: SampleFormat| supported.iter().any(|&(c, sf, lo, hi)| c == channels && sf == f && lo <= rate && rate <= hi);
    FORMATS.iter().copied().find(|&f| ok(f)).unwrap_or(if FORMATS.contains(&default) { default } else { SampleFormat::F32 })
}

fn build<T>(device: &cpal::Device, config: &StreamConfig, engine: SharedEngine) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let mut scratch = vec![0f32; 16384 * config.channels as usize];
    let stream = device
        .build_output_stream(
            config.clone(),
            move |data: &mut [T], _| {
                let mut engine = engine.lock();
                if scratch.len() < data.len() {
                    // Should never happen with sane buffer sizes; render in pieces instead.
                    for chunk in data.chunks_mut(scratch.len()) {
                        let s = &mut scratch[..chunk.len()];
                        engine.process(s);
                        for (o, &v) in chunk.iter_mut().zip(s.iter()) {
                            *o = T::from_sample(v);
                        }
                    }
                    return;
                }
                let s = &mut scratch[..data.len()];
                engine.process(s);
                for (o, &v) in data.iter_mut().zip(s.iter()) {
                    *o = T::from_sample(v);
                }
            },
            |e: cpal::Error| match e.kind() {
                ErrorKind::Xrun => {
                    XRUNS.fetch_add(1, Ordering::Relaxed);
                }
                ErrorKind::DeviceChanged | ErrorKind::RealtimeDenied => eprintln!("audio: {e}"),
                _ => {
                    eprintln!("audio: stream lost: {e}");
                    STREAM_LOST.store(true, Ordering::Relaxed);
                }
            },
            None,
        )
        .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_a_format_platter_can_write() {
        use SampleFormat::*;
        // A 4-channel profile that only offers 24-bit at 48 kHz (and float at stereo).
        let sup = [(4, I24, 44100, 48000), (2, F32, 44100, 48000)];
        assert_eq!(pick_format(&sup, 4, 48000, I24, false), I24);
        assert_eq!(pick_format(&sup, 2, 48000, I24, false), F32);
        // Nothing listed: fall back to the default if writable, else float.
        assert_eq!(pick_format(&[], 2, 48000, I16, false), I16);
        assert_eq!(pick_format(&[], 2, 48000, DsdU8, false), F32);
        // PipeWire/PulseAudio converts, so always float there.
        assert_eq!(pick_format(&sup, 4, 48000, I24, true), F32);
    }
}
