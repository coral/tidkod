// Actual Chromium WebTransport, independently calibrated through a separate pipe.
// No Playwright dependency, existing browser profile, or production service needed.
import { spawn, spawnSync } from 'node:child_process';
import { createInterface } from 'node:readline';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createServer } from 'vite';

const root = fileURLToPath(new URL('../..', import.meta.url));
const option = (key, fallback) => { const at = process.argv.indexOf(key); return at < 0 ? fallback : process.argv[at + 1]; };
const warmup = Number(option('--warmup-seconds', 300));
const seconds = Number(option('--seconds', 600));
if (!Number.isFinite(warmup + seconds) || warmup < 0 || seconds <= 0) throw Error('Invalid measurement duration');
const build = spawnSync('cargo', ['build', '--release', '--locked', '-p', 'tidkod-wasm', '--example', 'accuracy_peer'], { cwd: root, stdio: 'inherit' });
if (build.status !== 0) process.exit(build.status ?? 1);
const peer = spawn(join(root, 'target/release/examples/accuracy_peer'), ['--direct', ...(process.argv.includes('--tracked') ? ['--tracked'] : [])]);
peer.stderr.pipe(process.stderr);
const pending = new Map();
let readyResolve, readyReject;
const ready = new Promise((resolve, reject) => { readyResolve = resolve; readyReject = reject; });
const lines = createInterface({ input: peer.stdout });
lines.on('line', line => {
  const [kind, id, pin, coreBuildId] = line.split(' ');
  if (kind === 'READY') readyResolve({ address: id, pin, coreBuildId });
  else if (pending.has(id)) { const done = pending.get(id); pending.delete(id); done(line); }
});
peer.on('error', readyReject);
peer.on('exit', code => { readyReject(Error(`Fixture exited ${code}`)); for (const done of pending.values()) done(undefined); pending.clear(); });
const profile = await mkdtemp(join(tmpdir(), 'tidkod-accuracy-'));
let vite, chrome, socket;
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
try {
  const startup = setTimeout(() => readyReject(Error('Fixture startup timeout')), 15000);
  const endpoint = await ready.finally(() => clearTimeout(startup));
  // Never reload the measuring page when a concurrent test rebuilds WASM.
  vite = await createServer({ root: join(root, 'web'), server: { host: '127.0.0.1', port: 0, hmr: false, watch: null }, plugins: [{
    name: 'independent-timing-reference', configureServer(server) {
      server.middlewares.use('/__accuracy', (request, response) => {
        if (request.method !== 'POST') { response.writeHead(405).end(); return; }
        let command = '';
        request.on('data', chunk => { command += chunk; if (command.length > 128) request.destroy(); });
        request.on('end', () => {
          if (!/^(CAL \d+\s*|READ \d+ \d+)$/.test(command)) { response.writeHead(400).end('Invalid reference command'); return; }
          const id = command.split(' ')[1];
          const timeout = setTimeout(() => { pending.delete(id); response.writeHead(504).end('Reference timed out'); }, 3000);
          pending.set(id, line => { clearTimeout(timeout); if (line === undefined) response.writeHead(502).end('Fixture exited'); else response.end(line); });
          peer.stdin.write(`${command}\n`);
        });
      });
    },
  }] });
  await vite.listen();
  const page = new URL('/accuracy.html', vite.resolvedUrls.local[0]);
  page.search = new URLSearchParams({ url: `https://${endpoint.address}/`, pin: endpoint.pin, coreBuildId: endpoint.coreBuildId, warmup: String(warmup), seconds: String(seconds) }).toString();
  const executable = process.env.CHROME_PATH ?? (process.platform === 'darwin' ? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome' : 'chromium');
  chrome = spawn(executable, ['--headless=new', '--remote-debugging-port=0', `--user-data-dir=${profile}`, '--no-first-run',
    '--no-default-browser-check', '--disable-background-networking', '--disable-background-timer-throttling', '--disable-renderer-backgrounding', 'about:blank'], { stdio: 'ignore' });
  let launchError;
  chrome.on('error', error => { launchError = error; });
  let port;
  for (let i = 0; i < 150; i++) {
    if (launchError) throw launchError;
    try { port = (await readFile(join(profile, 'DevToolsActivePort'), 'utf8')).split('\n')[0]; break; } catch { await pause(100); }
  }
  if (!port) throw Error('Chrome debugging endpoint unavailable; set CHROME_PATH');
  const target = await (await fetch(`http://127.0.0.1:${port}/json/new?${encodeURIComponent(page.href)}`, { method: 'PUT' })).json();
  socket = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => { socket.addEventListener('open', resolve, { once: true }); socket.addEventListener('error', reject, { once: true }); });
  let serial = 0;
  const calls = new Map();
  socket.addEventListener('message', event => { const message = JSON.parse(event.data); if (calls.has(message.id)) { calls.get(message.id)(message); calls.delete(message.id); } });
  async function evaluate(expression) {
    const id = ++serial;
    const reply = new Promise(resolve => calls.set(id, resolve));
    socket.send(JSON.stringify({ id, method: 'Runtime.evaluate', params: { expression, returnByValue: true } }));
    const message = await Promise.race([reply, pause(5000).then(() => { throw Error('Chrome evaluation timed out'); })]);
    // Cross-origin isolation swaps the page into a new process; until it commits there is no context.
    if (message.error?.message === 'Cannot find default execution context') return undefined;
    if (message.error || message.result.exceptionDetails) throw Error(JSON.stringify(message));
    return message.result.result.value;
  }
  const began = Date.now();
  let nextReport = 0;
  for (;;) {
    const elapsed = (Date.now() - began) / 1000;
    if (elapsed > warmup + seconds + 60) throw Error('Browser accuracy run timed out');
    const status = await evaluate('({ result: window.accuracyResult, error: window.accuracyError })') ?? {};
    if (status.error) throw Error(status.error);
    if (status.result) {
      console.log(JSON.stringify({ ...status.result, tracked: process.argv.includes('--tracked'),
        revision: spawnSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).stdout.trim(), workingTree: 'local build' }, null, 2));
      if (!status.result.passed) process.exitCode = 1;
      break;
    }
    if (elapsed >= nextReport) { console.error(`Browser accuracy elapsed=${elapsed.toFixed(0)}s warmup=${warmup}s duration=${seconds}s`); nextReport += 30; }
    await pause(1000);
  }
} finally {
  socket?.close();
  if (chrome && chrome.exitCode === null) {
    const closed = new Promise(resolve => chrome.once('exit', resolve)); chrome.kill();
    await Promise.race([closed, pause(5000)]);
    if (chrome.exitCode === null) chrome.kill('SIGKILL');
  }
  await vite?.close();
  peer.stdin.end('QUIT\n');
  const closed = new Promise(resolve => peer.once('exit', resolve));
  await Promise.race([closed, pause(3000)]);
  if (peer.exitCode === null) peer.kill();
  lines.close();
  await rm(profile, { recursive: true, force: true });
}
