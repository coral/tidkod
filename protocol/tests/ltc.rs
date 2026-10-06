use tidkod_protocol::{ltc::*, *};

fn decode(c: LtcConfig, samples: &[f32], sizes: &[usize]) -> Vec<DecodedLtcFrame> {
    let mut d = LtcDecoder::new(c);
    let mut frames = Vec::new();
    let mut at = 0;
    let mut chunk = 0;
    while at < samples.len() {
        let end = (at + sizes[chunk % sizes.len()]).min(samples.len());
        chunk += 1;
        while at < end {
            let r = d.process(
                &samples[at..end],
                SampleBlock {
                    first_sample: at as u64,
                    first_sample_ns: sample_time(1_000_000_000, at as u64, c.sample_rate())
                        .unwrap(),
                },
            );
            assert!(r.consumed > 0);
            at += r.consumed;
            if let Some(f) = r.frame {
                frames.push(f);
            }
        }
    }
    frames
}
#[test]
fn rates_chunking_metadata_and_phase() {
    for (num, den, df) in [
        (24000, 1001, false),
        (24, 1, false),
        (25, 1, false),
        (30000, 1001, false),
        (30000, 1001, true),
        (30, 1, false),
    ] {
        for sr in [44100, 48000, 96000] {
            let c = LtcConfig::new(FrameFormat::new(num, den, df).unwrap(), sr).unwrap();
            let start = c
                .format()
                .position(0, 0, 59, c.format().nominal() as u8 - 2)
                .unwrap();
            let mut e = LtcEncoder::new(c, start);
            let m = LtcMetadata {
                user_bits: 0x12345678,
                binary_group_flags: 5,
                color_frame: true,
            };
            e.set_metadata(m);
            let mut all = vec![0.; sr as usize * 2];
            e.render(&mut all);
            let mut other = vec![0.; all.len()];
            e.reset(start);
            for b in other.chunks_mut(127) {
                e.render(b);
            }
            assert_eq!(all, other);
            let frames = decode(c, &all, &[1, 127, 512, 63, 2048]);
            assert!(frames.len() > 40, "{num}/{den} {sr}: {}", frames.len());
            for f in frames {
                assert_eq!(f.metadata, m);
                assert!(f.polarity_valid);
                let relative = (f.position.frames - start.frames) as u128;
                let expected = relative * u128::from(sr) * u128::from(den) / u128::from(num);
                assert!(f.start_sample.abs_diff(expected as u64) <= 1);
                assert!(f.end_sample > f.start_sample);
            }
        }
    }
}
#[test]
fn independent_codeword_pcm() {
    // ST 12-1 worked vector: 01:23:45:13. Construct PCM directly from these
    // bytes, independently of Tidkod's encoder, including a fractional origin.
    let word = [0x13u8, 0x29, 0x35, 0x4c, 0x53, 0x6a, 0x71, 0x88, 0xfc, 0xbf];
    let mut pcm = vec![0.; 37];
    let mut level = 0.6;
    for _ in 0..5 {
        for byte in word {
            for bit in 0..8 {
                level = -level;
                pcm.extend(std::iter::repeat_n(level, 10));
                if byte & (1 << bit) != 0 {
                    level = -level;
                }
                pcm.extend(std::iter::repeat_n(level, 10));
            }
        }
    }
    let c = LtcConfig::new(FrameFormat::default(), 48000).unwrap();
    let frames = decode(c, &pcm, &[129, 3, 1024]);
    assert!(frames.len() >= 3);
    for f in frames {
        assert_eq!(c.format().label(f.position).to_string(), "01:23:45:13");
        assert_eq!(f.metadata.user_bits, 0x87654321);
    }
}
#[test]
fn input_tracking_holdover_and_reset() {
    let c = LtcConfig::new(FrameFormat::new(30000, 1001, true).unwrap(), 48000).unwrap();
    let start = Position::from_frames(c.format().frames_per_day() - 4);
    let mut e = LtcEncoder::new(c, start);
    let mut samples = vec![0.; 48000];
    e.render(&mut samples);
    let mut input = LtcInput::new(c);
    input.reset(start);
    let mut at = 0;
    let mut last = None;
    while at < samples.len() {
        let r = input.process(
            &samples[at..],
            SampleBlock {
                first_sample: at as u64,
                first_sample_ns: sample_time(1_000_000_000, at as u64, 48000).unwrap(),
            },
        );
        at += r.consumed;
        if let Some(s) = r.observation {
            if last.is_none() {
                assert!(s.discontinuity);
                // Empty and rejected blocks cannot disturb acquired history.
                assert_eq!(input.process(&[], SampleBlock::default()).consumed, 0);
                assert_eq!(
                    input
                        .process(
                            &[0.],
                            SampleBlock {
                                first_sample: u64::MAX,
                                first_sample_ns: 0,
                            },
                        )
                        .state,
                    LtcState::InvalidTiming
                );
                assert_eq!(input.state(s.timestamp_ns), LtcState::Locked);
            } else {
                assert!(!s.discontinuity);
            }
            last = Some(s);
        }
    }
    let last = last.unwrap();
    assert!(last.position.frames >= c.format().frames_per_day());
    assert_eq!(input.state(2_000_000_000), LtcState::Locked);
    assert_eq!(input.state(3_000_000_000), LtcState::Holdover);
    assert!(input.read(3_000_000_000).position > last.position);
    input.reset(Position::ZERO);
    assert_eq!(input.state(3_000_000_000), LtcState::Searching);
}
#[test]
fn damaged_signal_reacquires() {
    let c = LtcConfig::new(FrameFormat::default(), 48000).unwrap();
    let mut e = LtcEncoder::new(c, Position::ZERO);
    let mut data = vec![0.; 96000];
    e.render(&mut data);
    for (i, s) in data.iter_mut().enumerate() {
        *s = -*s * 0.5 + 0.15 + ((i * 17 % 31) as f32 - 15.) * 0.0005;
    }
    data[20000..26000].fill(0.);
    data[35000] = f32::NAN;
    data[40000] = f32::INFINITY;
    let frames = decode(c, &data, &[127]);
    assert!(frames.len() > 35);
    assert!(frames.last().unwrap().position.frames > 55);
}

