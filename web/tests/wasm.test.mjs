import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { Follower, PresentationClock, core_build_id } from '../pkg-node/tidkod_wasm.js';

// Tiny fixture encoder, not production protocol code. Uses the authoritative schema's tags.
const v = n => { n = BigInt(n); const b = []; do { b.push(Number(n & 127n) | (n > 127n ? 128 : 0)); n >>= 7n; } while(n); return b; };
const uint = (tag, n) => [...v(tag * 8), ...v(n)];
const bytes = (tag, b) => [...v(tag * 8 + 2), ...v(b.length), ...b];
const sint = n => { n = BigInt(n); return n >= 0n ? 2n * n : -2n * n - 1n; };
const anchor = (time, frames, speed) => [...uint(1, time), ...bytes(2, uint(1, sint(frames))), ...bytes(3, [...uint(1, sint(speed)), ...uint(2, 1)])];
function state({rev = 1, disc = 0, session = 1, time = 0, frames = 0, speed = 1, scheduled = [], tracked = false, degraded = false} = {}) {
  return Uint8Array.from([...uint(1, 1), ...bytes(2, Array(16).fill(session)), ...uint(3, rev), ...uint(4, disc), ...uint(5, tracked ? 2 : 1), ...uint(6, degraded ? 2 : 1), ...bytes(7, [...uint(1,30), ...uint(2,1)]), ...bytes(8, anchor(time, frames, speed)), ...scheduled.flatMap(s => bytes(9, [...uint(1,s.disc), ...bytes(2, anchor(s.time,s.frames,s.speed))]))]);
}
function parseProbe(buf) {
  let i = 0; const result = {};
  const read = () => { let n=0n, shift=0n, b; do { b=buf[i++]; n|=BigInt(b & 127)<<shift; shift+=7n; } while(b & 128); return n; };
  while(i < buf.length) { const tag=Number(read()) >> 3; result[tag]=read(); }
  return result;
}
function sync(f, offset = 5000, start = 1000) {
  for(let i = 0; i < 25; i++) {
    const send = start + i*50;
    const p = parseProbe(f.probe(send));
    const reply = Uint8Array.from([...uint(1,1), ...uint(2,p[2]), ...uint(3,p[3]), ...uint(4,BigInt(Math.round((send+offset+2)*1e6))), ...uint(5,BigInt(Math.round((send+offset+2)*1e6)))]);
    f.reply(reply, send+4);
  }
  return start + 1204;
}
test('steady-state correction, quality and sample output use the shared core', () => {
  const f = new Follower();
  let snapshot;
  try {
    f.connected(); f.snapshot(state(), 0);
    const now = sync(f);
    const before = f.read(now);
    f.snapshot(state({ rev: 2, time: 20_000_000 }), now);
    assert.equal(f.read(now).frames, before.frames);
    assert.equal(f.read(now).aligned, false);
    assert.ok(f.read(now).alignmentErrorMs >= 20);
    assert.equal(f.probe_interval_at(now), 50);
    for (let i = 1; i <= 10; i++) f.snapshot(state({ rev: i + 2, time: 20_000_000 }), now + i * 100);
    const reading = f.read(now + 1000);
    assert.ok(Math.abs(reading.correctionFrames) / 30 * 1000 < 1);
    assert.equal(reading.resyncGeneration, before.resyncGeneration);
    snapshot = f.capture_snapshot();
    assert.deepEqual(snapshot.read_sample(now, 48000n, 48000), snapshot.read(now + 1000));
    assert.throws(() => snapshot.read_sample(now, 1n, 0));
    assert.match(core_build_id(), /^\d+\.\d+\.\d+\/[0-9a-f]{16}$/);
  } finally { snapshot?.free(); f.free(); }
});
test('browser presentation estimator resets on gaps and never targets a past deadline', () => {
  const clock = new PresentationClock();
  try {
    assert.equal(clock.target(0, 1).estimated, false);
    let target;
    for (let i = 1; i <= 16; i++) target = clock.target(i * 16, i * 16 + 1);
    assert.equal(target.localMs, 272);
    assert.equal(target.refreshMs, 16);
    assert.equal(target.estimated, true);
    target = clock.target(272, 300);
    assert.equal(target.localMs, 300);
    assert.equal(target.missed, true);
    assert.equal(clock.target(2000, 2001).estimated, false);
    assert.throws(() => clock.target(NaN, 0));
    clock.reset();
    assert.equal(clock.target(2016, 2017).estimated, false);
  } finally { clock.free(); }
});
test('actual WASM decodes golden fixture, validates version, size, timestamps', () => {
  const f = new Follower();
  try {
    assert.equal(f.read(0).label,'00:00:00:00');
    assert.equal(f.read(0).synchronization,'Uninitialized');
    f.connected();
    const golden = Buffer.from(readFileSync(new URL('../../protocol/fixtures/paused.hex',import.meta.url),'utf8').trim(),'hex');
    f.snapshot(golden,1);
    const time = sync(f);
    assert.equal(f.read(time).synchronization,'Synchronized');
    assert.equal(f.read(time).speed,0);
    assert.equal(f.probe_interval_ms(),250);
    const bad = golden.slice(); bad[1]=2;
    assert.throws(()=>f.snapshot(bad,time), /version 2/);
    assert.throws(()=>f.snapshot(new Uint8Array(513),time), /512/);
    assert.throws(()=>f.snapshot(new Uint8Array([128]),time));
    assert.throws(()=>f.probe(NaN), /timestamp/);
    assert.throws(()=>f.read(-1), /timestamp/);
    assert.throws(()=>f.reply(new Uint8Array(513),time), /512/);
  } finally { f.free(); }
});

