# Validation

## Reproducible local checks

Run `scripts/check.sh` after installing the pinned schema tools listed in `.github/workflows/validation.yml`. It executes formatting, Clippy with warnings denied, workspace tests/doctests, Rustdoc with warnings denied, Buf lint/format/build, protoc-gen-doc generation, and every example. `scripts/smoke_examples.py` starts actual separate leader/follower processes, asserts reverse playback and synchronized status, and runs the tracked example through loss, pause, and reverse.

Run the network-dependent discovery test separately:

```sh
cargo test -p tidkod --test mdns -- --ignored --nocapture
```

It advertises two equal display names, checks distinct identities and usable endpoint/pin metadata, and observes goodbye removal after leader shutdown. It requires a multicast-capable interface; a failure here must be reported independently from direct QUIC functionality.

The tests cover:

- Protocol golden fixtures, malformed randomized inputs, version/enum/rational validation, 512-byte limits, and ordered bounded schedules.
- Exact rational arithmetic, signed Q32 subframes, overflow-safe reverse motion, all frame formats, midnight wrapping, and every frame label in a full day at both drop-frame rates.
- Deterministic symmetric LAN delay uniformly distributed from 1–5 ms per direction, ±200 ppm drift, 1% loss, acquisition in less than two seconds, and at most 2 ms p95 clock-map error after acquisition.
- Asymmetry uncertainty, holdover uncertainty growth, invalid exchanges, reordering, delayed/lost probes, a latency step from 2 ms to 30 ms each way, a ten-second outage, and recovery.
- Immediate and late discontinuities, stale snapshots, changed sessions, scheduled controls during holdover, bounded slew, and three-measurement hard correction.
- Tracked input jitter, estimated speed, source loss, pause/reverse hints, and input recovery.
- Real QUIC with multiple followers, late joining, correct/incorrect pins, IPv4 and IPv6 loopback, same-address session restart, signed running and paused holdover, undrained events, and repeated startup/shutdown.
- A thread-local allocation counter around 100,000 reader evaluations, live-reader limits, slot reclamation, and shutdown state publication.

The simulation uses a reproducible linear-congruential random generator. The reported clock-mapping error is compared to the simulated ground truth, not to the estimator's own uncertainty. The accuracy target applies to the stated symmetric simulated network; asymmetric paths and oscillator changes can produce additional error.

## Two-computer Wi-Fi procedure

1. Build the same checkout and Cargo.lock on two computers. Record hardware, OS, power mode, Rust version, Wi-Fi adapter/driver, AP model, band/channel, and the revision under test. Put both machines on the same LAN with client isolation disabled. Permit UDP 4443 and mDNS UDP 5353 in the host firewalls. Avoid suspend during the run.
2. On computer A, run `cargo run --release -p tidkod --example leader -- --bind 0.0.0.0:4443 --fps 29.97 --drop-frame --start 107892`. Record its displayed certificate fingerprint and Wi-Fi IP. On B, run the follower with `--address A_WIFI_IP:4443 --pin FINGERPRINT --seconds 600`, saving stdout/stderr. Then repeat using discovery selection without `--address` and check that the same identity is shown.
3. On A, issue `play`, `pause`, `seek 0`, `shuttle -1 1`, `shuttle 1 2`, and `at 1000 1800 0 1`. Confirm B acquires synchronization, changes discontinuities immediately on explicit commands, runs in the correct direction/rate, and reaches the scheduled paused position. Record acquisition time, uncertainty, sample age, offset/drift, RTT, and loss counters. Console labels are display diagnostics, not subframe measurements.
4. While running at -1x, disconnect A from Wi-Fi for 30 seconds. B must enter holdover and continue in reverse with increasing uncertainty. Reconnect A and observe recovery. Repeat while paused; position must remain fixed. Restart the leader on the same port, exchange its new fingerprint through the trusted channel, and restart B with the new pin. For a separate trusted-LAN recovery test, omit `--pin` and verify that the existing follower reconnects and reinitializes on the new session. Confirm it never switches to another advertised leader.
5. Run the tracked example on A for ten seconds and observe B's `Tracked/Healthy` → `Tracked/Degraded` → `Tracked/Healthy` transitions during the 3–5 second input gap. Timecode continues through the gap; later pause/reverse changes should appear immediately.
6. Repeat with idle Wi-Fi and with normal local traffic. Run at least ten acquisitions and several ten-minute steady runs. Preserve logs and configuration, count failures, and report distributions instead of a single best run.