fn view() -> timeline::View {
    let mut v = timeline::View {
        local_clock: true,
        connection: ConnectionState::Connected,
        sync: SyncState::Synchronized,
        mapping: clock::ClockMapping {
            last_sample_ns: 1,
            uncertainty_ns: 0.,
            converged: true,
            ..Default::default()
        },
        ..Default::default()
    };
    v.timeline.anchor.time_ns = 1_000_000_000;
    v.timeline.anchor.rate = Rate::NORMAL;
    v
}
#[test]
fn snapshot_output_controls_holdover_and_rearm() {
    let c = LtcConfig::new(FrameFormat::default(), 48000).unwrap();
    let mut v = view();
    v.timeline.scheduled_len = 2;
    v.timeline.scheduled[0] = timeline::Scheduled {
        discontinuity: 1,
        anchor: timeline::Anchor {
            time_ns: 1_100_000_000,
            position: Position::from_frames(3),
            rate: Rate::PAUSED,
        },
    };
    v.timeline.scheduled[1] = timeline::Scheduled {
        discontinuity: 2,
        anchor: timeline::Anchor {
            time_ns: 1_200_000_000,
            position: Position::from_frames(900),
            rate: Rate::NORMAL,
        },
    };
    let mut output = LtcOutput::new(c);
    let mut samples = vec![0.; 48000];
    assert_eq!(
        output.render(
            &v.into(),
            SampleBlock {
                first_sample: 0,
                first_sample_ns: 1_000_000_000
            },
            &mut samples
        ),
        LtcState::Locked
    );
    assert!(samples[4800..9600].iter().all(|s| *s == 0.));
    let frames = decode(c, &samples, &[127]);
    assert!(frames.iter().any(|f| f.position.frames >= 901));
    v.sync = SyncState::Holdover;
    v.connection = ConnectionState::Disconnected;
    v.timeline.source_health = SourceHealth::Degraded;
    assert_eq!(
        output.render(
            &v.into(),
            SampleBlock {
                first_sample: 48000,
                first_sample_ns: 2_000_000_000
            },
            &mut samples[..512]
        ),
        LtcState::Holdover
    );
    assert!(samples[..512].iter().all(|s| s.abs() == 0.5));
    output.mute(true);
    output.render(
        &v.into(),
        SampleBlock {
            first_sample: 48512,
            first_sample_ns: 2_010_666_666,
        },
        &mut samples[..512],
    );
    assert!(samples[..512].iter().all(|s| *s == 0.));
    // A hard resync at a fractional position cannot emit a partial codeword.
    let mut v = view();
    v.timeline.anchor.position = Position {
        frames: 30,
        subframe: 1 << 31,
    };
    v.resync_generation = 2;
    output.mute(false);
    output.render(
        &v.into(),
        SampleBlock {
            first_sample: 0,
            first_sample_ns: 1_000_000_000,
        },
        &mut samples[..1600],
    );
    assert!(samples[..800].iter().all(|s| *s == 0.));
    assert!(samples[801..1600].iter().all(|s| s.abs() == 0.5));
    v.timeline.anchor.rate = Rate::new(-1, 1).unwrap();
    assert_eq!(
        output.render(
            &v.into(),
            SampleBlock {
                first_sample: 1600,
                first_sample_ns: 1_033_333_333
            },
            &mut samples[..512]
        ),
        LtcState::Unsupported
    );
    assert!(samples[..512].iter().all(|s| *s == 0.));
}