test('WASM counts acquisitions independently of retained history', () => {
  const f = new Follower();
  try {
    assert.equal(f.read(0).acceptedObservations, '0');
    f.connected(); f.snapshot(state(), 1);
    let now = sync(f);
    assert.equal(f.read(now).acceptedObservations, '25');
    for (let i=0; i<8; i++) now = sync(f, 5000, 3000+i*1300);
    assert.equal(f.read(now).acceptedObservations, '225');
    assert.equal(f.clock_trace().length, 128);
    assert.equal(f.clock_trace().at(-1).acceptedObservations, '225');
    assert.equal(f.read(now).offsetEvidence.samples, 128);
    f.disconnected();
    assert.equal(f.read(now+5000).acceptedObservations, '225');
    f.connected(); f.snapshot(state({rev:2}), now+5001);
    assert.equal(f.read(now+5001).acceptedObservations, '225');
    f.connected(); f.snapshot(state({session:2}), now+5002);
    assert.equal(f.read(now+5002).acceptedObservations, '0');
  } finally { f.free(); }
});

test('WASM frozen snapshots predict without mutating or retaining their follower', () => {
  const f = new Follower();
  f.connected(); f.snapshot(state({speed:-1}), 1);
  const now = sync(f);
  const at = BigInt(Math.round((now+5000+50)*1e6));
  f.snapshot(state({rev:2,speed:-1,scheduled:[{disc:1,time:at,frames:42,speed:0}]}),now);
  const reading = f.read(now);
  const frozen = f.capture_snapshot();
  try {
    assert.deepEqual(frozen.read(now), reading);
    assert.deepEqual(frozen.next_boundary(now), f.next_boundary(now));
    assert.equal(frozen.read_for_presentation(now,100).frames,42);
    assert.deepEqual(f.read(now), reading);
    assert.throws(()=>frozen.read(NaN), /timestamp/);
    assert.throws(()=>frozen.read_for_presentation(now,-1), /timestamp/);
    assert.throws(()=>frozen.read_for_presentation(9_223_372_036_854,1), /timestamp/);
    f.disconnected();
    assert.equal(f.read(now).connection,'Disconnected');
    f.free();
    assert.deepEqual(frozen.read(now), reading);
    assert.equal(frozen.read(now+100).frames,42);
  } finally { frozen.free(); }
});
test('WASM extrapolates signed motion, rejects stale state, latches scheduled control and holds indefinitely', () => {
  const f = new Follower();
  try {
    f.connected(); f.snapshot(state(),1); const now = sync(f);
    const r = f.read(now);
    assert.ok(Math.abs(r.frames - (now+5000)*.03) < .0001);
    f.snapshot(state({rev:2,disc:1,time:8_000_000_000,frames:900,speed:-2,scheduled:[{disc:2,time:9_000_000_000,frames:1200,speed:0}]}),now);
    assert.ok(Math.abs(f.read(3500).frames-870)<.0001);
    f.snapshot(state({rev:1}),3501);
    f.snapshot(state({session:2,rev:50}),3502);
    assert.ok(Math.abs(f.read(3503).frames-869.82)<.0001);
    const stopped=f.read(4500); assert.equal(stopped.frames,1200); assert.equal(stopped.speed,0);
    f.disconnected(); const later=f.read(86_400_000);
    assert.equal(later.frames,1200); assert.equal(later.synchronization,'Holdover');
    assert.ok(later.uncertaintyMs > stopped.uncertaintyMs);
    f.connected(); f.snapshot(state({session:2,speed:-1,tracked:true,degraded:true}),86_400_001);
    assert.equal(f.read(86_400_002).synchronization,'Uninitialized');
    sync(f, 1000, 86_400_010);
    const restart=f.read(86_402_000);
    assert.equal(restart.source,'Tracked'); assert.equal(restart.health,'Degraded');
    f.disconnected();
    assert.ok(f.read(86_403_000).frames < restart.frames);
  } finally { f.free(); }
});
test('replayed and unsolicited probes cannot move the clock mapping', () => {
  const f=new Follower();
  try {
    f.connected(); f.snapshot(state(),0); const now=sync(f);
    const before=f.read(now);
    const unknown=Uint8Array.from([...uint(1,1),...uint(2,1000),...uint(3,1),...uint(4,8e9),...uint(5,8e9)]);
    f.reply(unknown,now+1);
    assert.equal(f.read(now).offsetMs,before.offsetMs);
  } finally { f.free(); }
});
test('compiled WASM acquires with LAN jitter, drift and loss using the shared estimator', () => {
  const f = new Follower();
  let seed=19, pending=[], next=100, acquired;
  const random=()=> { seed=(Math.imul(seed,1664525)+1013904223)>>>0; return seed/2**32; };
  const leader=t=>5000+t*1.0002;
  const errors=[];
  try {
    f.connected(); f.snapshot(state(),0);
    for(let now=0; now<=20_000; now+=10) {
      pending.sort((a,b)=>a.at-b.at);
      while(pending.length && pending[0].at <= now) {
        const p=pending.shift(); f.reply(p.bytes,p.at);
      }
      const r=f.read(now);
      if(r.synchronization==='Synchronized' && acquired===undefined) acquired=now;
      if(now>=2000) errors.push(Math.abs(r.frames/30*1000-leader(now)));
      if(now>=next) {
        const p=parseProbe(f.probe(now));
        const up=1+random()*4, down=1+random()*4;
        if(random()>=.01) pending.push({at:now+up+down,bytes:Uint8Array.from([...uint(1,1),...uint(2,p[2]),...uint(3,p[3]),...uint(4,Math.round(leader(now+up)*1e6)),...uint(5,Math.round(leader(now+up)*1e6))])});
        next=now+f.probe_interval_ms();
      }
    }
    errors.sort((a,b)=>a-b);
    const p95=errors[Math.floor(errors.length*.95)];
    assert.ok(acquired<2000,`acquisition ${acquired}ms`);
    assert.ok(p95<=2,`p95 ${p95}ms`);
    console.log(`WASM simulation: acquired ${acquired}ms, p95 ${p95.toFixed(3)}ms`);
  } finally { f.free(); }
});
test('startup delay does not survive acquisition as a hidden slew, including a negative clock epoch offset', () => {
  for (const requestDelayed of [true,false]) {
    const f=new Follower();
    try {
      f.connected(); f.snapshot(state(),100_000);
      for(let i=0;i<40;i++) {
        const send=100_000+i*50, offset=-83_877;
        const up=i===0 && requestDelayed ? 41 : 1;
        const down=i===0 && !requestDelayed ? 41 : 1;
        const p=parseProbe(f.probe(send));
        const reply=Uint8Array.from([...uint(1,1),...uint(2,p[2]),...uint(3,p[3]),...uint(4,Math.round((send+offset+up)*1e6)),...uint(5,Math.round((send+offset+up)*1e6))]);
        f.reply(reply,send+up+down);
      }
      const now=102_000, r=f.read(now);
      assert.equal(r.synchronization,'Synchronized');
      assert.ok(Math.abs(r.mappedLeaderMs-(now-83_877))<.001);
      assert.ok(Math.abs(r.frames/30*1000-(now-83_877))<.001);
      assert.ok(Math.abs(r.correctionFrames)<.000001);
    } finally {f.free();}
  }
});
test('WASM recovers from sustained path/clock changes and rejects a stalled exchange', () => {
  const f=new Follower();
  try {
    f.connected(); f.snapshot(state(),0); sync(f);
    let now;
    for(const [start,offset] of [[2500,5000],[9000,5100]]) {
      for(let i=0;i<24;i++) {
        const send=start+i*250,p=parseProbe(f.probe(send));
        const reply=Uint8Array.from([...uint(1,1),...uint(2,p[2]),...uint(3,p[3]),...uint(4,(send+offset+30)*1e6),...uint(5,(send+offset+30)*1e6)]);
        now=send+60;f.reply(reply,now);
      }
      const r=f.read(now);
      assert.equal(r.synchronization,'Synchronized');
      assert.ok(Math.abs(r.mappedLeaderMs-(now+offset))<.1);
      assert.ok(Math.abs(r.frames/30*1000-(now+offset))<.1);
    }
    const send=now+250,p=parseProbe(f.probe(send));
    const stalled=Uint8Array.from([...uint(1,1),...uint(2,p[2]),...uint(3,p[3]),...uint(4,(send+5101)*1e6),...uint(5,(send+5101+30_000)*1e6)]);
    f.reply(stalled,send+30_002);
    assert.equal(f.read(send+30_002).synchronization,'Holdover');
  } finally {f.free();}
});

