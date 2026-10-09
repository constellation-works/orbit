'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

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
