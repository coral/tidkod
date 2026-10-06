use tidkod_bindings::api::*;
#[cfg(feature = "native")]
#[test]
fn external_discovery_captures_actual_listener_without_mutating_options() {
    let engine = engine_new().unwrap();
    let mut options = leader_options_new();
    leader_options_name(&mut options, &"é".repeat(30));
    leader_options_identity(&mut options, "apple-adapter-test");
    leader_options_bind_endpoint(&mut options, &endpoint_loopback(0).unwrap());
    let leader = engine_leader_external_discovery(&engine, &options).unwrap();
    let info = leader_advertisement(&leader);
    assert!(leader_advertisement_enabled(&info));
    assert_eq!(leader_advertisement_identity(&info), "apple-adapter-test");
    assert_eq!(
        leader_advertisement_bind_address(&info),
        endpoint_address(&leader_endpoint(&leader))
    );
    assert_ne!(endpoint_port(&leader_endpoint(&leader)), 0);
    assert_eq!(
        leader_advertisement_fingerprint(&info),
        leader_fingerprint(&leader)
    );
    assert!(leader_advertisement_instance(&info).len() <= 63);
    let host = leader_advertisement_hostname(&info);
    leader_rotate_session_id(&leader).unwrap();
    assert_eq!(
        host,
        leader_advertisement_hostname(&leader_advertisement(&leader))
    );
    leader_options_name(&mut options, "changed");
    let second = engine_leader_external_discovery(&engine, &options).unwrap();
    assert!(leader_advertisement_enabled(&leader_advertisement(&second)));
    assert_ne!(
        host,
        leader_advertisement_hostname(&leader_advertisement(&second))
    );
    assert_eq!(leader_advertisement_name(&info), "é".repeat(30));
    assert!(follower_options_resolved("127.0.0.1:4443", &leader_fingerprint(&leader), 2).is_err());
    assert!(follower_options_resolved("hostname:4443", &leader_fingerprint(&leader), 1).is_err());
    assert!(follower_options_resolved("[fe80::1%7]:4443", &leader_fingerprint(&leader), 1).is_ok());
    engine_shutdown(&engine).unwrap();
}
#[test]
fn timecode_arithmetic_is_exact_and_validates_labels() {
    assert!(timecode_format_new(25, 1, true).is_err());
    let f = timecode_format_new(30000, 1001, true).unwrap();
    assert!(timecode_format_position(&f, 0, 1, 0, 0).is_err());
    assert_eq!(
        timecode_format_position(&f, 0, 1, 0, 2).unwrap().frames,
        1800
    );
    assert_eq!(
        timecode_format_position(&f, 0, 10, 0, 0).unwrap().frames,
        17982
    );
    assert!(timecode_format_position(&f, 24, 0, 0, 0).is_err());
    let p = timecode_format_elapsed(&f, 1_001_000_000);
    assert_eq!((p.frames, p.subframe), (30, 0));
    let f = timecode_format_new(30, 1, false).unwrap();
    let p = timecode_format_elapsed(&f, -50_000_000);
    assert_eq!((p.frames, p.subframe), (-2, 0x80000000));
    assert_eq!(
        timecode_format_elapsed(&f, 86_400_000_000_000).frames,
        2_592_000
    );
}
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};
struct Alloc;
thread_local! {static ENABLED:Cell<bool>=const{Cell::new(false)};static COUNT:Cell<usize>=const{Cell::new(0)};}
unsafe impl GlobalAlloc for Alloc {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if ENABLED.try_with(Cell::get).unwrap_or(false) {
            let _ = COUNT.try_with(|c| c.set(c.get() + 1));
        }
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        if ENABLED.try_with(Cell::get).unwrap_or(false) {
            let _ = COUNT.try_with(|c| c.set(c.get() + 1));
        }
        unsafe { System.realloc(p, l, n) }
    }
}
#[global_allocator]
static ALLOC: Alloc = Alloc;
#[test]
fn ltc_processing_and_recovery_do_not_allocate() {
    let mut encoder = ltc_encoder_new(30000, 1001, true, 48000, 1797).unwrap();
    let mut decoder = ltc_decoder_new(30000, 1001, true, 48000).unwrap();
    let mut input = ltc_input_new(30000, 1001, true, 48000).unwrap();
    let mut output = ltc_output_new(30000, 1001, true, 48000).unwrap();
    let mut snapshot = timecode_snapshot_new();
    let mut samples = [0.; 512];
    let mut rendered = [0.; 512];
    let mut found = 0;
    COUNT.with(|c| c.set(0));
    ENABLED.with(|c| c.set(true));
    for block in 0..200u64 {
        assert_eq!(ltc_encoder_render(&mut encoder, &mut samples), 2);
        if block == 100 {
            samples[17] = f32::NAN;
            ltc_decoder_reset(&mut decoder);
        }
        for tracked in [false, true] {
            let mut at = 0;
            while at < samples.len() {
                let index = block * 512 + at as u64;
                let ns = 1_000_000_000 + index * 1_000_000_000 / 48000;
                let r = if tracked {
                    ltc_input_process(&mut input, &samples[at..], index, ns)
                } else {
                    ltc_decoder_process(&mut decoder, &samples[at..], index, ns)
                };
                assert!(r.consumed > 0);
                at += r.consumed as usize;
                found += usize::from(r.has_frame);
            }
        }
        ltc_input_capture_into(
            &input,
            &mut snapshot,
            1_000_000_000 + block * 512 * 1_000_000_000 / 48000,
        );
        ltc_output_render(
            &mut output,
            &snapshot,
            &mut rendered,
            block * 512,
            1_000_000_000 + block * 512 * 1_000_000_000 / 48000,
        );
    }
    ltc_input_reset(&mut input, 0);
    ltc_output_reset(&mut output);
    ltc_encoder_reset(&mut encoder, 0);
    ENABLED.with(|c| c.set(false));
    assert!(found > 80);
    assert_eq!(COUNT.with(Cell::get), 0);
}
#[test]
fn exact_large_position_and_allocation_free_portable_reads() {
    let core = core_configured(i64::MAX - 7, u32::MAX, 30, 1, false, 0.03, 1., 3).unwrap();
    let value = core_read(&core, 0);
    assert_eq!(value.frames, i64::MAX - 7);
    assert_eq!(value.subframe, u32::MAX);
    let mut snapshot = timecode_snapshot_new();
    let copy = timecode_snapshot_copy(&core_snapshot(&core));
    COUNT.with(|c| c.set(0));
    ENABLED.with(|c| c.set(true));
    for i in 0..100_000 {
        std::hint::black_box(core_read(&core, i));
        std::hint::black_box(core_next_boundary(&core, i));
        core_snapshot_into(&core, &mut snapshot);
        std::hint::black_box(timecode_snapshot_read(&snapshot, i));
        std::hint::black_box(timecode_snapshot_read_sample(&snapshot, i, i, 48000).unwrap());
        std::hint::black_box(timecode_snapshot_read_for_presentation(&snapshot, i, 1).unwrap());
        std::hint::black_box(timecode_snapshot_next_boundary(&snapshot, i));
    }
    ENABLED.with(|c| c.set(false));
    assert_eq!(COUNT.with(Cell::get), 0);
    drop(core);
    assert_eq!(timecode_snapshot_read(&copy, 100).frames, i64::MAX - 7);
    assert!(timecode_snapshot_read_for_presentation(&copy, i64::MAX as u64, 1).is_err());
}
#[cfg(feature = "native")]
#[test]
fn control_acknowledges_reader_publication_and_children_own_engine() {
    let engine = engine_new().unwrap();
    let mut options = leader_options_new();
    leader_options_advertise(&mut options, false);
    leader_options_session_id(&mut options, u64::MAX, 0x1234);
    let leader = engine_leader(&engine, &options).unwrap();
    let mut reader = leader_reader(&leader).unwrap();
    let initial = reader_read(&mut reader);
    assert!(initial.has_session_id);
    assert_eq!(
        (initial.session_id_high, initial.session_id_low),
        (u64::MAX, 0x1234)
    );
    let next = leader_rotate_session_id(&leader).unwrap();
    let r = reader_read(&mut reader);
    assert_eq!((r.session_id_high, r.session_id_low), (next.high, next.low));
    leader_set_session_id(&leader, 0, u64::MAX).unwrap();
    assert_eq!(reader_read(&mut reader).session_id_low, u64::MAX);
    let endpoints = leader_local_endpoints(&leader).unwrap();
    assert!(endpoint_list_count(&endpoints) > 0);
    assert!(endpoint_list_get(&endpoints, endpoint_list_count(&endpoints)).is_err());
    drop(engine); // The leader retains its worker owner.
    for frame in 1..1000 {
        leader_seek(&leader, frame, 0x12345678).unwrap();
        let r = reader_read(&mut reader);
        assert_eq!(r.frames, frame);
        assert_eq!(r.subframe, 0x12345678);
    }
    let mut snapshot = reader_snapshot(&mut reader);
    let copied = timecode_snapshot_copy(&snapshot);
    leader_seek(&leader, -7, 0x80000000).unwrap();
    COUNT.with(|c| c.set(0));
    ENABLED.with(|c| c.set(true));
    for _ in 0..100_000 {
        std::hint::black_box(reader_read(&mut reader));
        reader_snapshot_into(&mut reader, &mut snapshot);
        std::hint::black_box(timecode_snapshot_read(&snapshot, 0));
    }
    ENABLED.with(|c| c.set(false));
    assert_eq!(COUNT.with(Cell::get), 0);
    leader_shutdown(&leader).unwrap();
    drop((reader, leader));
    assert_eq!(timecode_snapshot_read(&copied, 0).frames, 999);
    assert_eq!(timecode_snapshot_read(&snapshot, 0).frames, -7);
}