test('WASM predicts deadlines and exposes interval evidence and timestamp diagnostics', () => {
  const f = new Follower();
  assert.equal(f.next_boundary(0), undefined);
  f.connected(); f.snapshot(state(), 1000);
  const now = sync(f);
  const r = f.read(now);
  assert.ok(r.offsetEvidence.consistent);
  assert.ok(r.offsetEvidence.lowerMs <= 5000 && r.offsetEvidence.upperMs >= 5000);
  const b = f.next_boundary(now);
  assert.equal(b.kind, 'Frame');
  assert.ok(b.localDeadlineMs > now);
  assert.ok(f.read(b.localDeadlineMs).frames >= Number(b.frames));
  const send = now + 10;
  const p = parseProbe(f.probe(send));
  f.probe_published(send + 0.125);
  const reply = Uint8Array.from([...uint(1,1), ...uint(2,p[2]), ...uint(3,p[3]), ...uint(4,BigInt(Math.round((send+5002)*1e6))), ...uint(5,BigInt(Math.round((send+5002)*1e6)))]);
  f.reply_timed(reply, send+4, send+4.25);
  const trace = f.clock_trace();
  assert.equal(trace.at(-1).publicationNs, '125000');
  assert.equal(trace.at(-1).processingDelayNs, '250000');
  assert.equal(trace.at(-1).t4, String(Math.round((send+4)*1e6)));
  assert.throws(() => f.reply_timed(reply, send+4, send+3));
  f.snapshot(state({rev:2,disc:1,time:BigInt(Math.round((send+5004)*1e6)),frames:42,speed:0}),send+4);
  assert.equal(f.next_boundary(send+4), undefined);
  f.free();
});