### Physical accuracy measurement

The above procedure verifies connectivity and recovery but cannot establish absolute time error: the two host monotonic clocks have no independent shared epoch, and network-estimator diagnostics are not ground truth. Do not claim physical-network p95 accuracy from the printed offset or labels.

To measure accuracy, independently calibrate both hosts' monotonic clocks against the same reference (for example a captured common timing pulse or a calibrated external timing instrument), and record the timestamp of each reader evaluation together with its unwrapped Q32 position. The measurement apparatus must have known uncertainty substantially below 2 ms. Compare B's evaluated timeline to A's reference trajectory at the same independently determined instant, convert frame error to milliseconds using the exact rational format, and compute median/p95/max separately for acquisition, steady playback, explicit changes, and recovery. Report the reference method and its uncertainty, path direction, sample count, drift, delay/loss, and any rejected measurements. Source/device output latency must be measured separately from library clock-map error.

No two-computer or instrumented physical-network measurement has been performed as part of local implementation. Actual LTC I/O and hardware timestamp support remain follow-up work.

## Local result, 2026-09-19

Validated on macOS arm64 with Rust 1.98.1, Buf 1.73.0, and protoc-gen-doc 1.5.1. Formatting, Clippy, workspace tests/doctests, Rustdoc, schema lint/build/format, generated-reference consistency, all examples, and the explicit mDNS smoke test passed. The suite has 27 automatic tests including the doctest, plus one explicit multicast test.

The 80-second symmetric-LAN simulations use seeds 7, 19, and 997, each at -200, 0, and +200 ppm. They deliver replies only after simulated t4, switch from 20 Hz acquisition to 4 Hz steady probes, and evaluate clock-map error every 10 ms, including intervals between probes. All nine runs acquired in 0.56 seconds. P95 error after two seconds ranged from 0.333 to 0.585 ms. These are deterministic software simulation results only.

## WASM portability, 2026-09-20

The shared clock, timeline, and source-tracking tests now live in `tidkod-protocol`; the native
client reuses those modules. `clients/wasm` depends only on that protocol crate and binding/serialization
libraries. It builds for `wasm32-unknown-unknown`. The web pnpm tests execute the generated WASM in
Node, covering protocol fixtures, malformed/version/size rejection, signed extrapolation, controls,
scheduled pause, stale revisions/sessions, source health, reconnect, and indefinite holdover.

`native/tests/webtransport.rs` exercises a real HTTP/3 connection to the native leader on its existing
UDP listener, validating state, private request/reply datagrams, and correct/incorrect pin behavior.
This is a Rust HTTP/3 client test, not a browser test. See [the browser test procedure](../web/README.md).
No browser was available through the browser automation runtime during this implementation; live
browser interoperation and its manual acceptance checklist remain unverified.

The compiled-WASM simulation also exercises the complete follower (including timeline correction)
for 20 seconds with 1–5 ms per-direction delay, +200 ppm drift, and 1% loss. Seed 19 acquires at
660 ms with 1.360 ms p95 timecode error after acquisition. This is simulated ground truth, not
a browser or physical-network measurement. Native formatting, Clippy, tests/doctests, Rustdoc,
schema checks, generated-reference consistency, all three example smoke tests, and the explicit
mDNS test also pass after the extraction.

## Investigation of a follower appearing ahead, 2026-09-20

A regression test exposed a real startup issue: with identical clock epochs, a first probe
whose request path is delayed by 40 ms creates a +20 ms provisional offset. Later symmetric
probes converge to the correct clock mapping, but the old implementation preserved the
provisional timeline error with its 0.03 frames/second slew limit (1 ms/second at 30 fps).
At two seconds, the clock error was 0.000 ms while timecode remained 18.052 ms ahead, with
`Synchronized` status and 1.112 ms **clock** uncertainty. Thus clock uncertainty alone did
not bound the displayed timecode error. This is a reproducible defect, not a conclusion
inferred from a screenshot.

