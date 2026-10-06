# Improvement backlog

Repository review: 2026-10-06. Based on source, tests, workflows, and existing
validation reports; this review did not run new timing measurements or builds.

Priorities: **P1** = correctness, compatibility, or missing validation of an
important contract; **P2** = robustness, performance, and usability; **P3** =
optional expansion after the foundations are measured. Items marked
**investigation** need reproduction or measurement before implementation.

Existing strengths to preserve: shared native/WASM timing logic, exact rational
rates and signed Q32 positions, precision correction, adaptive probe cadence,
holdover, scheduled controls, bounded queues, allocation-free readers, golden
wire fixtures, and archived native/core SDK consumer tests. These already exist.

## Synchronization

- [ ] **P1 — Establish physical LAN acceptance results.** Existing reports cover
  loopback, simulations, and browser reference measurements; they do not establish
  accuracy between physical machines. Run the independently calibrated two-host
  procedure on wired LAN and separately on Wi-Fi. Record build identity, reference
  uncertainty, signed median, p95/p99/max, acquisition failures, and recovery times.
  Done when reproducible reports evaluate the documented wired p99 <= 2 ms and
  maximum <= 5 ms targets without treating estimator offset as ground truth.
  References: [validation](docs/validation.md), [acceptance](docs/synchronization-acceptance.md).

- [ ] **P1 — Add end-to-end suspend/resume and clock-domain recovery coverage.**
  **Investigation:** estimator step tests and presentation-gap resets do not prove
  application recovery after OS sleep. Exercise native sleep/wake and browser
  suspension during playback, pause, reverse, and pending schedules. Verify stale
  confidence expires, output bridges are refreshed, controls are not applied twice,
  and reacquisition follows the documented policy. Preserve failing traces.
  References: [clock](protocol/src/clock.rs), [timeline](protocol/src/timeline.rs),
  [output](protocol/src/output.rs), [browser lifecycle](web/src/main.ts).

- [ ] **P2 — Measure estimator behavior across a broader impairment matrix.**
  Extend existing recovery regressions with seeded burst loss, correlated jitter,
  changing one-way asymmetry, drift ramps, and queued observations. Report actual
  error versus reported uncertainty, time outside the alignment budget, recovery
  time, and false hard resyncs. Tune policy only against reproducible failures;
  four timestamps cannot identify arbitrary path asymmetry.
  References: [clock](protocol/src/clock.rs), [alignment tests](protocol/tests/alignment.rs).

- [ ] **P2 — Measure adaptive-probe scaling.** Fast/steady probing already exists.
  Benchmark simultaneous acquisition and recovery with increasing follower counts,
  recording traffic, worker latency, and alignment. Evaluate cadence staggering
  only if synchronized bursts cause a measurable problem; preserve recovery's
  minimum evidence span. References: [network](native/src/network.rs),
  [worker load tests](native/tests/poll_worker.rs).

## Tracking

- [ ] **P1 — Characterize tracking across source sample cadences.** The tracker
  uses three-sample confirmation and a fixed per-sample position correction, so
  response time depends on arrival cadence. Replay equivalent trajectories at
  multiple cadences, including irregular intervals and dropped samples; measure
  settling time, false discontinuities, and inferred-rate error. Use the results
  to decide whether confirmation and smoothing should use elapsed time.
  References: [tracker](protocol/src/tracking.rs), [tracked example](native/examples/tracked.rs).

- [ ] **P1 — Specify and test recovery after source loss.** **Investigation:**
  `Tracker::health` degrades stale input, while rate inference retains the previous
  sample across a gap. Cover resumed continuous motion, a stopped source, seeks,
  direction changes, and stale-but-monotonic samples. Define when old rate evidence
  should be discarded and assert health, discontinuity, and first recovered output.
  References: [tracker](protocol/src/tracking.rs), [sample handling](native/src/network.rs).

- [ ] **P2 — Expose source-quality diagnostics separately from clock quality.**
  Measure source sample age, cadence jitter, residual position error, and inferred
  versus explicit rate before introducing new wire fields. Provide bounded local
  diagnostics first. Done when a capture can distinguish noisy input timestamps
  from network clock error without folding both into clock uncertainty.
  References: [stability review](docs/stability-review.md), [tracker](protocol/src/tracking.rs).

- [ ] **P3 — Add a real external-source adapter.** Start with one concrete LTC or
  MIDI timecode input, timestamped in the engine clock domain. Keep device decoding
  outside the shared tracker; document timestamp placement, midnight unwrapping,
  discontinuities, and input loss. Validate against captured fixtures and an actual
  source before claiming device accuracy. References: [SourceSample API](native/src/api.rs),
  [validation limits](docs/validation.md).

## Bindings and foreign API

