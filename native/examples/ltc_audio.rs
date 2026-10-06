//! CPAL belongs only to examples. The shipped libraries have no device backend.
use cpal::{
    FromSample, Sample,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tidkod::{ltc::*, *};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
struct Handoff {
    producer: rtrb::Producer<SourceSample>,
    drops: Arc<AtomicU64>,
    lost: bool,
}
impl Handoff {
    fn push(&mut self, mut observation: SourceSample) {
        observation.discontinuity |= self.lost;
        self.lost = self.producer.push(observation).is_err();
        if self.lost {
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
    }
}
struct Audio {
    config: cpal::StreamConfig,
    channel: usize,
    bridge: triple_buffer::Output<Option<ClockBridge>>,
    errors: Arc<AtomicU64>,
}
fn input_stream<T, F>(
    device: &cpal::Device,
    mut audio: Audio,
    mut process: F,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample,
    f32: FromSample<T>,
    F: FnMut(&[f32], SampleBlock) + Send + 'static,
{
    let config = audio.config;
    let errors = audio.errors.clone();
    let mut index = 0u64;
    let mut scratch = [0.; 1024];
    Ok(device.build_input_stream::<T, _, _>(
        config,
        move |data, info| {
            let count = data.len() / audio.config.channels as usize;
            let start = index;
            index = index.saturating_add(count as u64);
            let Some(bridge) = *audio.bridge.read() else {
                return;
            };
            let Ok(ns) = u64::try_from(info.timestamp().capture.as_nanos()) else {
                return;
            };
            let Ok(at) = bridge.convert(ns) else {
                return;
            };
            let block = SampleBlock {
                first_sample: start,
                first_sample_ns: at.local_ns,
            };
            let mut offset = 0;
            for chunk in data.chunks(1024 * audio.config.channels as usize) {
                let frames = chunk.len() / audio.config.channels as usize;
                for (i, frame) in chunk
                    .chunks_exact(audio.config.channels as usize)
                    .enumerate()
                {
                    scratch[i] = f32::from_sample(frame[audio.channel]);
                }
                if let Some(block) = block.advance(offset, audio.config.sample_rate) {
                    process(&scratch[..frames], block);
                }
                offset += frames;
            }
        },
        move |_| {
            errors.fetch_add(1, Ordering::Relaxed);
        },
        None,
    )?)
}
fn output_stream<T, F>(
    device: &cpal::Device,
    mut audio: Audio,
    mut render: F,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample + FromSample<f32>,
    F: FnMut(&mut [f32], SampleBlock) + Send + 'static,
{
    let config = audio.config;
    let errors = audio.errors.clone();
    let mut index = 0u64;
    let mut scratch = [0.; 1024];
    Ok(device.build_output_stream::<T, _, _>(
        config,
        move |data, info| {
            data.fill(T::from_sample(0.));
            let count = data.len() / audio.config.channels as usize;
            let start = index;
            index = index.saturating_add(count as u64);
            let Some(bridge) = *audio.bridge.read() else {
                return;
            };
            let Ok(ns) = u64::try_from(info.timestamp().playback.as_nanos()) else {
                return;
            };
            let Ok(at) = bridge.convert(ns) else {
                return;
            };
            let block = SampleBlock {
                first_sample: start,
                first_sample_ns: at.local_ns,
            };
            let mut offset = 0;
            for chunk in data.chunks_mut(1024 * audio.config.channels as usize) {
                let frames = chunk.len() / audio.config.channels as usize;
                if let Some(block) = block.advance(offset, audio.config.sample_rate) {
                    render(&mut scratch[..frames], block);
                    for (i, frame) in chunk
                        .chunks_exact_mut(audio.config.channels as usize)
                        .enumerate()
                    {
                        frame[audio.channel] = T::from_sample(scratch[i]);
                    }
                }
                offset += frames;
            }
        },
        move |_| {
            errors.fetch_add(1, Ordering::Relaxed);
        },
        None,
    )?)
}
fn number<T: std::str::FromStr>(args: &BTreeMap<String, String>, key: &str, default: T) -> Result<T>
where
    T::Err: std::error::Error + 'static,
{
    Ok(args
        .get(key)
        .map(|v| v.parse())
        .transpose()?
        .unwrap_or(default))
}
fn refresh(
    stream: &cpal::Stream,
    clock: MonotonicClock,
    bridge: &mut triple_buffer::Input<Option<ClockBridge>>,
) {
    let before = clock.now_ns();
    let external = stream.now().as_nanos();
    let after = clock.now_ns();
    bridge.write(
        u64::try_from(external)
            .ok()
            .and_then(|ns| ClockBridge::new(before, ns, after).ok()),
    );
}
pub fn run(input: bool) -> Result<()> {
    let args = super::common::args();
    if args.contains_key("--help") {
        println!(
            "LTC {}: --list-devices --device INDEX --channel 0 --sample-rate 48000 --fps 30 --drop-frame --seconds 10\nInput: --bind 0.0.0.0:4443 --no-mdns\nOutput: --address HOST:PORT --pin SHA256 --start-frame 0 --amplitude 0.5\nWithout --address, output is a standalone generator. Device timestamps include latency; do not add it twice.",
            if input { "input" } else { "output" }
        );
        return Ok(());
    }
    let host = cpal::default_host();
    let devices: Vec<_> = if input {
        host.input_devices()?.collect()
    } else {
        host.output_devices()?.collect()
    };
    if args.contains_key("--list-devices") {
        for (i, d) in devices.iter().enumerate() {
            println!("{i}: {}", d.description()?.name());
        }
        return Ok(());
    }
    let device = if let Some(index) = args.get("--device") {
        devices
            .get(index.parse::<usize>()?)
            .cloned()
            .ok_or("device index out of range")?
    } else if input {
        host.default_input_device().ok_or("no input device")?
    } else {
        host.default_output_device().ok_or("no output device")?
    };
    let rate = number(&args, "--sample-rate", 48000u32)?;
    let channel = number(&args, "--channel", 0usize)?;
    let config = LtcConfig::new(super::common::format(&args)?, rate)?;
    let ranges: Vec<_> = if input {
        device.supported_input_configs()?.collect()
    } else {
        device.supported_output_configs()?.collect()
    };
    let supported = ranges
        .into_iter()
        .filter(|r| channel < r.channels() as usize)
        .filter_map(|r| r.try_with_sample_rate(rate))
        .find(|r| {
            matches!(
                r.sample_format(),
                cpal::SampleFormat::F32
                    | cpal::SampleFormat::I16
                    | cpal::SampleFormat::I32
                    | cpal::SampleFormat::U16
                    | cpal::SampleFormat::F64
            )
        })
        .ok_or("device does not support requested channel/rate and supported PCM formats")?;
    println!(
        "{}: {:?}, {} Hz, channel {}",
        device.description()?.name(),
        supported.sample_format(),
        rate,
        channel
    );
    let engine = Engine::new()?;
    let clock = engine.clock();
    let errors = Arc::new(AtomicU64::new(0));
    let drops = Arc::new(AtomicU64::new(0));
    let (mut bridge_tx, bridge_rx) = triple_buffer::triple_buffer(&None);
    let audio = Audio {
        config: supported.config(),
        channel,
        bridge: bridge_rx,
        errors: errors.clone(),
    };
    let (producer, mut consumer) = rtrb::RingBuffer::<SourceSample>::new(64);
    let leader = if input {
        Some(
            engine.leader(LeaderConfig {
                bind: args
                    .get("--bind")
                    .map(String::as_str)
                    .unwrap_or("0.0.0.0:4443")
                    .parse()?,
                format: config.format(),
                source_kind: SourceKind::Tracked,
                advertise: !args.contains_key("--no-mdns"),
                ..Default::default()
            })?,
        )
    } else {
        None
    };
    if let Some(l) = &leader {
        println!(
            "Listening {} fingerprint {}",
            l.info().address,
            l.info().fingerprint
        );
    }
    let follower = if let Some(address) = args.get("--address") {
        let mut c = FollowerConfig::direct(address.parse()?);
        if let Some(pin) = args.get("--pin") {
            c.trust = Trust::Pinned(pin.clone());
        }
        Some(engine.follower(c)?)
    } else {
        None
    };
    macro_rules! build {
        ($fun:ident,$callback:expr) => {
            match supported.sample_format() {
                cpal::SampleFormat::F32 => $fun::<f32, _>(&device, audio, $callback),
                cpal::SampleFormat::I16 => $fun::<i16, _>(&device, audio, $callback),
                cpal::SampleFormat::I32 => $fun::<i32, _>(&device, audio, $callback),
                cpal::SampleFormat::U16 => $fun::<u16, _>(&device, audio, $callback),
                cpal::SampleFormat::F64 => $fun::<f64, _>(&device, audio, $callback),
                _ => unreachable!(),
            }
        };
    }
    let stream = if input {
        let mut source = LtcInput::new(config);
        let mut handoff = Handoff {
            producer,
            drops: drops.clone(),
            lost: false,
        };
        let callback = move |samples: &[f32], block: SampleBlock| {
            let mut at = 0;
            while at < samples.len() {
                let Some(timing) = block.advance(at, rate) else {
                    break;
                };
                let r = source.process(&samples[at..], timing);
                if r.consumed == 0 {
                    break;
                }
                at += r.consumed;
                if let Some(observation) = r.observation {
                    handoff.push(observation);
                }
            }
        };
        build!(input_stream, callback)?
    } else {
        let mut reader = follower.as_ref().map(|f| f.reader()).transpose()?;
        let mut output = LtcOutput::new(config);
        let mut generator = LtcEncoder::new(
            config,
            Position::from_frames(number(&args, "--start-frame", 0i64)?),
        );
        let amplitude = number(&args, "--amplitude", 0.5f32)?;
        if !output.set_amplitude(amplitude) || !generator.set_amplitude(amplitude) {
            return Err("amplitude must be finite and within 0..=1".into());
        }
        let callback = move |samples: &mut [f32], block: SampleBlock| {
            if let Some(r) = reader.as_mut() {
                output.render(&r.snapshot(), block, samples);
            } else {
                generator.render(samples);
            }
        };
        build!(output_stream, callback)?
    };
    refresh(&stream, clock, &mut bridge_tx);
    stream.play()?;
    let duration = super::common::duration(&args)?.unwrap_or(Duration::from_secs(10));
    let start = Instant::now();
    let mut report = Instant::now();
    while start.elapsed() < duration {
        refresh(&stream, clock, &mut bridge_tx);
        if let Some(l) = &leader {
            while let Ok(sample) = consumer.pop() {
                l.track(sample)?;
            }
        }
        if report.elapsed() >= Duration::from_secs(1) {
            println!(
                "device errors={} source queue drops={}",
                errors.load(Ordering::Relaxed),
                drops.load(Ordering::Relaxed)
            );
            report = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(stream);
    engine.shutdown()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_queue_overflow_marks_next_delivery() {
        let (producer, mut consumer) = rtrb::RingBuffer::new(1);
        let drops = Arc::new(AtomicU64::new(0));
        let mut handoff = Handoff {
            producer,
            drops: drops.clone(),
            lost: false,
        };
        let sample = SourceSample {
            timestamp_ns: 1,
            position: Position::ZERO,
            rate_hint: Some(Rate::NORMAL),
            discontinuity: false,
        };
        handoff.push(sample);
        handoff.push(sample);
        handoff.push(sample);
        assert_eq!(drops.load(Ordering::Relaxed), 2);
        assert!(!consumer.pop().unwrap().discontinuity);
        handoff.push(sample);
        assert!(consumer.pop().unwrap().discontinuity);
        handoff.push(sample);
        assert!(!consumer.pop().unwrap().discontinuity);
    }
}
