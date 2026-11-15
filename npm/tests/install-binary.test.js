'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const http = require('node:http');
const crypto = require('node:crypto');
const { execFile, execFileSync } = require('node:child_process');
const { promisify } = require('node:util');

const { installBinary } = require('../scripts/install-binary.js');

function fixture(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'orbit-npm-install-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const source = path.join(root, 'source-orbit');
  fs.writeFileSync(source, 'complete binary');
  const binDir = path.join(root, 'binaries');
  fs.mkdirSync(binDir);
  return { source, binDir, dest: path.join(binDir, 'orbit') };
}

// Simulates ENOSPC: the copy writes part of the file, then throws.
function partialCopyFs(failure) {
  return {
    ...fs,
    copyFileSync(from, to) {
      fs.writeFileSync(to, fs.readFileSync(from).subarray(0, 4));
      throw failure;
    },
    chmodSync: fs.chmodSync,
    renameSync: fs.renameSync,
    rmSync: fs.rmSync,
  };
}

test('successful install leaves an executable binary and no temp files', (t) => {
  const { source, binDir, dest } = fixture(t);
  installBinary(source, dest);
  assert.equal(fs.readFileSync(dest, 'utf8'), 'complete binary');
  assert.equal(fs.statSync(dest).mode & 0o777, 0o755);
  assert.deepEqual(fs.readdirSync(binDir), ['orbit']);
});

test('failed copy leaves the final path absent so the shim retries the download', (t) => {
  const { source, binDir, dest } = fixture(t);
  const failure = Object.assign(new Error('no space left on device'), { code: 'ENOSPC' });
  assert.throws(() => installBinary(source, dest, partialCopyFs(failure)), failure);
  assert.equal(fs.existsSync(dest), false, 'a partial copy must not appear at the final path');
  assert.deepEqual(fs.readdirSync(binDir), [], 'the temp file must be cleaned up');
});

test('failed copy keeps a previously installed binary intact', (t) => {
  const { source, binDir, dest } = fixture(t);
  fs.writeFileSync(dest, 'previous binary');
  assert.throws(() => installBinary(source, dest, partialCopyFs(new Error('boom'))), /boom/);
  assert.equal(fs.readFileSync(dest, 'utf8'), 'previous binary');
  assert.deepEqual(fs.readdirSync(binDir), ['orbit']);
});

test('failed rename leaves the final path absent and removes the temp file', (t) => {
  const { source, binDir, dest } = fixture(t);
  const failingRename = {
    ...fs,
    renameSync() {
      throw new Error('rename failed');
    },
  };
  assert.throws(() => installBinary(source, dest, failingRename), /rename failed/);
  assert.equal(fs.existsSync(dest), false);
  assert.deepEqual(fs.readdirSync(binDir), []);
});