test('browser trace capture can be replayed exactly including publication and rendering', async () => {
  const { instrument } = await import('../src/trace.ts');
  const { replay } = await import('../scripts/replay-trace.mjs');
  const capture = instrument(new Follower(), true);
  const f = capture.follower;
  f.read(0); f.connecting(); f.connected(); f.snapshot(state(), 1000);
  const now = sync(f);
  f.read(now); f.read_for_presentation(now,33); f.next_boundary(now); f.disconnected(); f.read(now+30_000);
  const exported = JSON.parse(JSON.stringify(capture.export()));
  assert.equal(replay(exported).operations, exported.records.length);
  exported.records.find(r => r.method === 'probe').result.bytes[0] ^= 1;
  assert.throws(() => replay(exported), /Result at operation/);
  f.free();
});

test('capture retains a bounded replayable prefix and can be disabled', async () => {
  const { instrument } = await import('../src/trace.ts');
  const { replay } = await import('../scripts/replay-trace.mjs');
  const capture = instrument(new Follower(), true);
  for (let i=0; i<16400; i++) capture.follower.read(i);
  const data=JSON.parse(JSON.stringify(capture.export()));
  assert.equal(data.records.length, 16384);
  assert.equal(data.omitted, 16);
  assert.equal(data.prefixComplete, false);
  assert.equal(replay(data).operations,16384);
  capture.follower.free();
  const raw=new Follower();
  const disabled=instrument(raw,false);
  assert.equal(disabled.follower,raw);
  disabled.follower.read(0);
  assert.equal(disabled.export().records.length,0);
  raw.free();
});

test('presentation compensation predicts full trajectory without latching future controls', () => {
  const f = new Follower();
  f.connected(); f.snapshot(state(),1000);
  const now=sync(f);
  assert.deepEqual(f.read_for_presentation(now,0),f.read(now));
  const raw=f.read(now), future=f.read_for_presentation(now,50);
  assert.ok(Math.abs(future.frames-raw.frames-1.5)<.00001);
  const changeAt=Math.round((now+5000+20)*1e6);
  f.snapshot(state({rev:2,scheduled:[{disc:1,time:changeAt,frames:100,speed:-2}]}),now);
  const predicted=f.read_for_presentation(now,50);
  assert.equal(predicted.discontinuity,'1');
  assert.equal(predicted.speed,-2);
  assert.ok(Math.abs(predicted.frames-98.2)<.00001);
  assert.equal(f.read(now).discontinuity,'0');
  assert.equal(f.read(now).speed,1);
  f.read_for_presentation(now,5000);
  assert.equal(f.read(now).synchronization,'Synchronized');
  for(const bad of [-1,NaN,Infinity,1e20]) assert.throws(()=>f.read_for_presentation(now,bad));
  f.snapshot(state({rev:3,disc:2,time:changeAt,frames:12,speed:0}),now+21);
  assert.equal(f.read_for_presentation(now+21,200).frames,12);
  f.free();
});