- [ ] **P1 — Add an explicit foreign ABI compatibility handshake.** `Reading`
  contains evolving by-value C fields, and the docs require matching consumers and
  libraries. The protocol core build ID does not describe the entire binding ABI.
  Generate an ABI version/layout identity and check it before wrapper calls that
  write records. Test deliberate header/library and native/core mismatches, plus
  size/alignment/field offsets across supported architectures.
  References: [API source](clients/bindings/src/api.rs), [generator](clients/bindings/build.rs),
  [SDK tests](scripts/sdk.py).

- [ ] **P1 — Expand direct C ABI failure-path tests.** Existing facade tests and
  consumer smoke tests are a useful base. Exercise documented null handling,
  invalid UTF-8, invalid byte spans, output/error ownership, and panic-to-status
  conversion through exported symbols. Run sanitizer-backed valid-ownership
  scenarios; do not assume arbitrary invalid pointers can be safely validated.
  References: [C support](clients/bindings/src/c_support.rs),
  [generated entry points](clients/bindings/build.rs), [facade tests](clients/bindings/tests/facade.rs).

- [ ] **P2 — Make correction-policy configuration consistent across languages.**
  Defaults use precision correction, while foreign explicit fixed-slew constructors
  select the legacy policy. Expose clearly named precision/legacy options and the
  supported policy fields without silently changing existing constructors. Verify
  equivalent traces through Rust, C, C++, Swift, C#, and WASM where supported.
  References: [foreign configuration](clients/bindings/src/api.rs),
  [WASM API](clients/wasm/src/lib.rs), [timing contract](docs/synchronization-acceptance.md).

