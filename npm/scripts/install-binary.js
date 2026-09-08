#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const https = require('node:https');
const crypto = require('node:crypto');
const { spawnSync } = require('node:child_process');
const os = require('node:os');

const PKG = require('../package.json');
const BINARY_REPO = PKG.config.orbit.binaryRepo;
const CHECKSUM_FILE = 'orbit-checksums.txt';
const CHECKSUM_SIGNATURE_FILE = 'orbit-checksums.txt.sig';
// npm package version is kept in lockstep with the orbit release tag. `0.3.1`
// → fetches `v0.3.1` from GitHub Releases. The mapping is intentionally strict:
// older orbit binaries can no longer be installed through this package, so the
// trust set only needs to cover the current release.
const BINARY_VERSION = `v${PKG.version}`;
const PKG_ROOT = path.resolve(__dirname, '..');
const BIN_DIR = path.join(PKG_ROOT, 'binaries');
const BIN_PATH = path.join(BIN_DIR, process.platform === 'win32' ? 'orbit.exe' : 'orbit');
const PUBLIC_KEY_OVERRIDE = process.env.ORBIT_RELEASE_PUBLIC_KEY_FILE;
const PUBLIC_KEY_OVERRIDE_ACK_ENV = 'ORBIT_RELEASE_PUBLIC_KEY_FILE_ACKNOWLEDGE_TRUST_CHANGE';
const TRUSTED_KEYS_OVERRIDE = process.env.ORBIT_RELEASE_TRUSTED_KEYS_FILE;
const TRUSTED_KEYS_OVERRIDE_ACK_ENV = 'ORBIT_RELEASE_TRUSTED_KEYS_FILE_ACKNOWLEDGE_TRUST_CHANGE';
const TRUSTED_PUBLIC_KEY_PATH = PUBLIC_KEY_OVERRIDE
  ? path.resolve(PUBLIC_KEY_OVERRIDE)
  : path.join(PKG_ROOT, 'release-signing.pub');
const TRUSTED_KEYS_OVERRIDE_PATH = TRUSTED_KEYS_OVERRIDE ? path.resolve(TRUSTED_KEYS_OVERRIDE) : null;
const TRUSTED_RELEASE_KEYS = Object.freeze([
  Object.freeze({
    id: 'orbit-release-key-3',
    notAfter: '2029-12-31',
    revokedAt: null,
    publicKeyPem: `-----BEGIN PUBLIC KEY-----
MIIBojANBgkqhkiG9w0BAQEFAAOCAY8AMIIBigKCAYEAoQGLKOvvsvXriGIQ0oxA
PcDyVHLM1iqXBCYXg+blQU41haEkG1eYabvDfeGcyGaC4awW7Q2uCZK05+/Hdjpe
cRUVxP+QWKCAHyretQwOsoXzutZjJgId/ZRiUJPS/FeJOSv0xrayaol0tmfeJ4mH
gFseCLq+mIIWIPRvXmYiKaUB//bjF79w/m4VXlyBhfi6n+f6x2UPG+gjjsjwG6mn
Orec31AAFCIIX69YAd21D3MBc4S89/LoYZCq3neDscZ09Y+e6Jg2HpoBstvqSnq/
3s34unLuIRlyB8jyK8CrdzT1E6YVB7+riAjycE9XMlLOQ2xA4tl6CKIx5YTKHyeW
npMLlbzNaVfFT7p3IPTxsoEI0SB3ZtO7/XhzuOvOpklYcqjW2DGw/yzr2epAqHE/
y4rLO3hkxWhxfgF5KPSR2iftc3LMONRGWELK6jpD5KB7No5vwIvjpVPUc5xA45Xw
tT/bo0mm4TvrumxYr1xyEHrdum+ej/WYz/0BZQlwDOtXAgMBAAE=
-----END PUBLIC KEY-----
`,
  }),
]);
let publicKeyOverrideLogged = false;
let trustedKeysOverrideLogged = false;

function log(msg) {
  process.stderr.write(`@orbit-tools/cli: ${msg}\n`);
}

function fail(msg) {
  process.stderr.write(`@orbit-tools/cli: ${msg}\n`);
  process.exit(1);
}