test('TOD reference wraps midnight, accounts for timezone, and does not round to frames', async () => {
  const {compareTod}=await import('../src/wall-reference.ts');
  const midnight=Date.UTC(2026,8,20);
  assert.equal(compareTod(0,30,midnight+10,0).differenceMs,-10);
  assert.equal(compareTod(30*86400-.3,30,midnight,0).differenceMs,-10);
  assert.ok(Math.abs(compareTod(.03,30,midnight,0).differenceMs-1)<.00001);
  assert.equal(compareTod(30*19800,30,midnight,-330).differenceMs,0);
});

test('recording UUID updates without resetting timing and remains in frozen/holdover readings', () => {
  const f = new Follower();
  let frozen;
  const withId = (rev, id) => Uint8Array.from([...state({rev, speed: 0}), ...bytes(10, id)]);
  try {
    assert.equal(f.read(0).sessionId, undefined);
    f.connected();
    f.snapshot(withId(1, Array(16).fill(0x12)), 1);
    const now = sync(f);
    const before = f.read(now);
    assert.equal(before.sessionId, '12121212-1212-1212-1212-121212121212');
    frozen = f.capture_snapshot();
    f.snapshot(withId(2, Array(16).fill(0x34)), now);
    const after = f.read(now);
    assert.equal(after.sessionId, '34343434-3434-3434-3434-343434343434');
    assert.equal(after.acceptedObservations, before.acceptedObservations);
    assert.equal(after.synchronization, 'Synchronized');
    assert.equal(after.discontinuity, before.discontinuity);
    assert.equal(after.frames, before.frames);
    f.snapshot(withId(1, Array(16).fill(0x12)), now); // stale state cannot roll back the ID
    assert.equal(f.read(now).sessionId, after.sessionId);
    assert.equal(frozen.read(now).sessionId, before.sessionId);
    assert.equal(f.read(now + 10000).sessionId, after.sessionId);
    assert.throws(() => f.snapshot(withId(3, [1, 2]), now + 10000), /session ID/);
  } finally { frozen?.free(); f.free(); }
});

test('LTC reusable samples, exact metadata, tracking and snapshot output', async () => {
  const {LtcEncoder,LtcDecoder,LtcInput,LtcOutput,LtcSampleBuffer,ltc_memory}=await import('../pkg-node/tidkod_wasm.js');
  const buffer=new LtcSampleBuffer(48000);
  const encoder=new LtcEncoder(30000,1001,true,48000,1797n);
  const decoder=new LtcDecoder(30000,1001,true,48000);
  const input=new LtcInput(30000,1001,true,48000);
  const output=new LtcOutput(30000,1001,true,48000);
  let snapshot;
  try {
    encoder.set_metadata(0x87654321,5,true);
    assert.equal(encoder.render(buffer,48000),2);
    const samples=new Float32Array(ltc_memory().buffer,buffer.pointer(),buffer.capacity());
    assert.equal(Math.abs(samples[100]),0.5);
    for (const reader of [decoder,input]) {
      let offset=0,frames=0;
      while(offset<48000) {
        const used=reader.process(buffer,offset,48000-offset,BigInt(offset),1000000000n+BigInt(offset)*1000000000n/48000n);
        assert.ok(used>0);offset+=used;
        const result=reader.result();
        if(result.has_frame){frames++;assert.equal(result.user_bits,0x87654321);assert.equal(result.binary_group_flags,5);assert.ok(result.polarity_valid);assert.ok(result.end_ns>result.start_ns);}
        result.free();
      }
      assert.ok(frames>=25);
    }
    const reading=input.read(2000);assert.equal(reading.health,'Healthy');assert.ok(BigInt(reading.wholeFrames)>=1825n);
    snapshot=input.capture_snapshot(2000);
    assert.equal(output.render(snapshot,buffer,48000,48000n,2000000000n),2);
    assert.equal(decoder.process(buffer,48001,1,0n,0n),0);assert.equal(decoder.state(),5);
    assert.throws(()=>new LtcEncoder(60,1,false,48000,0n));
  } finally {snapshot?.free();output.free();input.free();decoder.free();encoder.free();buffer.free();}
});
