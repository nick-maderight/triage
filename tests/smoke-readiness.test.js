const assert = require('node:assert/strict');
const { spawn, spawnSync } = require('node:child_process');
const fs = require('node:fs');
const net = require('node:net');
const os = require('node:os');
const path = require('node:path');
const test = require('node:test');

const smokeScript = path.join(__dirname, 'smoke.sh');
const foreignListenerTest = path.join(__dirname, 'smoke_rejects_foreign_listener.sh');

test('foreign-listener shell fixture rejects unrelated CLI calls', () => {
  const result = spawnSync('/bin/sh', [foreignListenerTest], {
    cwd: __dirname,
    encoding: 'utf8',
  });

  assert.equal(result.status, 0, result.stderr || result.error?.message || 'foreign-listener smoke fixture failed');
});

async function startStalledEndpoint() {
  const sockets = new Set();
  const server = net.createServer((socket) => {
    sockets.add(socket);
    socket.on('error', () => {});
    socket.on('close', () => sockets.delete(socket));
  });

  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });

  return {
    port: server.address().port,
    close: async () => {
      for (const socket of sockets) socket.destroy();
      await new Promise((resolve) => server.close(resolve));
    },
  };
}

function runSmoke(env) {
  return new Promise((resolve) => {
    const child = spawn('/bin/sh', [smokeScript], {
      cwd: __dirname,
      detached: true,
      env,
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let stdout = '';
    let stderr = '';
    let timedOut = false;
    let killTimer;
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', (chunk) => { stdout += chunk; });
    child.stderr.on('data', (chunk) => { stderr += chunk; });
    child.on('error', (error) => { stderr += error.message; });
    const deadline = setTimeout(() => {
      timedOut = true;
      try { process.kill(-child.pid, 'SIGTERM'); } catch {}
      killTimer = setTimeout(() => {
        try { process.kill(-child.pid, 'SIGKILL'); } catch {}
      }, 250);
    }, 2500);
    child.on('close', (code, signal) => {
      clearTimeout(deadline);
      clearTimeout(killTimer);
      resolve({ code, signal, stderr, stdout, timedOut });
    });
  });
}

// `cargo test --release` leaves only the release binary, so fall back to it before `cargo build`.
function defaultTriageBin() {
  const debugBin = path.resolve(__dirname, '../target/debug/triage');
  return fs.existsSync(debugBin) ? debugBin : path.resolve(__dirname, '../target/release/triage');
}

test('readiness probe fails within its curl deadline', async () => {
  const tempBase = process.env.TMPDIR || os.tmpdir();
  fs.mkdirSync(tempBase, { recursive: true });
  const scratch = fs.mkdtempSync(path.join(tempBase, 'triage-smoke-readiness.'));
  const bin = path.join(scratch, 'bin');
  fs.mkdirSync(bin);
  const pidFile = path.join(scratch, 'server.pid');
  const mockLog = path.join(scratch, 'triage.log');
  const triage = path.join(bin, 'triage');
  const ss = path.join(bin, 'ss');
  let endpoint;

  try {
    endpoint = await startStalledEndpoint();
    fs.writeFileSync(triage, [
      '#!/bin/sh',
      'if [ "${1-}" = serve ]; then',
      '  printf "%s\\n" "$*" >> "$MOCK_LOG"',
      '  printf "%s\\n" "$$" > "$MOCK_SERVER_PID_FILE"',
      '  exec /bin/sleep 30',
      'fi',
      'printf "%s\\n" "$*" >> "$MOCK_LOG"',
      'exit 97',
      '',
    ].join('\n'), { mode: 0o755 });
    fs.writeFileSync(ss, [
      '#!/bin/sh',
      'attempt=0',
      'while [ ! -s "$MOCK_SERVER_PID_FILE" ]; do',
      '  attempt=$((attempt + 1))',
      '  [ "$attempt" -lt 50 ] || exit 1',
      '  /bin/sleep 0.01',
      'done',
      'pid=$(cat "$MOCK_SERVER_PID_FILE")',
      'printf "LISTEN 0 128 127.0.0.1:%s 0.0.0.0:* users:((\\"triage\\",pid=%s,fd=3))\\n" "$TRIAGE_SMOKE_PORT" "$pid"',
      '',
    ].join('\n'), { mode: 0o755 });

    const result = await runSmoke({
      ...process.env,
      HOME: scratch,
      PATH: `${bin}:${process.env.PATH || '/usr/bin:/bin'}`,
      TMPDIR: scratch,
      TRIAGE_BIN: triage,
      TRIAGE_SMOKE_ATTEMPTS: '1',
      TRIAGE_SMOKE_PORT: String(endpoint.port),
      MOCK_LOG: mockLog,
      MOCK_SERVER_PID_FILE: pidFile,
      XDG_CONFIG_HOME: path.join(scratch, 'xdg-config'),
      XDG_DATA_HOME: path.join(scratch, 'xdg-data'),
      XDG_STATE_HOME: path.join(scratch, 'xdg-state'),
    });

    assert.equal(result.timedOut, false, 'readiness probe hung past the bounded wrapper deadline');
    assert.equal(result.signal, null, 'smoke process exited by signal');
    assert.equal(result.code, 1, 'smoke process did not take the bounded readiness failure path');
    assert.match(result.stderr, /server did not become ready/);
  } finally {
    if (endpoint) await endpoint.close();
    fs.rmSync(scratch, { recursive: true, force: true });
  }
});

test('smoke script completes the qualified Finding skip path', async () => {
  const tempBase = process.env.TMPDIR || os.tmpdir();
  fs.mkdirSync(tempBase, { recursive: true });
  const scratch = fs.mkdtempSync(path.join(tempBase, 'triage-smoke-qualified-id.'));
  const reservation = net.createServer();

  try {
    await new Promise((resolve, reject) => {
      reservation.once('error', reject);
      reservation.listen(0, '127.0.0.1', resolve);
    });
    const port = reservation.address().port;
    await new Promise((resolve, reject) => {
      reservation.close((error) => error ? reject(error) : resolve());
    });

    const result = await runSmoke({
      ...process.env,
      HOME: path.join(scratch, 'home'),
      TMPDIR: scratch,
      TRIAGE_BIN: process.env.TRIAGE_BIN || defaultTriageBin(),
      TRIAGE_SMOKE_PORT: String(port),
      XDG_CONFIG_HOME: path.join(scratch, 'xdg-config'),
      XDG_DATA_HOME: path.join(scratch, 'xdg-data'),
      XDG_STATE_HOME: path.join(scratch, 'xdg-state'),
    });

    const failure = `stderr:\n${result.stderr}`;
    assert.equal(result.timedOut, false, failure);
    assert.equal(result.signal, null, failure);
    assert.equal(result.code, 0, failure);
  } finally {
    if (reservation.listening) {
      await new Promise((resolve) => reservation.close(resolve));
    }
    fs.rmSync(scratch, { recursive: true, force: true });
  }
});
