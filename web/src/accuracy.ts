import init, { Follower, core_build_id } from '../pkg/tidkod_wasm';
import { follow } from './transport';

declare global { interface Window { accuracyResult?: unknown; accuracyError?: string } }
const sleep = (ms: number) => new Promise(resolve => setTimeout(resolve, ms));
const options = new URLSearchParams(location.search);
let sequence = 0;
async function command(command: string) {
  const sent = performance.now();
  const response = await fetch('/__accuracy', { method: 'POST', body: `${command.split(' ')[0]} ${++sequence} ${command.split(' ').slice(1).join(' ')}` });
  if (!response.ok) throw Error(await response.text());
  const text = await response.text();
  const received = performance.now();
  return { sent, received, values: text.trim().split(' ').slice(2) };
}
async function calibrate() {
  let low = -Infinity, high = Infinity;
  for (let i = 0; i < 100; i++) {
    const r = await command('CAL');
    const leader = Number(r.values[0]) / 1e6;
    low = Math.max(low, leader - r.received);
    high = Math.min(high, leader - r.sent);
  }
  if (high < low || high - low > 0.5) throw Error(`Independent reference imprecise: [${low}, ${high}] ms`);
  return { offset: (low + high) / 2, uncertainty: (high - low) / 2 };
}
function statistics(values: number[]) {
  const signed = [...values].sort((a, b) => a - b);
  const absolute = values.map(Math.abs).sort((a, b) => a - b);
  return { samples: values.length, medianMs: signed[Math.floor(signed.length / 2)], p99AbsMs: absolute[Math.floor(absolute.length * .99)], maxAbsMs: absolute.at(-1)! };
}
await init();
const core = new Follower();
const stop = new AbortController();
let transportFailure: unknown;
const following = follow(core, new URL(options.get('url')!), options.get('pin')!, stop.signal, d => {
  if (d.message.startsWith('Retrying')) transportFailure = d.message;
}).catch(e => { transportFailure = e; });
try {
  if (options.get('coreBuildId') !== core_build_id()) throw Error('Native and browser core builds differ');
  const warmup = Number(options.get('warmup') ?? 300) * 1000;
  const duration = Number(options.get('seconds') ?? 600) * 1000;
  const began = performance.now();
  while (performance.now() - began < warmup) { core.read(performance.now()); await sleep(20); }
  if (core.read(performance.now()).synchronization !== 'Synchronized') throw Error(`Browser did not acquire: ${transportFailure}`);
  const calibration = await calibrate();
  const browserErrors: number[] = [], nativeErrors: number[] = [], clockErrors: number[] = [];
  let correctionMaxMs = 0, generationChanges = 0, previousGeneration: string | undefined;
  const measured = performance.now();
  while (performance.now() - measured < duration) {
    const now = performance.now();
    const reading = core.read(now);
    const reply = await command(`READ ${Math.round((now + calibration.offset) * 1e6)}`);
    const [frames, subframe, discontinuity, nativeFrames, nativeSubframe] = reply.values;
    if (reading.discontinuity !== discontinuity) throw Error('Reference and browser discontinuities differ');
    const reference = BigInt(frames) * 4294967296n + BigInt(subframe);
    const browserPosition = BigInt(reading.wholeFrames) * 4294967296n + BigInt(reading.subframe);
    const nativePosition = BigInt(nativeFrames) * 4294967296n + BigInt(nativeSubframe);
    browserErrors.push(Number(browserPosition - reference) / 4294967296 / reading.fps * 1000);
    nativeErrors.push(Number(nativePosition - reference) / 4294967296 / reading.fps * 1000);
    clockErrors.push(reading.mappedLeaderMs - now - calibration.offset);
    correctionMaxMs = Math.max(correctionMaxMs, Math.abs(reading.correctionFrames) / reading.fps * 1000);
    if (previousGeneration !== undefined && previousGeneration !== reading.resyncGeneration) generationChanges++;
    previousGeneration = reading.resyncGeneration;
    await sleep(10);
  }
  const final = await calibrate();
  const referenceChange = Math.abs(final.offset - calibration.offset);
  if (referenceChange > calibration.uncertainty + final.uncertainty + .05) throw Error(`Independent reference changed by ${referenceChange}ms`);
  const browser = statistics(browserErrors), native = statistics(nativeErrors);
  // Smallest observed performance.now() increment: ~0.1 ms coarsened, ~0.005 ms isolated.
  let timerStepMs = Infinity;
  for (let previous = performance.now(), end = previous + 5; previous < end;) {
    const next = performance.now();
    if (next > previous) timerStepMs = Math.min(timerStepMs, next - previous);
    previous = next;
  }
  window.accuracyResult = { coreBuildId: core_build_id(), browser, native, clock: statistics(clockErrors), calibration, finalCalibration: final,
    correctionMaxMs, generationChanges, crossOriginIsolated, timerStepMs, userAgent: navigator.userAgent, warmupSeconds: warmup / 1000, seconds: duration / 1000,
    // Include reference uncertainty, rather than allowing it to hide an excursion.
    passed: browser.p99AbsMs + calibration.uncertainty <= 1 && browser.maxAbsMs + calibration.uncertainty <= 2
      && native.p99AbsMs <= 1 && native.maxAbsMs <= 2 };
  document.getElementById('result')!.textContent = JSON.stringify(window.accuracyResult, null, 2);
} catch (e) { window.accuracyError = String(e); }
finally { stop.abort(); await following; core.free(); }