function resolveTarget() {
  const platform = process.platform;
  const arch = process.arch;
  const key = `${platform}-${arch}`;
  const map = {
    'darwin-arm64': 'aarch64-apple-darwin',
    'darwin-x64': 'x86_64-apple-darwin',
    'linux-x64': 'x86_64-unknown-linux-gnu',
    'linux-arm64': 'aarch64-unknown-linux-gnu',
  };
  const target = map[key];
  if (!target) {
    fail(`unsupported platform/arch: ${key}. Supported: ${Object.keys(map).join(', ')}`);
  }
  return target;
}

function fetchBuffer(url, redirectsLeft = 5) {
  return new Promise((resolve, reject) => {
    https
      .get(url, { headers: { 'user-agent': '@orbit-tools/cli installer' } }, (res) => {
        if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          if (redirectsLeft <= 0) return reject(new Error(`too many redirects fetching ${url}`));
          res.resume();
          return resolve(fetchBuffer(res.headers.location, redirectsLeft - 1));
        }
        if (res.statusCode !== 200) {
          res.resume();
          return reject(new Error(`HTTP ${res.statusCode} fetching ${url}`));
        }
        const chunks = [];
        res.on('data', (c) => chunks.push(c));
        res.on('end', () => resolve(Buffer.concat(chunks)));
        res.on('error', reject);
      })
      .on('error', reject);
  });
}

function sha256(buf) {
  return crypto.createHash('sha256').update(buf).digest('hex');
}

function parseChecksums(text) {
  const out = {};
  for (const line of text.split('\n')) {
    const m = line.trim().match(/^([a-f0-9]{64})\s+(\S+)$/i);
    if (m) out[m[2]] = m[1].toLowerCase();
  }
  return out;
}

function acknowledgeTrustedPublicKeyOverride() {
  if (PUBLIC_KEY_OVERRIDE && TRUSTED_KEYS_OVERRIDE) {
    throw new Error('ORBIT_RELEASE_PUBLIC_KEY_FILE and ORBIT_RELEASE_TRUSTED_KEYS_FILE cannot both be set');
  }
  if (!PUBLIC_KEY_OVERRIDE) {
    return;
  }
  if (process.env[PUBLIC_KEY_OVERRIDE_ACK_ENV] !== '1') {
    throw new Error(`ORBIT_RELEASE_PUBLIC_KEY_FILE requires ${PUBLIC_KEY_OVERRIDE_ACK_ENV}=1`);
  }
  if (!publicKeyOverrideLogged) {
    log(`ORBIT_RELEASE_PUBLIC_KEY_FILE=${TRUSTED_PUBLIC_KEY_PATH} set; trusting replacement release signing key`);
    log('ORBIT_RELEASE_PUBLIC_KEY_FILE is deprecated; prefer ORBIT_RELEASE_TRUSTED_KEYS_FILE for the full trust set (key IDs, notAfter, revokedAt)');
    publicKeyOverrideLogged = true;
  }
}

function acknowledgeTrustedKeysOverride() {
  if (PUBLIC_KEY_OVERRIDE && TRUSTED_KEYS_OVERRIDE) {
    throw new Error('ORBIT_RELEASE_PUBLIC_KEY_FILE and ORBIT_RELEASE_TRUSTED_KEYS_FILE cannot both be set');
  }
  if (!TRUSTED_KEYS_OVERRIDE) {
    return;
  }
  if (process.env[TRUSTED_KEYS_OVERRIDE_ACK_ENV] !== '1') {
    throw new Error(`ORBIT_RELEASE_TRUSTED_KEYS_FILE requires ${TRUSTED_KEYS_OVERRIDE_ACK_ENV}=1`);
  }
  if (!trustedKeysOverrideLogged) {
    log(`ORBIT_RELEASE_TRUSTED_KEYS_FILE=${TRUSTED_KEYS_OVERRIDE_PATH} set; trusting replacement release signing key set`);
    trustedKeysOverrideLogged = true;
  }
}

function readTrustedKeysManifest(manifestPath) {
  const manifestDir = path.dirname(manifestPath);
  return fs
    .readFileSync(manifestPath, 'utf8')
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line.length > 0 && !line.startsWith('#'))
    .map((line) => {
      const [id, notAfter, revokedAt, publicKeyPath, ...extra] = line.split('|');
      if (!id || !publicKeyPath || extra.length > 0) {
        throw new Error(`invalid trusted release signing key record: ${line}`);
      }
      const resolvedPublicKeyPath = path.isAbsolute(publicKeyPath)
        ? publicKeyPath
        : path.resolve(manifestDir, publicKeyPath);
      return {
        id,
        notAfter: notAfter || null,
        revokedAt: revokedAt || null,
        publicKeyPem: fs.readFileSync(resolvedPublicKeyPath, 'utf8'),
      };
    });
}