- [ ] **P2 — Broaden managed-wrapper lifecycle validation.** C# already protects
  native lifetime and rejects overlapping calls with `HandleLease`. Add targeted
  disposal/overlap stress tests and run .NET consumers on Linux and macOS as well
  as Windows. Preserve independent readers and allocation-free successful read
  calls. References: [C# support](clients/bindings/templates/Tidkod.Support.cs),
  [smoke consumer](clients/csharp/Smoke/Program.cs), [SDK CI](.github/workflows/native.yml).

## Browser and WASM

- [ ] **P1 — Enforce state-frame limits before browser-side assembly.** The adapter
  awaits `group.readFrame()` before passing the payload to WASM; the native path
  checks the declared size before reading the whole frame. Audit the pinned MoQ
  client's receive API and add a bounded read path or upstream support as needed.
  Test declared oversize frames, endless partial frames, and replacement by a newer
  independent group while clock replies remain serviceable.
  References: [browser transport](web/src/transport.ts), [native equivalent](native/src/network.rs),
  [wire limits](docs/protocol.md).

- [ ] **P1 — Exercise browser orchestration in CI.** Current web CI runs compiled
  WASM tests and a production build; the real Chromium WebTransport measurement is
  a separate command. Add a short real-browser integration job covering pinned
  connection, invalid pin, reconnect, cancellation, state timeout, and shutdown.
  Keep long accuracy acceptance runs separate from smoke tests and retain failure
  traces. References: [web CI](.github/workflows/validation.yml),
  [browser runner](web/scripts/measure-browser.mjs), [transport](web/src/transport.ts).

- [ ] **P2 — Compare main-thread and Dedicated Worker operation.**
  **Investigation:** rendering, transport, and WASM currently share the window
  thread. Measure an optional worker implementation under rendering and GC load,
  explicitly calibrating worker/window timestamps and message delays. Adopt it
  only with demonstrated timing or responsiveness benefits.
  References: [browser app](web/src/main.ts), [stability follow-up](docs/stability-review.md).

- [ ] **P2 — Improve retry behavior for repeated short-lived connections.** The
  browser resets backoff immediately on connection; native waits for a longer-lived
  attempt before resetting. Test a server that accepts then immediately closes,
  distinguish terminal configuration errors from transient failures, and add a
  stable-connection reset rule with bounded jitter. Verify cancellation interrupts
  every wait. References: [browser retries](web/src/transport.ts),
  [native retries](native/src/network.rs).

## Networking and discovery

- [ ] **P1 — Automate interface-change and multicast validation on suitable hosts.**
  Existing mDNS and multiple-address tests are intentionally ignored by ordinary
  CI. Add an opt-in environment that exercises interface removal/reappearance,
  IPv6 link-local scope, goodbye/expiry, and discovery alongside active followers.
  Report multicast failures separately from direct QUIC failures.
  References: [mDNS tests](native/tests/mdns.rs), [loopback tests](native/tests/loopback.rs),
  [interface contract](docs/multiple-interfaces.md).

- [ ] **P2 — Add discovery-assisted endpoint recovery as an explicit policy.**
  A follower configuration selects one socket address, even when discovery lists
  several. Support caller-controlled fallback among addresses for the selected
  service, preserving IPv6 scope and explicit pin precedence. Test disappearing
  interfaces and duplicate display names; never silently select a different leader.
  References: [follower configuration](native/src/api.rs), [discovery](native/src/discovery.rs).

- [ ] **P2 — Bound discovery poll work under event floods.** `Discovery::poll`
  drains the available event queue and returns a cloned leader list. Measure large
  discovery bursts, then introduce a per-poll work budget or incremental updates
  if needed. Verify eventual delivery of removals and bounded caller latency.
  Reference: [discovery polling](native/src/discovery.rs).

- [ ] **P2 — Stress aggregate transport resource limits.** Per-connection stream
  limits, operation budgets, follower caps, and cache targets already exist. Test
  many slow peers, incomplete handshakes, stream churn, and sustained datagrams;
  measure total memory, command responsiveness, and healthy-peer probe lateness.
  Add missing aggregate limits only where measured growth or starvation warrants.
  References: [QUIC driver](native/src/transport/quic.rs),
  [HTTP/3 handling](native/src/transport/web.rs), [worker tests](native/tests/poll_worker.rs).

## Output timing and real-time performance

- [ ] **P1 — Measure physical output latency separately from synchronization.**
  Host-clock presentation prediction and headless Chromium do not establish screen
  or DAC alignment. Add an instrumented display/audio procedure with independent
  reference timing, calibrated device latency, and separate reports per output
  path. References: [output contract](docs/synchronization-acceptance.md),
  [display measurement script](scripts/measure_display.py).

- [ ] **P2 — Add reproducible reader and worker performance benchmarks.** Existing
  allocation checks and starvation tests are valuable but do not give a latency
  baseline. Benchmark read, snapshot, sample evaluation, and boundary prediction
  across schedules, reverse playback, correction, and publication contention.
  Record hardware and tail latency; keep platform-sensitive performance gates off
  ordinary shared CI until stable baselines exist.
  References: [real-time tests](native/tests/realtime.rs), [worker](native/src/worker.rs),
  [boundaries](protocol/src/boundary.rs).

- [ ] **P3 — Build an actual audio/LTC output example.** Use the existing sample
  evaluation API with a device-provided host timestamp and cumulative sample
  indices. Cover scheduled changes inside blocks, fractional frame rates, reverse,
  and pause. Keep encoding outside the network reader and measure independent
  audio-clock drift rather than assuming nominal sample rate synchronizes devices.
  Reference: [sample timing](protocol/src/output.rs).

## Diagnostics and observability

- [ ] **P1 — Make dropped diagnostic events observable.** Native `emit` discards
  `try_send` failures on the bounded event queue. Add monotonic dropped-event
  counters available independently of that queue and expose them in foreign APIs.
  Saturation tests should prove reporting remains nonblocking and authoritative
  reading state, including resync generation, remains available.
  References: [event delivery](native/src/api.rs), [foreign events](clients/bindings/src/api.rs).

- [ ] **P2 — Add machine-readable native timing captures.** Native clock events
  exist, while the browser has an export/replay workflow. Provide a bounded native
  capture path containing core identity, input operations, receive/processing
  timestamps, worker timing, and event-drop counts. Replay equivalent inputs
  against the shared core and explicitly mark incomplete captures.
  References: [native events](native/src/api.rs), [native measurements](native/examples/accuracy.rs),
  [browser capture format](web/src/trace.ts).

## Protocol and test coverage

- [ ] **P1 — Extend malformed-message testing into persistent fuzz targets.**
  There is already a random malformed-message test. Add coverage-guided targets
  for protobuf validation, timeline transitions, probe matching, and incremental
  HTTP/3 parsing, seeded with golden fixtures. Assert bounded resource use and
  state invariants as well as absence of panics; retain minimized regressions.
  References: [wire tests](protocol/tests/wire.rs), [probes](protocol/src/probes.rs),
  [HTTP/3 parser](native/src/transport/web.rs).

- [ ] **P2 — Add property-based state-machine and arithmetic tests.** Generate
  sequences of controls, reconnects, revisions, delayed probes, and schedule
  execution. Assert no applied discontinuity regression, no repeated scheduled
  control, and direction-preserving correction. Cross-check timecode arithmetic
  with an independent rational reference around negative Q32 values, drop-frame
  boundaries, and saturation. References: [timeline](protocol/src/timeline.rs),
  [timecode](protocol/src/timecode.rs), [boundaries](protocol/src/boundary.rs).

## Security and certificate lifecycle

- [ ] **P1 — Reconcile certificate lifetime documentation and expose expiry.**
  The protocol document describes a 14-day certificate interval; `tls::server`
  currently sets 13 days. Document the actual policy and expose expiration in leader
  information and the UI. Test expiry/restart behavior and give callers a concrete
  repinning procedure without relaxing verification.
  References: [certificate generation](native/src/transport/tls.rs),
  [protocol documentation](docs/protocol.md), [leader info](native/src/api.rs).
