#include "tidkod.h"
#include "check.h"
#include <stdio.h>
#include <string.h>
static void check_ltc(void) {
    LtcEncoder *encoder = NULL; LtcDecoder *decoder = NULL; TKBuffer *error = NULL;
    CHECK(tidkod_ltc_encoder_new(30000, 1001, true, 48000, 1797, &encoder, &error) == 0);
    CHECK(tidkod_ltc_decoder_new(30000, 1001, true, 48000, &decoder, &error) == 0);
    CHECK(tidkod_ltc_encoder_set_metadata(encoder, 0x87654321u, 5, true, &error) == 0);
    float samples[8000]; uint8_t state = 0;
    CHECK(tidkod_ltc_encoder_render(encoder, samples, 8000, &state, &error) == 0 && state == 2);
    size_t offset = 0; unsigned frames = 0;
    while (offset < 8000) {
        LtcResult result;
        CHECK(tidkod_ltc_decoder_process(decoder, samples + offset, 8000 - offset,
            offset, 1000000000ULL + offset * 1000000000ULL / 48000, &result, &error) == 0);
        CHECK(result.consumed > 0); offset += (size_t)result.consumed;
        if (result.has_frame) { ++frames; CHECK(result.user_bits == 0x87654321u && result.polarity_valid); }
    }
    CHECK(frames >= 3);
    CHECK(tidkod_ltc_encoder_render(encoder, NULL, 0, &state, &error) == 0);
    tidkod_ltcencoder_free(encoder); tidkod_ltcdecoder_free(decoder);
}
int main(void) {
    check_ltc();
    Core *core = NULL;
    TKBuffer *error = NULL;
    CHECK(tidkod_core_new(&core, &error) == 0);
    Reading value;
    CHECK(tidkod_core_read(core, 123, &value, &error) == 0);
    CHECK(value.frames == 0 && value.fps_numerator == 30);
    TimecodeSnapshot *snapshot = NULL, *copy = NULL;
    CHECK(tidkod_core_snapshot(core, &snapshot, &error) == 0);
    CHECK(tidkod_timecode_snapshot_copy(snapshot, &copy, &error) == 0);
    CHECK(tidkod_core_snapshot_into(core, snapshot, &error) == 0);
    CHECK(tidkod_timecode_snapshot_read(snapshot, 123, &value, &error) == 0);
    CHECK(value.accepted_observations == 0);
    Boundary boundary;
    CHECK(tidkod_timecode_snapshot_next_boundary(snapshot, 123, &boundary, &error) == 0);
    CHECK(!boundary.valid);
    CHECK(tidkod_timecode_snapshot_read_for_presentation(snapshot, 123, 1, &value, &error) == 0);
    CHECK(tidkod_timecode_snapshot_read_sample(snapshot, 123, 48000, 48000, &value, &error) == 0);
    CHECK(!value.aligned);
    ClockBridge *bridge = NULL;
    OutputTime output_time;
    CHECK(tidkod_clock_bridge_new(100, 10000, 110, &bridge, &error) == 0);
    CHECK(tidkod_clock_bridge_convert(bridge, 10100, &output_time, &error) == 0);
    CHECK(output_time.local_ns == 205 && output_time.uncertainty_ns >= 5);
    tidkod_clockbridge_free(bridge);
    const uint8_t bad[] = {255};
    CHECK(tidkod_core_state(core, bad, sizeof(bad), 123, &error) != 0);
    CHECK(error && tidkod_buffer_len(error) > 0);
    tidkod_buffer_free(error);
    error = NULL;
    tidkod_core_free(core);
    CHECK(tidkod_timecode_snapshot_read(copy, 123, &value, &error) == 0);
    tidkod_timecodesnapshot_free(copy);
#ifdef TIDKOD_NATIVE
    Engine *engine = NULL;
    LeaderOptions *options = NULL;
    Leader *leader = NULL;
    Reader *reader = NULL;
    CHECK(tidkod_engine_new(&engine, &error) == 0);
    CHECK(tidkod_leader_options_new(&options, &error) == 0);
    CHECK(tidkod_leader_options_advertise(options, false, &error) == 0);
    const uint8_t bind_address[] = "127.0.0.1:0";
    CHECK(tidkod_leader_options_bind(options, bind_address, sizeof(bind_address)-1, &error) == 0);
    CHECK(tidkod_engine_leader(engine, options, &leader, &error) == 0);
    CHECK(tidkod_leader_reader(leader, &reader, &error) == 0);
    CHECK(tidkod_leader_seek(leader, -7, 0x80000000u, &error) == 0);
    CHECK(tidkod_reader_read(reader, &value, &error) == 0);
    CHECK(value.frames == -7 && value.subframe == 0x80000000u);
    CHECK(tidkod_reader_snapshot_into(reader, snapshot, &error) == 0);
    EndpointList *endpoints = NULL;
    Endpoint *endpoint = NULL;
    uint32_t count = 0;
    CHECK(tidkod_leader_local_endpoints(leader, &endpoints, &error) == 0);
    CHECK(tidkod_endpoint_list_count(endpoints, &count, &error) == 0 && count == 1);
    CHECK(tidkod_endpoint_list_get(endpoints, 0, &endpoint, &error) == 0);
    tidkod_endpoint_free(endpoint);
    CHECK(tidkod_endpoint_list_get(endpoints, count, &endpoint, &error) != 0);
    tidkod_buffer_free(error); error = NULL;
    tidkod_endpointlist_free(endpoints);
    CHECK(tidkod_engine_shutdown(engine, &error) == 0);
    tidkod_reader_free(reader);
    tidkod_leader_free(leader);
    tidkod_leaderoptions_free(options);
    tidkod_engine_free(engine);
    CHECK(tidkod_timecode_snapshot_read(snapshot, 123, &value, &error) == 0);
    CHECK(value.frames == -7);
#endif
    tidkod_timecodesnapshot_free(snapshot);
    puts("C bindings passed");
}