function normalizeTrustedReleaseKeys(trustedKeys) {
  if (typeof trustedKeys === 'string') {
    return [
      {
        id: 'override',
        notAfter: null,
        revokedAt: null,
        publicKeyPem: trustedKeys,
      },
    ];
  }

  if (!Array.isArray(trustedKeys) || trustedKeys.length === 0) {
    throw new Error('at least one trusted release signing key is required');
  }

  return trustedKeys.map((key) => {
    if (!key || !key.id || !key.publicKeyPem) {
      throw new Error('trusted release signing keys require id and publicKeyPem');
    }
    const notAfter = key.notAfter || null;
    const revokedAt = key.revokedAt || null;
    // Mirror the shell-side awk regex (release_date_number) so a malformed
    // override like notAfter: "next month" fails closed instead of silently
    // becoming "never expires" under lexicographic comparison.
    if (notAfter !== null && !/^\d{4}-\d{2}-\d{2}$/.test(notAfter)) {
      throw new Error(`trusted release signing key ${key.id} has invalid notAfter: ${notAfter} (expected YYYY-MM-DD)`);
    }
    if (revokedAt !== null && !/^\d{4}-\d{2}-\d{2}$/.test(revokedAt)) {
      throw new Error(`trusted release signing key ${key.id} has invalid revokedAt: ${revokedAt} (expected YYYY-MM-DD)`);
    }
    return {
      id: key.id,
      notAfter,
      revokedAt,
      publicKeyPem: key.publicKeyPem,
    };
  });
}

function readTrustedReleaseKeys() {
  acknowledgeTrustedPublicKeyOverride();
  acknowledgeTrustedKeysOverride();

  if (PUBLIC_KEY_OVERRIDE) {
    return normalizeTrustedReleaseKeys(fs.readFileSync(TRUSTED_PUBLIC_KEY_PATH, 'utf8'));
  }
  if (TRUSTED_KEYS_OVERRIDE_PATH) {
    return normalizeTrustedReleaseKeys(readTrustedKeysManifest(TRUSTED_KEYS_OVERRIDE_PATH));
  }
  return TRUSTED_RELEASE_KEYS;
}

function releaseDateString(date = new Date()) {
  return date.toISOString().slice(0, 10);
}

function assertTrustedKeyUsable(key, verificationDate = new Date()) {
  if (key.revokedAt) {
    throw new Error(`release checksum signature was made by revoked release signing key ${key.id} (revoked ${key.revokedAt})`);
  }
  if (key.notAfter && releaseDateString(verificationDate) > key.notAfter) {
    throw new Error(`release checksum signature was made by expired release signing key ${key.id} (not_after ${key.notAfter})`);
  }
}

function verifyWithPublicKey(checksumText, signatureBuf, publicKeyPem) {
  const verifier = crypto.createVerify('RSA-SHA256');
  verifier.update(checksumText, 'utf8');
  verifier.end();
  return verifier.verify(publicKeyPem, signatureBuf);
}

function verifyChecksumSignature(
  checksumText,
  signatureBuf,
  trustedKeys = readTrustedReleaseKeys(),
  verificationDate = new Date()
) {
  for (const key of normalizeTrustedReleaseKeys(trustedKeys)) {
    if (verifyWithPublicKey(checksumText, signatureBuf, key.publicKeyPem)) {
      assertTrustedKeyUsable(key, verificationDate);
      log(`authenticated ${CHECKSUM_FILE} with release signing key ${key.id}`);
      return key.id;
    }
  }
  throw new Error(`release checksum signature verification failed for ${CHECKSUM_FILE}: no trusted release signing key matched`);
}

function verifyArchiveChecksum(asset, archiveBuf, checksumText) {
  const checksums = parseChecksums(checksumText);
  const expected = checksums[asset];
  if (!expected) {
    throw new Error(`checksum entry for ${asset} was not found in ${CHECKSUM_FILE}`);
  }
  const actual = sha256(archiveBuf);
  if (actual !== expected) {
    throw new Error(`checksum mismatch for ${asset}: expected ${expected}, got ${actual}`);
  }
}

function runTar(args, options = {}) {
  const result = spawnSync('tar', args, { encoding: 'utf8', ...options });
  if (result.error) {
    throw result.error;
  }
  if (result.status !== 0) {
    const stderr = result.stderr ? `: ${result.stderr.trim()}` : '';
    throw new Error(`tar ${args.join(' ')} failed (status ${result.status})${stderr}`);
  }
  return result.stdout || '';
}