#[cfg(feature = "native")]
#[test]
fn typed_endpoints_validate_and_preserve_ipv6_scope() {
    assert!(endpoint_ipv4(&[127, 0, 0], 4443).is_err());
    assert!(endpoint_ipv4(&[127, 0, 0, 1], 65536).is_err());
    assert!(endpoint_ipv6(&[0; 15], 4443, 0).is_err());
    assert!(endpoint_parse("host.invalid:4443").is_err());
    let endpoint = endpoint_parse("[fe80::1%7]:4443").unwrap();
    assert_eq!(endpoint_port(&endpoint), 4443);
    assert_eq!(endpoint_address(&endpoint), "[fe80::1%7]:4443");
    assert!(follower_options_endpoint(&endpoint).is_ok());
    assert!(follower_options_endpoint(&endpoint_loopback(0).unwrap()).is_err());
    assert!(follower_options_endpoint(&endpoint_any_ipv4(4443).unwrap()).is_err());
    let mut options = leader_options_new();
    leader_options_bind_endpoint(&mut options, &endpoint_loopback(0).unwrap());
}

#[test]
fn core_recording_id_is_exact_optional_and_allocation_free() {
    let mut core = core_new();
    assert!(!core_read(&core, 1).has_session_id);
    core_connected(&mut core);
    let timeline = tidkod_protocol::timeline::Timeline {
        session: [1; 16],
        session_id: Some([0xff; 16]),
        revision: 1,
        ..Default::default()
    };
    core_state(
        &mut core,
        &tidkod_protocol::encode(&timeline.wire()).unwrap(),
        1,
    )
    .unwrap();
    let frozen = core_snapshot(&core);
    COUNT.with(|c| c.set(0));
    ENABLED.with(|c| c.set(true));
    for now in 1..1000 {
        let r = timecode_snapshot_read(&frozen, now);
        assert!(r.has_session_id);
        assert_eq!((r.session_id_high, r.session_id_low), (u64::MAX, u64::MAX));
    }
    ENABLED.with(|c| c.set(false));
    assert_eq!(COUNT.with(Cell::get), 0);
}
