//! Audio output via cpal. Prefers PipeWire's PulseAudio interface (SteamOS), falls back to ALSA.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, FromSample, SampleFormat, SizedSample, StreamConfig};

use crate::engine::{CueRouting, Engine};

pub struct AudioOptions {
    /// Case-insensitive substring of the device name or id.
    pub device: Option<String>,
    pub force_alsa: bool,
    pub buffer_frames: u32,
    pub split_cue: bool,
}

pub struct AudioOut {
    _stream: cpal::Stream,
    pub description: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub routing: CueRouting,
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

pub fn start(
    opts: &AudioOptions,
    make_engine: impl Fn(u32, u16, CueRouting) -> Engine,
) -> Result<AudioOut, String> {
    let mut last_err = String::from("no audio output device found");
    for host in hosts(opts.force_alsa) {
        let Some(device) = pick_device(&host, opts.device.as_deref()) else {
            continue;
        };
        let (_, desc) = describe(&device);
        let def = match device.default_output_config() {
            Ok(c) => c,
            Err(e) => {
                last_err = format!("{desc}: {e}");
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
        for buffer in [BufferSize::Fixed(opts.buffer_frames), BufferSize::Default] {
            let config = StreamConfig { channels, sample_rate, buffer_size: buffer };
            let engine = make_engine(sample_rate, channels, routing);
            let res = match def.sample_format() {
                SampleFormat::F32 => build::<f32>(&device, &config, engine),
                SampleFormat::I16 => build::<i16>(&device, &config, engine),
                SampleFormat::I32 => build::<i32>(&device, &config, engine),
                SampleFormat::U16 => build::<u16>(&device, &config, engine),
                f => Err(format!("unsupported sample format {f:?}")),
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
                Err(e) => last_err = format!("{desc}: {e}"),
            }
        }
    }
    Err(last_err)
}

fn build<T>(device: &cpal::Device, config: &StreamConfig, mut engine: Engine) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let mut scratch = vec![0f32; 16384 * config.channels as usize];
    let stream = device
        .build_output_stream(
            config.clone(),
            move |data: &mut [T], _| {
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
            |e| eprintln!("audio: {e}"),
            None,
        )
        .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(stream)
}