The follower now applies acquisition mappings directly, through the first converged sample,
and only then uses the steady-state slew limiter. The regression case now has 0.000 ms
clock and timecode error. Compiled-WASM tests cover initial request and reply delays with
a negative -83,877 ms epoch offset, matching the sign/magnitude seen in the reported UI.
`Status::correction_frames` and the browser's Timeline adjustment field expose the remaining
intentional adjustment separately from clock uncertainty.

`cd web && pnpm measure` supplies an independent same-instant test. A native fixture creates
a real leader and a pinned HTTP/3 connection; compiled WASM in Node receives the wire bytes
and creates probes. A separate IPC calibration channel intersects send/receive bounds to
map Node monotonic time into the leader epoch; it does not use the NTP estimator or assume
symmetric IPC delay. The native reader and WASM reader evaluate at the same calibrated
instant. Signed median and absolute p95/max distinguish systematic lead from random jitter.

Measured run after the fix, 600 samples per playback rate:

| Rate | Signed median timecode error | Absolute p95 | Absolute maximum |
| --- | ---: | ---: | ---: |
| +1x | +0.163 ms | 0.238 ms | 0.252 ms |
| -1x | -0.144 ms | 0.196 ms | 0.198 ms |
| +0.5x | +0.112 ms | 0.117 ms | 0.118 ms |
| +2x | +0.429 ms | 0.441 ms | 0.444 ms |
| Paused | 0.000 ms | 0.000 ms | 0.000 ms |

Acquisition took 560 ms. Calibration uncertainty was at most ±0.023 ms; beginning/end
reference offset changed by only 0.002 ms. Timecode errors are expressed in nominal frame
milliseconds, so the +2x error naturally doubles the same clock error. These are local
HTTP/3 + IPC + WASM measurements, not measurements of browser JavaScript transport scheduling
or its compositor. The live browser automation runtime remained unavailable.

`python3 scripts/measure_display.py` separately samples a real leader terminal through a PTY.
One run recorded 261 updates: median sampling interval 18.067 ms, p95 18.467 ms, maximum
19.200 ms. This measures actual timecode sampling cadence, excluding terminal rendering and
OS-compositor latency. Independent display sampling can therefore contribute a visible
frame difference, but this does not establish the cause of the specific screenshot.

The timestamp audit found one application of the clock offset/drift, followed by one anchor
extrapolation; no additional RTT advance is applied. Both native and WASM use the same Q32
arithmetic and label formatting. The reproducible startup defect is fixed; the exact split
between browser timing error and presentation delay in the user's screenshot remains unmeasured.

## Stability follow-up — 2026-09-20

The [stability review](stability-review.md) records additional reproduced failures and fixes.
The suite now includes 30 protocol unit tests, five wire tests, six compiled-WASM tests,
and native integration tests. Formatting, Clippy with warnings denied, workspace tests and
doctests, and the production web build pass. The multicast test remains explicitly opt-in
and was not rerun for this algorithm-only follow-up.

The deterministic symmetric LAN scenarios still acquire in 0.56 seconds, with p95 clock
error below 0.6 ms across the tested seeds and ±200 ppm drifts. The compiled-WASM jitter,
drift, and loss scenario acquires in 660 ms with p95 timecode error 1.360 ms. The new
latency-change regression accepts the changed path after 1.75 seconds at the steady probe
cadence; this is a deterministic result, not a universal recovery deadline.

Latest `pnpm --dir web measure` run after the stability changes, 600 samples per rate:

| Rate | Signed median timecode error | Absolute p95 | Absolute maximum |
| --- | ---: | ---: | ---: |
| +1x | +0.187 ms | 0.248 ms | 0.254 ms |
| -1x | -0.212 ms | 0.237 ms | 0.240 ms |
| +0.5x | +0.111 ms | 0.115 ms | 0.116 ms |
| +2x | +0.450 ms | 0.467 ms | 0.468 ms |
| Paused | 0.000 ms | 0.000 ms | 0.000 ms |