#[test]
fn rejects_unsupported_format_invalid_timestamps_and_bad_amplitudes() {
    assert!(LtcConfig::new(FrameFormat::new(60, 1, false).unwrap(), 48000).is_err());
    let c = LtcConfig::new(FrameFormat::default(), 48000).unwrap();
    let mut e = LtcEncoder::new(c, Position::ZERO);
    assert!(!e.set_amplitude(f32::NAN));
    assert!(!e.set_amplitude(2.));
    let mut input = LtcInput::new(c);
    assert_eq!(
        input
            .process(
                &[0.; 128],
                SampleBlock {
                    first_sample: u64::MAX,
                    first_sample_ns: 1
                }
            )
            .state,
        LtcState::InvalidTiming
    );
    assert_eq!(
        input
            .process(
                &[0.; 128],
                SampleBlock {
                    first_sample: 0,
                    first_sample_ns: i64::MAX as u64
                }
            )
            .state,
        LtcState::InvalidTiming
    );
}

#[test]
fn refreshed_snapshots_preserve_codewords_and_converge() {
    for sr in [16000, 44100, 48000, 96000] {
        let c = LtcConfig::new(FrameFormat::default(), sr).unwrap();
        for change in [-0.02, -0.005, 0.005, 0.02] {
            let mut v = view();
            let mut output = LtcOutput::new(c);
            let mut pcm = vec![0.; sr as usize * 4];
            let metadata = LtcMetadata {
                user_bits: 0x87654321,
                binary_group_flags: 5,
                color_frame: true,
            };
            output.set_metadata(metadata);
            // Refresh near frame cadence, including a small backward phase step.
            let size = sr as usize / 30;
            for (i, chunk) in pcm.chunks_mut(size).enumerate() {
                v.timeline.anchor.position =
                    Position::from_fixed((i.min(29) as f64 * change * 4294967296.) as i128);
                let at = (i * size) as u64;
                assert_eq!(
                    output.render(
                        &v.into(),
                        SampleBlock {
                            first_sample: at,
                            first_sample_ns: sample_time(1_000_000_000, at, sr).unwrap(),
                        },
                        chunk
                    ),
                    LtcState::Locked
                );
            }
            let frames = decode(c, &pcm, &[127, 512, 3]);
            assert!(frames.len() >= 116, "{sr} {change}: {}", frames.len());
            for pair in frames.windows(2) {
                assert_eq!(pair[1].position.frames, pair[0].position.frames + 1);
                assert_eq!(pair[1].start_sample, pair[0].end_sample);
            }
            for frame in &frames {
                assert_eq!(frame.metadata, metadata);
                assert!(frame.polarity_valid);
            }
            let last = frames.last().unwrap();
            let expected = v.evaluate(last.start_ns).position;
            assert!((expected.as_frames() - last.position.as_frames()).abs() < 0.003);
        }
    }
}

#[test]
fn default_snapshot_slew_remains_decodable() {
    for (n, d) in [(30, 1), (25, 1), (24000, 1001), (30000, 1001)] {
        for sign in [-1., 1.] {
            let c = LtcConfig::new(FrameFormat::new(n, d, false).unwrap(), 48000).unwrap();
            let mut v = view();
            v.timeline.format = c.format();
            v.correction_frames = sign * 0.25;
            v.correction_at = 1_000_000_000;
            v.slew_frames_per_second =
                0.25 * 1e9 / timeline::CorrectionPolicy::default().settle_time_ns as f64;
            let mut output = LtcOutput::new(c);
            let mut pcm = vec![0.; 96000];
            assert_eq!(
                output.render(
                    &v.into(),
                    SampleBlock {
                        first_sample: 0,
                        first_sample_ns: 1_000_000_000,
                    },
                    &mut pcm
                ),
                LtcState::Locked
            );
            let initial = decode(c, &pcm[..9600], &[127]);
            assert!(initial.len() >= 2, "{n}/{d} {sign}");
            let frames = decode(c, &pcm, &[512]);
            for pair in frames.windows(2) {
                assert_eq!(pair[1].position.frames, pair[0].position.frames + 1);
                assert_eq!(pair[1].start_sample, pair[0].end_sample);
            }
            let last = frames.last().unwrap();
            assert!(
                (v.evaluate(last.start_ns).position.as_frames() - last.position.as_frames()).abs()
                    < 0.002
            );
        }
    }
}
