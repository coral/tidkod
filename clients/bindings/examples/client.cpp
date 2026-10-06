#include "tidkod-client.hpp"
#include "check.h"
#include <iostream>
int main() {
    namespace es = tidkod::client;
    auto ltc_encoder = es::LtcEncoder::create(25, 1, false, 48000, 90);
    auto ltc_decoder = es::LtcDecoder::create(25, 1, false, 48000);
    CHECK(ltc_encoder && ltc_decoder);
    float pcm[8000];
    CHECK(ltc_encoder.value().render({pcm, 8000}) == 2);
    size_t ltc_offset = 0; unsigned ltc_frames = 0;
    while (ltc_offset < 8000) {
        auto result = ltc_decoder.value().process({pcm + ltc_offset, 8000 - ltc_offset}, ltc_offset,
            1000000000ULL + ltc_offset * 1000000000ULL / 48000);
        CHECK(result.consumed > 0); ltc_offset += result.consumed;
        if (result.has_frame) { ++ltc_frames; CHECK(result.frames >= 90); }
    }
    CHECK(ltc_frames >= 3);
    auto core = es::Core::create();
    CHECK(core.read(123).fps_numerator == 30);
    auto snapshot = es::TimecodeSnapshot::create();
    core.snapshot_into(snapshot);
    const auto frozen = snapshot.copy();
    CHECK(frozen.read(123).accepted_observations == 0);
    CHECK(!frozen.next_boundary(123).valid);
    CHECK(frozen.read_for_presentation(123, 1));
    CHECK(frozen.read_sample(123, 48000, 48000));
    auto bridge = es::ClockBridge::create(100, 10000, 110);
    CHECK(bridge);
    auto output_time = bridge.value().convert(10100);
    CHECK(output_time && output_time.value().local_ns == 205);
    CHECK(!bridge.value().convert(0));
    CHECK(!es::Core::build_id().empty());
    auto bad = core.state({255}, 123);
    CHECK(!bad && !bad.error().empty());
#ifdef TIDKOD_NATIVE
    auto created = es::Engine::create();
    if (!created) { std::cerr << created.error(); return 1; }
    auto engine = created.take();
    auto options = es::LeaderOptions::create();
    options.advertise(false);
    options.session_id(0x1234, 0x5678);
    auto endpoint = es::Endpoint::loopback(0);
    CHECK(endpoint);
    options.bind_endpoint(endpoint.value());
    auto started = engine.leader(options);
    CHECK(started);
    auto leader = started.take();
    auto reading = leader.reader();
    CHECK(reading);
    auto reader = reading.take();
    CHECK(leader.seek(-7, 0x80000000u));
    CHECK(reader.read().frames == -7);
    CHECK(reader.read().has_session_id && reader.read().session_id_high == 0x1234);
    auto part = leader.rotate_session_id();
    CHECK(part && reader.read().session_id_low == part.value().low);
    CHECK(leader.set_session_id(0x8765, 0x4321));
    CHECK(reader.read().session_id_low == 0x4321);
    CHECK(reader.read().subframe == 0x80000000u);
    reader.snapshot_into(snapshot);
    const auto leader_snapshot = reader.snapshot();
    auto endpoints = leader.local_endpoints();
    CHECK(endpoints && endpoints.value().count() == 1);
    CHECK(endpoints.value().get(0));
    CHECK(!endpoints.value().get(1));
    auto address = leader.endpoint();
    auto follow_options = es::FollowerOptions::endpoint(address);
    CHECK(follow_options);
    follow_options.value().pin(leader.fingerprint());
    auto started_follower = engine.follower(follow_options.value());
    CHECK(started_follower);
    auto follower = started_follower.take();
    auto remote_result = follower.reader();
    CHECK(remote_result);
    auto remote = remote_result.take();
    unsigned attempts = 0;
    while (remote.read().synchronization != 2) { CHECK(++attempts < 1000); smoke_sleep(); }
    auto synced = remote.read();
    CHECK(synced.frames == -7 && synced.subframe == 0x80000000u);
    CHECK(synced.accepted_observations >= 12);
    CHECK(follower.shutdown());
    CHECK(engine.shutdown());
    CHECK(leader_snapshot.read(123).frames == -7);
#endif
    std::cout << "C++ client passed\n";
}