Acquisition took 573 ms. Per-rate independent calibration uncertainty was at most ±0.030 ms;
the final calibration was ±0.042 ms and the reference offset changed by 0.004 ms across the
run. These measurements include real HTTP/3, IPC delivery, and compiled WASM in Node, and
exclude browser scheduling and presentation. The small positive clock bias remains measurable;
the results do not claim zero bias or justify subtracting a fixed offset.

## Deadline/evidence/trace APIs — 2026-09-20

The [timing APIs](timing-apis.md) add six protocol regression tests: deadline prediction across
fractional/reverse/slow/slewed motion, scheduled controls and pause, midnight wrapping, drift-bounded
interval coverage under asymmetry, explicit contradictory evidence and deterministic estimator
replay, and publication-time correlation under response reordering. The suite now has 36 protocol
unit tests and five wire tests. Native integration additionally verifies deadline and diagnostic
availability over real QUIC. The allocation-counting test includes deadline queries; undrained
clock diagnostic events do not block synchronization.

Nine compiled-WASM tests pass, including public deadlines/evidence, timestamp diagnostic values,
exact browser-adapter capture/replay, tamper detection, bounded prefix retention, and disabled
capture. TypeScript and production bundling pass. These capture tests use compiled WASM in Node;
they do not measure live browser callback or compositor behavior.

The real HTTP/3 + IPC + compiled-WASM same-instant test passes after these additions:

| Rate | Signed median timecode error | Absolute p95 | Absolute maximum |
| --- | ---: | ---: | ---: |
| +1x | +0.342 ms | 0.506 ms | 0.517 ms |
| -1x | -0.321 ms | 0.364 ms | 0.366 ms |
| +0.5x | +0.160 ms | 0.182 ms | 0.182 ms |
| +2x | +0.492 ms | 0.529 ms | 0.532 ms |
| Paused | 0.000 ms | 0.000 ms | 0.000 ms |

Acquisition took 569 ms. Maximum per-rate independent calibration uncertainty was ±0.028 ms;
reference offset changed by 0.002 ms during the run. These results remain within the existing
acceptance target, but do not establish an accuracy improvement over previous runs. The point
estimator was intentionally not replaced: interval evidence and tracing provide information
for evaluating future changes.

## User browser trace investigation — 2026-09-20

Two supplied browser captures replayed exactly (1,309 and 1,438 operations, neither truncated).
This establishes reproducibility, **not** independent accuracy. In the first capture, 64 matched
exchanges had path RTT 1.291–3.888 ms; remaining slew was at most 0.078 ms and browser read intervals
reached 22.4 ms. The second had 67 exchanges, RTT p95 3.286 ms (maximum 7.384 ms), at most 0.044 ms
remaining slew, and read intervals up to 22.8 ms. These captures do not contain a wall reference
or pixel presentation timestamps, so they cannot establish the actual display offset.

An independent leader PTY test using `tod` measured median sample age at receipt 0.517 ms,
p95 0.773 ms, maximum 1.191 ms. Sampling intervals were median 17.967 ms, p95 18.333 ms,
maximum 21.533 ms. This supports a fresh leader sample reaching the terminal input; it does
not measure when the terminal renders it. The new browser wall-reference pairing provides
a same-computer TOD accuracy test at sampling time. No fixed visual offset was inferred from
these measurements or applied as compensation.


The subsequent user screenshots include independent same-computer TOD readings of −0.093 ms
and +0.212 ms (approximately 1 ms wall-clock resolution), including a screenshot where the
visible labels differ by one frame. At those sample instants, the browser timeline agrees with
wall time within measurement precision; the visible mismatch is not a frame-sized sampled
clock error.

A concrete terminal scheduling defect was measured and fixed: the old unrelated refresh timer
sampled newly crossed 30 fps frames with median 9.933 ms / p95 17.200 ms / maximum 18.133 ms
lateness. Leader and follower terminals now use the predicted next frame boundary as a wakeup,
while retaining the refresh cap for status/input. The same PTY test then measured median 1.033 ms /
p95 1.100 ms / maximum 1.200 ms frame-crossing sample lateness. Terminal/compositor presentation
latency is excluded from both runs; exact pixel alignment is not claimed. No fixed compensation
was added. Example smoke tests pass after the change.