function validateArchiveMemberName(member) {
  if (member === 'orbit') {
    return;
  }
  if (!member || member.startsWith('/') || member.includes('..') || path.isAbsolute(member)) {
    throw new Error(`unsafe release archive member: ${member || '<empty>'}`);
  }
  throw new Error(`unexpected release archive member: ${member}`);
}

function validateArchiveMembers(archivePath) {
  const members = runTar(['-tzf', archivePath])
    .split(/\r?\n/)
    .filter((line) => line.length > 0);
  if (members.length !== 1) {
    throw new Error("release archive must contain only the 'orbit' binary");
  }
  validateArchiveMemberName(members[0]);
}

function validateExtractedBinary(extractedBinary) {
  const stat = fs.lstatSync(extractedBinary);
  if (stat.isSymbolicLink()) {
    throw new Error('release archive member is a symlink: orbit');
  }
  if (!stat.isFile()) {
    throw new Error('release archive member must be a regular file: orbit');
  }
  if (stat.nlink !== 1) {
    throw new Error('release archive member must not be a hard link: orbit');
  }
}

function extractTarGz(archivePath, destDir) {
  validateArchiveMembers(archivePath);
  runTar(['-xzf', archivePath, '-C', destDir, 'orbit'], { stdio: 'inherit' });
  validateExtractedBinary(path.join(destDir, 'orbit'));
}

async function main() {
  if (process.env.ORBIT_SKIP_DOWNLOAD === '1') {
    log('ORBIT_SKIP_DOWNLOAD=1 set; skipping binary download.');
    return;
  }
  if (process.env.ORBIT_BINARY) {
    log(`ORBIT_BINARY=${process.env.ORBIT_BINARY} set; skipping download (bin shim will use it directly).`);
    return;
  }

  const target = resolveTarget();
  const asset = `orbit-${target}.tar.gz`;
  const baseUrl = `https://github.com/${BINARY_REPO}/releases/download/${BINARY_VERSION}`;
  const archiveUrl = `${baseUrl}/${asset}`;
  const checksumUrl = `${baseUrl}/${CHECKSUM_FILE}`;
  const checksumSignatureUrl = `${baseUrl}/${CHECKSUM_SIGNATURE_FILE}`;

  log(`installing orbit ${BINARY_VERSION} for ${target}...`);

  fs.mkdirSync(BIN_DIR, { recursive: true });

  let checksumText;
  let signatureBuf;
  try {
    checksumText = (await fetchBuffer(checksumUrl)).toString('utf8');
    signatureBuf = await fetchBuffer(checksumSignatureUrl);
    verifyChecksumSignature(checksumText, signatureBuf);
  } catch (err) {
    fail(`could not authenticate ${CHECKSUM_FILE}: ${err.message}`);
  }

  let archiveBuf;
  try {
    archiveBuf = await fetchBuffer(archiveUrl);
  } catch (err) {
    fail(`failed to download ${archiveUrl}: ${err.message}`);
  }

  try {
    verifyArchiveChecksum(asset, archiveBuf, checksumText);
  } catch (err) {
    fail(err.message);
  }

  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'orbit-cli-'));
  try {
    const archivePath = path.join(tmpDir, asset);
    fs.writeFileSync(archivePath, archiveBuf);
    extractTarGz(archivePath, tmpDir);

    const extractedBinary = path.join(tmpDir, 'orbit');
    if (!fs.existsSync(extractedBinary)) {
      fail(`extracted archive did not contain 'orbit' binary at ${extractedBinary}`);
    }
    fs.copyFileSync(extractedBinary, BIN_PATH);
    fs.chmodSync(BIN_PATH, 0o755);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }

  log(`installed orbit binary at ${BIN_PATH}`);
}

if (require.main === module) {
  main().catch((err) => fail(err && err.message ? err.message : String(err)));
}

module.exports = {
  TRUSTED_RELEASE_KEYS,
  parseChecksums,
  sha256,
  acknowledgeTrustedPublicKeyOverride,
  acknowledgeTrustedKeysOverride,
  extractTarGz,
  normalizeTrustedReleaseKeys,
  readTrustedReleaseKeys,
  validateArchiveMembers,
  validateExtractedBinary,
  verifyArchiveChecksum,
  verifyChecksumSignature,
};
