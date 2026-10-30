# Synchronization stability review — 2026-09-20

This review follows the startup acquisition defect documented in [validation](validation.md).
It exercises the shared protocol crate, which supplies the same estimator and timeline policy
to native and WASM clients. These are implemented changes, not proposed algorithms.

| Failure reproduced | Change and regression coverage |
| --- | --- |
| A sustained latency increase or clock step remains rejected by the old fit | Quarantine consistent outliers; replace stale history after eight consistent observations spanning at least 500 ms. The steady 4 Hz latency-step test begins accepting the new path after 1.75 seconds. (Later tightened for bufferbloat: elevated-delay regimes need a two-second run with a settled delay floor; see [protocol](protocol.md).) Isolated, alternating, and reordered outliers cannot establish a replacement. |
| A 30-second leader stall appears fresh after subtracting leader processing time | Limit the complete probe exchange to 500 ms; check freshness in both probe matching and clock estimation. Invalid replies do not consume an otherwise valid outstanding request. |
| Delay jitter over a short baseline produces a drift estimate | Require eight selected low-delay observations spanning two seconds before fitting drift. Prefer newer observations when delays tie. |
| Old low-delay support makes uncertainty too optimistic | Age each sample's asymmetry bound independently and add regression residual. A changing-asymmetry regression previously had 4.101 ms error with 1.154 ms uncertainty; it now reports 4.304 ms uncertainty for the same error. |
| Alternating positive/negative phase errors trigger a hard resync | Require three large errors in the same direction; reset evidence at controls and reconnects. Clock reacquisition cannot reuse the initial-startup alignment shortcut. |
| Slew can reverse extremely slow playback or move paused output | Limit slew to the smaller of the configured cap and 10% of playback velocity. Paused output holds its position. |
| A higher revision can undo an applied discontinuity | Validate transitions after latching already-applied schedules. Reject discontinuity regression and frame-format changes without a new discontinuity. |
| Tracked input loses tiny explicit speed hints or treats alternating jitter as a jump | Respect exact rational hints; require consistent position-error direction and clear evidence after discontinuities. |

Probe decoding also rejects partially filled reply timestamps. Native and WASM defaults now
both enter holdover after two seconds without an accepted exchange. No protobuf fields or
protocol version changed; the [specification](protocol.md) defines the stricter v1 semantics.

## Tradeoffs and limits

Recovery is deliberately bounded but not instantaneous. Eight coherent outliers provide evidence
for a changed regime; changing asymmetry can also supply that evidence. Four timestamps cannot
uniquely distinguish clock offset from unequal one-way delays. The uncertainty fix reports this
ambiguity rather than claiming to remove it. Aging confidence follows the general dispersion
principle in [RFC 5905's clock filter](https://www.rfc-editor.org/rfc/rfc5905.html#section-10);
the particular thresholds here are Tidkod policy, not NTP compliance.

Uncertainty grows at 1000 ppm, conservatively allowing separation between a fitted drift within
±500 ppm and an actual relative rate within ±500 ppm. This is an engineering estimate under those
assumptions, not a guarantee for arbitrary clock jumps, suspend behavior, or timestamp defects.
Clock uncertainty excludes intentional timeline adjustment, tracked-source error, and display
latency. Remaining timeline adjustment is separately exposed in status.

Very slow playback corrects more slowly to preserve direction. Small residual adjustments while
paused remain pending instead of moving stopped timecode; an explicit change or confirmed large
resync can still reposition it. This favors stable transport behavior over invisible convergence.

## Validation and next investigations

The new regressions cover the concrete failures above, including compiled-WASM recovery after a
path change and clock step. The complete native suite, allocation-free reader test, pinned raw
QUIC/HTTP3 integration tests, compiled-WASM tests, and real same-instant measurement pass. See
[validation results and reproduction commands](validation.md) for measurements and their limits.

The next useful work is measurement-driven:

1. **Implemented in the follow-up:** capture a bounded, exportable trace of browser probe timestamps, accepted/rejected samples,
   state revisions, mapping updates, and animation timestamps. Replay it through the deterministic
   core to separate transport scheduling, estimator error, correction lag, and display sampling.
   See [timing APIs](timing-apis.md); actual compositor timing is still unobserved.
2. Compare main-thread operation with a Dedicated Worker under deliberate rendering and GC load.
   Establish the worker/window timestamp relationship explicitly. Moving work alone does not
   establish accuracy; compare against an independent clock reference.
3. Measure tracked-source timestamp precision and noise before adding protocol quality metadata.
   Source uncertainty and clock uncertainty are different quantities. Nanosecond wire units do
   not promise nanosecond timestamp accuracy. Any future schema fields need fixtures and defined
   behavior for clients that do not supply them.

Adaptive probing based on uncertainty is another candidate, but should follow these traces rather
than merely increase traffic. Physical Wi-Fi accuracy and browser presentation latency remain
separate measurements; neither is established by loopback or Node/WASM tests.