Presentation prediction is exposed as a developer API in protocol/native/WASM; its tests cover
zero delay, fractional/reverse playback with drift and slew, paused output, overflow, fallback,
future scheduled controls without early latching, and unchanged actual synchronization state.
The native allocation test covers compensated reads. Eleven compiled-WASM/browser-helper tests
pass, including independent TOD arithmetic and capture/replay of compensated reads.

The third user export had replay capture disabled (zero operation records), but retained the
latest 128 clock observations. One 33.591 ms path-RTT outlier was rejected; the stored offset
and drift were unchanged from the preceding accepted observation. The other 127 observations
were accepted; RTT median was 2.087 ms and p95 4.585 ms. No independent wall references were
present in this export. Bounded replay capture is now enabled by default in the web example
(`?trace=0` opts out), avoiding an export that silently lacks the requested replay evidence.

## Ghostty output-path audit

The previous inference that the remaining mismatch required replacing the terminal display was
premature. The renderer itself lacked DEC mode 2026 synchronized output and erased changed rows
before rewriting them. [Ghostty's application-developer guidance](https://ghostty.org/docs/help/synchronized-output)
identifies unbracketed redraws as an application-side source of tearing on fast terminals.

Leader and follower now build each update in memory, bracket it with synchronized-output begin/end,
and write/flush it under one stdout lock. Changed rows are overwritten before clearing their unused
trailing cells. Shutdown defensively ends a synchronized update. The PTY measurement checks complete,
non-nested transaction markers in addition to sampling metrics. Formatting, Clippy, example builds,
and cross-process example smoke tests pass. Frame-change sampling lateness remains approximately
1.1 ms p95. These checks validate the emitted output and sampling behavior; they do not establish
that the missing redraw transaction caused the full observed visual gap. Live Ghostty inspection
through Computer Use was blocked by that tool's app safety policy.


## Release pipeline validation, 2026-09-21

After moving the native crate to `native/` and renaming it `tidkod`, local
workspace tests, strict Clippy, Rustdoc, schema checks, the protocol package build,
all example smokes, and all 11 compiled-WASM/web tests passed. README files were
checked against their original hashes and remain unchanged.

Optimized macOS ARM64 native/core archives passed C and C++ static/shared consumer
tests, SwiftPM and Swift dylib consumer tests, dynamic-link inspection, and a
build of their extracted Rust sources. An Ubuntu 22.04 ARM64 container passed the
Rust workspace suite and all 16 native/core static/shared C/C++ consumer tests.
The aggregate Apple SDK built both macOS architectures, iOS ARM64, and the ARM64
simulator slice; an app using the extracted package passed its XCTest loopback
synchronization test on the locally installed iOS 27 simulator. CI targets iOS 26.

A disposable Git repository verified that pinned cargo-release creates one shared
version bump, updates the native protocol dependency, and makes exactly one
version tag, with publication and pushes disabled. Six release-script tests cover
incomplete assets, wrong architectures, debug builds, unsafe archive paths,
checksums, and preservation of already published releases. Workflow syntax was
checked with actionlint. Windows consumers and the remaining desktop architecture
combinations still require their GitHub matrix runs; local checks do not claim
those have passed. Registry publishing remains disabled pending the release
validation and packaging checks in `docs/releasing.md`.

## Synchronization accuracy work, 2026-10-30

### Deterministic impairment matrix

`cargo run --release -p tidkod-protocol --example impairment` drives the shared
`FollowerCore` (estimator, correction and probe cadence) against a ground-truth leader
clock. It runs 300 simulated seconds per scenario, three seeds each, and evaluates
every 10 ms after a 10-second warmup. `protocol/tests/impairment.rs` runs the same matrix
with loose regression bounds.

Scenarios:
- wired queueing;
- loaded wired;
- an unobservable 150 µs stack asymmetry;
- the earlier uniform 1–5 ms LAN model;
- Wi-Fi with heavy tails and burst loss;
- uplink-only Wi-Fi queueing;
- Wi-Fi power save at the leader;
- browser timer quantization with scheduling jitter, coarsened and isolated;
- a 1 ppm/s drift ramp;
- a symmetric 5 ms latency step;
- burst loss.

Every scenario reported an alignment error that covered the actual error at every
evaluation (0% uncovered). None had a hard resync after acquisition.

The estimator previously fitted an unweighted line through the lower half of samples by
delay. It now weights that half by the possible queueing asymmetry `(delay − min)/2`.
The weighting scale is set by the delay spread of the four lowest-delay samples, with a
50 µs floor. Before: core `0.2.0/d8f32b574bc04c83`; after: the final working tree.

| Scenario | Signed median before → after | p99 before → after | Max before → after |
| --- | ---: | ---: | ---: |
| wired | +0.000 ms → +0.000 ms | 0.005 ms → 0.005 ms | 0.008 ms → 0.007 ms |
| wired-loaded | -0.000 ms → +0.000 ms | 0.059 ms → 0.034 ms | 0.074 ms → 0.053 ms |
| wired-stack-asymmetry | +0.075 ms → +0.075 ms | 0.080 ms → 0.080 ms | 0.083 ms → 0.082 ms |
| lan-uniform | +0.004 ms → -0.000 ms | 0.497 ms → 0.392 ms | 0.668 ms → 0.602 ms |
| wifi | +0.000 ms → +0.007 ms | 0.334 ms → 0.192 ms | 0.434 ms → 0.266 ms |
| wifi-uplink-queue | +0.195 ms → +0.047 ms | 0.325 ms → 0.132 ms | 0.373 ms → 0.181 ms |
| wifi-leader-power-save | -0.054 ms → -0.015 ms | 0.349 ms → 0.189 ms | 0.474 ms → 0.290 ms |
| browser | +0.006 ms → +0.019 ms | 0.054 ms → 0.051 ms | 0.070 ms → 0.061 ms |
| browser-isolated | — → -0.007 ms | — → 0.043 ms | — → 0.055 ms |
| wired-drift-ramp | -0.059 ms → -0.059 ms | 0.072 ms → 0.072 ms | 0.078 ms → 0.078 ms |
| wired-latency-step | +0.000 ms → +0.000 ms | 0.005 ms → 0.006 ms | 0.008 ms → 0.082 ms |
| wired-burst-loss | +0.000 ms → +0.000 ms | 0.005 ms → 0.005 ms | 0.007 ms → 0.007 ms |

The stack-asymmetry row is the documented four-timestamp limit; no estimator removes it.
The latency-step maximum rose because of exchanges in flight across the step. They saw
the old delay on one leg and the new delay on the other, a genuine asymmetry. Once the old
low-delay samples expire, those exchanges are the lowest-delay samples left and get more
weight. The resulting transient is under 0.1 ms; p99 is unchanged.
The drift-ramp row is fit lag over the 32-second window. A separate drift horizon,
evaluated as an option, would have increased that lag, and wired noise was already about
5 µs, so it was not adopted. Burst and chained-burst probing were also simulated at equal
and triple traffic. They did not improve Wi-Fi or power-save p99 and worsened browser p99,
so cadence is unchanged. With a 100 µs timer, the lowest-delay samples are partly rounding
luck, which adds about +0.01 ms median; cross-origin isolation removes it.

### Hint-less tracked sources

`protocol/tests/tracking_cadence.rs` replays tracked sources without rate hints:
- 10–240 Hz, uniform timestamp jitter up to 1 ms (4 ms in the characterization sweep);
- ±30% irregular intervals and 2% dropped samples;
- rates 1x, 1.0004x, 0.97x and −1x;
- 1.5-second gaps.

The previous tracker inferred speed from consecutive samples, quantized it to 1e-4, and
turned any confirmed change into a discontinuity:
- 240 Hz with 200 µs jitter: 130 false discontinuities per minute.
- 60 Hz with 1 ms jitter: 30.
- 60 Hz with 200 µs jitter settled on a rate 2,400 ppm off (inferred 1.0024x);
  240 Hz with 1 ms jitter settled about 19,800 ppm off.

After the change there are no discontinuities after initial rate acquisition up to 1 ms
jitter. Tracking error p99 at sample instants falls from roughly the jitter amplitude to
0.035–0.41 ms, depending on cadence. Rate error stays within tens to a few hundred ppm,
resolved to about 1 ppm.

A hinted 20 Hz source with 1% of timestamps displaced by up to ±500 µs went from 56.6 µs
to 9.5 µs p99 trajectory error, through robust rejection; the old tracker measured 43.8 µs.

### Native timestamp placement

`cargo run --release -p tidkod --example asymmetry` splits loopback exchanges into true up and
down legs. Both engines share one host clock, so the true offset is exact. On the development
Mac, which runs the Little Snitch network extension, a UDP `sendmsg` took 0.1–1.4 ms. The probe
left near the end of that call, but t1 was taken before encoding. The follower→leader leg
therefore included the whole send. Measured each exchange ran 160 µs median ahead and the
fitted offset 100–128 µs ahead, while send latency was high. A plain-`std` UDP benchmark
reproduced the send cost outside Tidkod and outside the sandbox. Raising the worker thread
to user-interactive QoS changed nothing, idle or with all 16 cores loaded, so it was not
adopted.

Two changes:
- Native publication now drives the MoQ runtime and the QUIC endpoint through `sendmsg` in
  the same worker pass (`Io::flush`), for both requests and replies.
- `Probes` uses the local publication-return time as the exchange's t1, provided the path
  delay stays nonnegative.

Fitted offset error A/B in alternating runs:

| t1 placement | Run 1 median | Run 2 median |
| --- | ---: | ---: |
| Before send | +42.6 µs | +32.9 µs |
| After send | +4.1 µs | +6.1 µs |

Paired 15 s warmup / 60 s same-instant `accuracy --tracked` runs, two followers, run back to
back with an untouched HEAD build:

| Build | Timeline signed median | Timeline p99 | Timeline max | Clock p99 |
| --- | ---: | ---: | ---: | ---: |
| HEAD `6a308bb`, run 1 | +0.040 / +0.038 ms | 0.054 / 0.051 ms | 0.059 / 0.055 ms | 0.054 / 0.053 ms |
| Working tree, run 1 | −0.002 / −0.003 ms | 0.018 / 0.010 ms | 0.020 / 0.011 ms | 0.017 / 0.008 ms |
| HEAD `6a308bb`, run 2 | +0.038 / +0.038 ms | 0.054 / 0.053 ms | 0.059 / 0.058 ms | 0.054 / 0.053 ms |
| Working tree, run 2 | −0.005 / −0.001 ms | 0.009 / 0.008 ms | 0.010 / 0.009 ms | 0.007 / 0.007 ms |

Earlier in the session, when filtered sends were slower, HEAD showed +0.10 ms bias. The
magnitude of this bias depends on the host's send path. These are loopback results on one
machine, not physical LAN or Wi-Fi measurements.

The leader's t3 is still taken before its send, because the reply carries it. On a host
where the leader's sends are slow, that remains as negative bias. Removing it needs a
two-step or interleaved transmit timestamp, which is a protocol change.

### Browser

`web/vite.config.ts` serves `Cross-Origin-Opener-Policy: same-origin` and
`Cross-Origin-Embedder-Policy: require-corp` in dev and preview. Production hosts must
send the same headers. Browser accuracy results now record `crossOriginIsolated` and the
observed `performance.now()` step: 0.005 ms isolated and 0.100 ms otherwise in headless
Chromium 156. WebTransport connects under COEP. The runner tolerates the execution-context
swap that COOP causes.

60-second `measure:browser --tracked` runs:
- isolated: signed median +0.031 to +0.040 ms, p99 0.047–0.060 ms;
- not isolated: +0.012 to +0.044 ms, p99 0.031–0.067 ms.

The independent reference uncertainty is ±0.05–0.13 ms, so these runs cannot separate the
two configurations; the simulated effect is about 0.01 ms. The remaining browser bias is
consistent with the native finding. Its t1 still precedes the asynchronous `@moq/net` send,
which the page cannot observe without changing that dependency.