// Exercise the real postinstall entrypoint against loopback without adding a
// production download/trust bypass. Only transport and backoff time are faked.
async function releaseFixture(t, { failures = [], fallback = false, corrupt = '', token = '' } = {}) {
  const scratch = path.resolve(__dirname, '../../.orbit/tmp');
  fs.mkdirSync(scratch, { recursive: true });
  const root = fs.mkdtempSync(path.join(scratch, 'npm-download-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.mkdirSync(path.join(root, 'scripts'));
  fs.copyFileSync(path.resolve(__dirname, '../scripts/install-binary.js'), path.join(root, 'scripts/install-binary.js'));
  const pkg = require('../package.json');
  fs.writeFileSync(path.join(root, 'package.json'), JSON.stringify(pkg));
  fs.writeFileSync(path.join(root, 'orbit'), 'fixture binary');
  const target = {
    'darwin-arm64': 'aarch64-apple-darwin', 'darwin-x64': 'x86_64-apple-darwin',
    'linux-arm64': 'aarch64-unknown-linux-gnu', 'linux-x64': 'x86_64-unknown-linux-gnu',
  }[`${process.platform}-${process.arch}`];
  const asset = `orbit-${target}.tar.gz`;
  const archivePath = path.join(root, asset);
  execFileSync('tar', ['-czf', archivePath, '-C', root, 'orbit']);
  const archive = fs.readFileSync(archivePath);
  const checksums = Buffer.from(`${crypto.createHash('sha256').update(archive).digest('hex')}  ${asset}\n`);
  const { privateKey, publicKey } = crypto.generateKeyPairSync('rsa', { modulusLength: 2048 });
  fs.writeFileSync(path.join(root, 'key.pub'), publicKey.export({ type: 'spki', format: 'pem' }));
  const signature = crypto.sign('RSA-SHA256', checksums, privateKey);
  const assets = new Map([
    ['orbit-checksums.txt', checksums], ['orbit-checksums.txt.sig', signature], [asset, archive],
  ]);
  if (corrupt === 'signature') assets.set('orbit-checksums.txt.sig', Buffer.alloc(signature.length));
  if (corrupt === 'archive') assets.set(asset, Buffer.from('tampered archive'));
  const requests = [];
  const counts = new Map();
  const names = [...assets.keys()];
  const server = http.createServer((req, res) => {
    requests.push({ url: req.url, headers: req.headers });
    const count = counts.get(req.url) || 0;
    counts.set(req.url, count + 1);
    if (req.url.startsWith('/github.com/')) {
      const failure = fallback ? 504 : failures[count];
      if (failure === 'network') return req.socket.destroy();
      if (failure) { res.writeHead(failure); return res.end('release unavailable'); }
      return res.end(assets.get(decodeURIComponent(req.url.split('/').pop())));
    }
    if (req.url === `/api.github.com/repos/${pkg.config.orbit.binaryRepo}/releases/tags/v${pkg.version}`) {
      return res.end(JSON.stringify({ assets: names.map((name, i) => ({ name, id: i + 1 })) }));
    }
    if (req.url.startsWith(`/api.github.com/repos/${pkg.config.orbit.binaryRepo}/releases/assets/`)) {
      const id = req.url.split('/').pop();
      res.writeHead(302, { location: `https://release-assets.githubusercontent.com/${id}` });
      return res.end();
    }
    if (req.url.startsWith('/release-assets.githubusercontent.com/')) {
      return res.end(assets.get(names[Number(req.url.split('/').pop()) - 1]));
    }
    res.writeHead(404); res.end('unexpected URL');
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  t.after(() => new Promise((resolve) => server.close(resolve)));
  fs.writeFileSync(path.join(root, 'transport.cjs'), `
const https = require('node:https');
const http = require('node:http');
const fs = require('node:fs');
https.get = (url, options, cb) => {
  const parsed = new URL(url);
  return http.get('http://127.0.0.1:${server.address().port}/' + parsed.hostname + parsed.pathname,
    options, cb);
};
const originalTimeout = global.setTimeout;
global.setTimeout = (fn, delay, ...args) => {
  if ([4000, 8000, 16000].includes(delay)) {
    fs.appendFileSync(${JSON.stringify(path.join(root, 'delays'))}, delay + '\\n');
    return originalTimeout(fn, 0, ...args);
  }
  return originalTimeout(fn, delay, ...args);
};
`);
  const env = {
    ...process.env, HOME: root, USERPROFILE: root, TMPDIR: root,
    ORBIT_SKIP_DOWNLOAD: '', ORBIT_BINARY: '', NODE_OPTIONS: '',
    ORBIT_RELEASE_TRUSTED_KEYS_FILE: '',
    ORBIT_RELEASE_PUBLIC_KEY_FILE: path.join(root, 'key.pub'),
    ORBIT_RELEASE_PUBLIC_KEY_FILE_ACKNOWLEDGE_TRUST_CHANGE: '1',
    GITHUB_TOKEN: token, GH_TOKEN: '',
  };
  let outcome;
  try {
    outcome = await promisify(execFile)(process.execPath,
      ['--require', path.join(root, 'transport.cjs'), path.join(root, 'scripts/install-binary.js')],
      { env, timeout: 15000 });
  } catch (err) { outcome = err; }
  return { root, requests, counts, outcome, asset };
}

test('postinstall retries transient HTTP and network failures before installing', async (t) => {
  const { root, counts, outcome } = await releaseFixture(t, { failures: [504, 429, 'network'] });
  assert.equal(outcome.code, undefined, outcome.stderr);
  assert.equal(fs.readFileSync(path.join(root, 'binaries/orbit'), 'utf8'), 'fixture binary');
  assert.deepEqual([...counts.values()], [4, 4, 4]);
  assert.deepEqual(fs.readFileSync(path.join(root, 'delays'), 'utf8').trim().split('\n').map(Number),
    [4000, 8000, 16000, 4000, 8000, 16000, 4000, 8000, 16000]);
});

test('postinstall falls back after exhausted retries and confines API auth to the API host', async (t) => {
  const { root, requests, outcome } = await releaseFixture(t, { fallback: true, token: 'fixture-token' });
  assert.equal(outcome.code, undefined, outcome.stderr);
  assert.equal(fs.readFileSync(path.join(root, 'binaries/orbit'), 'utf8'), 'fixture binary');
  assert.equal(requests.filter(r => r.url.startsWith('/github.com/')).length, 12);
  const api = requests.filter(r => r.url.startsWith('/api.github.com/'));
  assert.equal(api.length, 6);
  for (const req of api) {
    assert.equal(req.headers.authorization, 'Bearer fixture-token');
    assert.equal(req.headers.accept, req.url.includes('/assets/') ? 'application/octet-stream' : 'application/vnd.github+json');
  }
  for (const req of requests.filter(r => !r.url.startsWith('/api.github.com/'))) {
    assert.equal(req.headers.authorization, undefined, 'credentials must not follow asset redirects');
  }
});

test('postinstall fails immediately on permanent HTTP errors', async (t) => {
  const { root, requests, outcome } = await releaseFixture(t, { failures: [404] });
  assert.equal(outcome.code, 1);
  assert.match(outcome.stderr, /HTTP 404/);
  assert.equal(requests.length, 1);
  assert.equal(fs.existsSync(path.join(root, 'binaries/orbit')), false);
});

for (const fallback of [false, true]) {
  for (const corrupt of ['signature', 'archive']) {
    test(`postinstall rejects corrupt ${corrupt} via ${fallback ? 'API fallback' : 'release URL'}`, async (t) => {
      const { root, outcome } = await releaseFixture(t, { fallback, corrupt });
      assert.equal(outcome.code, 1);
      assert.match(outcome.stderr, corrupt === 'signature' ? /signature verification failed/ : /checksum mismatch/);
      assert.equal(fs.existsSync(path.join(root, 'binaries/orbit')), false);
    });
  }
}
