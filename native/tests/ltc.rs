use std::time::Duration;
use tidkod::{ltc::*, *};
#[test]
fn decoded_source_crosses_quic_and_renders_follower_ltc() {
    let engine = Engine::new().unwrap();
    let clock = engine.clock();
    let leader = engine
        .leader(LeaderConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            source_kind: SourceKind::Tracked,
            advertise: false,
            ..Default::default()
        })
        .unwrap();
    let follower = engine
        .follower(FollowerConfig::direct(leader.info().address))
        .unwrap();
    let mut reader = follower.reader().unwrap();
    let config = LtcConfig::new(FrameFormat::default(), 48000).unwrap();
    let mut encoder = LtcEncoder::new(config, Position::from_frames(300));
    let mut source = LtcInput::new(config);
    let mut pcm = [0.; 1600];
    let mut observations = 0;
    let mut last = None;
    let origin = clock.now_ns();
    for chunk in 0..30u64 {
        encoder.render(&mut pcm);
        let first = chunk * 1600;
        let deadline = sample_time(origin, first + 1600, 48000).unwrap();
        while clock.now_ns() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        let mut offset = 0;
        while offset < pcm.len() {
            let index = first + offset as u64;
            let result = source.process(
                &pcm[offset..],
                SampleBlock {
                    first_sample: index,
                    first_sample_ns: sample_time(origin, index, 48000).unwrap(),
                },
            );
            assert!(result.consumed > 0);
            offset += result.consumed;
            if let Some(s) = result.observation {
                leader.track(s).unwrap();
                last = Some(s);
                observations += 1;
            }
        }
    }
    assert!(observations > 20);
    let at = clock.now_ns();
    let reading = reader.read_at(at);
    let sample = last.unwrap();
    assert_eq!(reading.status.synchronization, SyncState::Synchronized);
    assert_eq!(reading.status.source_kind, SourceKind::Tracked);
    assert_eq!(reading.status.source_health, SourceHealth::Healthy);
    let expected = sample.position.advance(
        (at - sample.timestamp_ns) as i64,
        config.format(),
        Rate::NORMAL,
    );
    assert!((reading.position.as_frames() - expected.as_frames()).abs() < 0.2);
    let snapshot = reader.snapshot();
    let mut output = LtcOutput::new(config);
    let mut rendered = [0.; 16000];
    assert_eq!(
        output.render(
            &snapshot,
            SampleBlock {
                first_sample: 0,
                first_sample_ns: at
            },
            &mut rendered
        ),
        LtcState::Locked
    );
    let mut decoder = LtcDecoder::new(config);
    let mut offset = 0;
    let mut count = 0;
    while offset < rendered.len() {
        let r = decoder.process(
            &rendered[offset..],
            SampleBlock {
                first_sample: offset as u64,
                first_sample_ns: sample_time(at, offset as u64, 48000).unwrap(),
            },
        );
        assert!(r.consumed > 0);
        offset += r.consumed;
        if let Some(f) = r.frame {
            let expected = snapshot.evaluate(f.start_ns);
            assert!((f.position.as_frames() - expected.position.as_frames()).abs() < 0.01);
            count += 1;
        }
    }
    assert!(count >= 8);
    engine.shutdown().unwrap();
}
