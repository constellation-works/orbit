#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT

require_bin() {
  local bin="$1"
  if ! command -v "$bin" >/dev/null 2>&1; then
    echo "test-installer-security: required binary '$bin' not on PATH" >&2
    exit 2
  fi
}

require_bin node
require_bin openssl
require_bin tar

target_name() {
  local os arch
  os="$(uname -s 2>/dev/null || true)"
  arch="$(uname -m 2>/dev/null || true)"

  case "${os}/${arch}" in
    Darwin/arm64 | Darwin/aarch64) echo "aarch64-apple-darwin" ;;
    Darwin/x86_64 | Darwin/amd64) echo "x86_64-apple-darwin" ;;
    Linux/x86_64 | Linux/amd64) echo "x86_64-unknown-linux-gnu" ;;
    Linux/arm64 | Linux/aarch64) echo "aarch64-unknown-linux-gnu" ;;
    *)
      echo "test-installer-security: unsupported platform ${os:-unknown}/${arch:-unknown}" >&2
      exit 2
      ;;
  esac
}

sha256_file() {
  local file="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$file" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$file" | awk '{print $1}'
  else
    openssl dgst -sha256 "$file" | awk '{print $NF}'
  fi
}

write_test_binary() {
  local dir="$1"
  cat > "$dir/orbit" <<'BIN'
#!/bin/sh
if [ -n "${ORBIT_TEST_EXEC_MARKER:-}" ]; then
  printf '%s\n' executed > "$ORBIT_TEST_EXEC_MARKER"
fi
printf '%s\n' "orbit test 0.0.0"
BIN
  chmod 755 "$dir/orbit"
}

sign_checksums() {
  local private_key="$1"
  local checksums="$2"
  local signature="$3"
  openssl dgst -sha256 -sign "$private_key" -out "$signature" "$checksums"
}

make_regular_archive() {
  local archive="$1"
  local build_dir="$TMP_ROOT/build-regular"
  rm -rf "$build_dir"
  mkdir -p "$build_dir"
  write_test_binary "$build_dir"
  tar -czf "$archive" -C "$build_dir" orbit
}

make_symlink_archive() {
  local archive="$1"
  local build_dir="$TMP_ROOT/build-symlink"
  rm -rf "$build_dir"
  mkdir -p "$build_dir"
  ln -s /etc/passwd "$build_dir/orbit"
  tar -czf "$archive" -C "$build_dir" orbit
}

make_traversal_archive() {
  local archive="$1"
  local build_dir="$TMP_ROOT/build-traversal"
  rm -rf "$build_dir"
  mkdir -p "$build_dir/nested"
  printf '%s\n' "not orbit" > "$build_dir/evil"
  # GNU tar strips leading "../" from archive member names unless absolute-name
  # preservation is enabled, which would turn this fixture into a harmless
  # unexpected-member case instead of the traversal case it is meant to cover.
  tar -czPf "$archive" -C "$build_dir/nested" ../evil
}

prepare_release_dir() {
  local name="$1"
  local archive_kind="$2"
  local checksum_kind="$3"
  local signing_key="$4"
  local release_dir="$TMP_ROOT/$name"
  local asset="orbit-${TARGET}.tar.gz"
  local archive="$release_dir/$asset"
  mkdir -p "$release_dir"

  case "$archive_kind" in
    regular) make_regular_archive "$archive" ;;
    symlink) make_symlink_archive "$archive" ;;
    traversal) make_traversal_archive "$archive" ;;
    *)
      echo "unknown archive kind: $archive_kind" >&2
      exit 2
      ;;
  esac

  case "$checksum_kind" in
    correct) printf '%s  %s\n' "$(sha256_file "$archive")" "$asset" > "$release_dir/orbit-checksums.txt" ;;
    wrong) printf '%064d  %s\n' 0 "$asset" > "$release_dir/orbit-checksums.txt" ;;
    *)
      echo "unknown checksum kind: $checksum_kind" >&2
      exit 2
      ;;
  esac

  sign_checksums "$signing_key" "$release_dir/orbit-checksums.txt" "$release_dir/orbit-checksums.txt.sig"
  echo "$release_dir"
}

run_shell_install() {
  local release_dir="$1"
  local install_dir="$2"
  local marker="$3"
  ORBIT_INSTALL_BASE_URL="file://$release_dir" \
    ORBIT_INSTALL_DIR="$install_dir" \
    ORBIT_RELEASE_TRUSTED_KEYS_FILE="$TRUSTED_KEYS_FILE" \
    ORBIT_RELEASE_TRUSTED_KEYS_FILE_ACKNOWLEDGE_TRUST_CHANGE=1 \
    ORBIT_TEST_EXEC_MARKER="$marker" \
    sh "$ROOT/install.sh"
}

expect_shell_failure() {
  local release_dir="$1"
  local label="$2"
  local install_dir="$TMP_ROOT/install-$label"
  local marker="$TMP_ROOT/marker-$label"
  local log_file="$TMP_ROOT/$label.log"

  if run_shell_install "$release_dir" "$install_dir" "$marker" > "$log_file" 2>&1; then
    echo "FAIL: shell installer accepted $label" >&2
    cat "$log_file" >&2
    exit 1
  fi
  if [ -e "$marker" ]; then
    echo "FAIL: shell installer executed binary for $label" >&2
    exit 1
  fi
}

CURRENT_PRIVATE_KEY="$TMP_ROOT/current-release-signing.key"
CURRENT_PUBLIC_KEY="$TMP_ROOT/current-release-signing.pub"
EXPIRED_PRIVATE_KEY="$TMP_ROOT/expired-release-signing.key"
EXPIRED_PUBLIC_KEY="$TMP_ROOT/expired-release-signing.pub"
REVOKED_PRIVATE_KEY="$TMP_ROOT/revoked-release-signing.key"
REVOKED_PUBLIC_KEY="$TMP_ROOT/revoked-release-signing.pub"
UNTRUSTED_PRIVATE_KEY="$TMP_ROOT/untrusted-release-signing.key"
UNTRUSTED_PUBLIC_KEY="$TMP_ROOT/untrusted-release-signing.pub"
TRUSTED_KEYS_FILE="$TMP_ROOT/trusted-release-keys.txt"

openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out "$CURRENT_PRIVATE_KEY" >/dev/null 2>&1
openssl rsa -pubout -in "$CURRENT_PRIVATE_KEY" -out "$CURRENT_PUBLIC_KEY" >/dev/null 2>&1
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out "$EXPIRED_PRIVATE_KEY" >/dev/null 2>&1
openssl rsa -pubout -in "$EXPIRED_PRIVATE_KEY" -out "$EXPIRED_PUBLIC_KEY" >/dev/null 2>&1
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out "$REVOKED_PRIVATE_KEY" >/dev/null 2>&1
openssl rsa -pubout -in "$REVOKED_PRIVATE_KEY" -out "$REVOKED_PUBLIC_KEY" >/dev/null 2>&1
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out "$UNTRUSTED_PRIVATE_KEY" >/dev/null 2>&1
openssl rsa -pubout -in "$UNTRUSTED_PRIVATE_KEY" -out "$UNTRUSTED_PUBLIC_KEY" >/dev/null 2>&1

printf 'current|2099-12-31||%s\n' "$CURRENT_PUBLIC_KEY" > "$TRUSTED_KEYS_FILE"
printf 'expired|2000-01-01||%s\n' "$EXPIRED_PUBLIC_KEY" >> "$TRUSTED_KEYS_FILE"
printf 'revoked|2099-12-31|2026-05-23|%s\n' "$REVOKED_PUBLIC_KEY" >> "$TRUSTED_KEYS_FILE"

PRIVATE_KEY="$CURRENT_PRIVATE_KEY"
PUBLIC_KEY="$CURRENT_PUBLIC_KEY"

TARGET="$(target_name)"

good_release="$(prepare_release_dir good regular correct "$CURRENT_PRIVATE_KEY")"
bad_checksum_release="$(prepare_release_dir bad-checksum regular wrong "$CURRENT_PRIVATE_KEY")"
expired_key_release="$(prepare_release_dir expired-key regular correct "$EXPIRED_PRIVATE_KEY")"
revoked_key_release="$(prepare_release_dir revoked-key regular correct "$REVOKED_PRIVATE_KEY")"
untrusted_key_release="$(prepare_release_dir untrusted-key regular correct "$UNTRUSTED_PRIVATE_KEY")"
symlink_release="$(prepare_release_dir symlink symlink correct "$CURRENT_PRIVATE_KEY")"
traversal_release="$(prepare_release_dir traversal traversal correct "$CURRENT_PRIVATE_KEY")"

expect_shell_failure "$bad_checksum_release" "checksum-mismatch"
expect_shell_failure "$expired_key_release" "expired-key"
expect_shell_failure "$revoked_key_release" "revoked-key"
expect_shell_failure "$untrusted_key_release" "untrusted-key"
expect_shell_failure "$symlink_release" "symlink-member"
expect_shell_failure "$traversal_release" "traversal-member"

good_install_dir="$TMP_ROOT/install-good"
good_marker="$TMP_ROOT/marker-good"
run_shell_install "$good_release" "$good_install_dir" "$good_marker" > "$TMP_ROOT/good.log" 2>&1
test -x "$good_install_dir/orbit"
test -f "$good_marker"

npm_checksums="$TMP_ROOT/npm-checksums.txt"
npm_signature="$TMP_ROOT/npm-checksums.txt.sig"
good_archive="$good_release/orbit-${TARGET}.tar.gz"
printf '%s  %s\n' "$(sha256_file "$good_archive")" "orbit-${TARGET}.tar.gz" > "$npm_checksums"
sign_checksums "$CURRENT_PRIVATE_KEY" "$npm_checksums" "$npm_signature"

npm_expired_signature="$TMP_ROOT/npm-checksums-expired.sig"
npm_revoked_signature="$TMP_ROOT/npm-checksums-revoked.sig"
npm_untrusted_signature="$TMP_ROOT/npm-checksums-untrusted.sig"
sign_checksums "$EXPIRED_PRIVATE_KEY" "$npm_checksums" "$npm_expired_signature"
sign_checksums "$REVOKED_PRIVATE_KEY" "$npm_checksums" "$npm_revoked_signature"
sign_checksums "$UNTRUSTED_PRIVATE_KEY" "$npm_checksums" "$npm_untrusted_signature"

ROOT="$ROOT" \
  TMP_ROOT="$TMP_ROOT" \
  PUBLIC_KEY="$PUBLIC_KEY" \
  EXPIRED_PUBLIC_KEY="$EXPIRED_PUBLIC_KEY" \
  REVOKED_PUBLIC_KEY="$REVOKED_PUBLIC_KEY" \
  CHECKSUMS="$npm_checksums" \
  SIGNATURE="$npm_signature" \
  EXPIRED_SIGNATURE="$npm_expired_signature" \
  REVOKED_SIGNATURE="$npm_revoked_signature" \
  UNTRUSTED_SIGNATURE="$npm_untrusted_signature" \
  GOOD_ARCHIVE="$good_archive" \
  SYMLINK_ARCHIVE="$symlink_release/orbit-${TARGET}.tar.gz" \
  TRAVERSAL_ARCHIVE="$traversal_release/orbit-${TARGET}.tar.gz" \
  TARGET="$TARGET" \
  node <<'NODE'
const fs = require('node:fs');
const path = require('node:path');
const installer = require(path.join(process.env.ROOT, 'npm/scripts/install-binary.js'));

function expectThrow(fn, pattern, label) {
  try {
    fn();
  } catch (err) {
    if (!pattern.test(err.message)) {
      throw new Error(`${label}: expected ${pattern}, got ${err.message}`);
    }
    return;
  }
  throw new Error(`${label}: expected failure`);
}

function withTempDir(label, fn) {
  const safeLabel = label.replace(/[^a-z0-9_-]/gi, '-');
  const dir = fs.mkdtempSync(path.join(process.env.TMP_ROOT, `${safeLabel}-`));
  try {
    return fn(dir);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
}

function expectExtractFailure(archivePath, pattern, label) {
  withTempDir(label, (dir) => {
    expectThrow(
      () => installer.extractTarGz(archivePath, dir),
      pattern,
      label
    );
  });
}

const publicKey = fs.readFileSync(process.env.PUBLIC_KEY, 'utf8');
const expiredPublicKey = fs.readFileSync(process.env.EXPIRED_PUBLIC_KEY, 'utf8');
const revokedPublicKey = fs.readFileSync(process.env.REVOKED_PUBLIC_KEY, 'utf8');
const checksumText = fs.readFileSync(process.env.CHECKSUMS, 'utf8');
const signature = fs.readFileSync(process.env.SIGNATURE);
const expiredSignature = fs.readFileSync(process.env.EXPIRED_SIGNATURE);
const revokedSignature = fs.readFileSync(process.env.REVOKED_SIGNATURE);
const untrustedSignature = fs.readFileSync(process.env.UNTRUSTED_SIGNATURE);
const goodArchive = fs.readFileSync(process.env.GOOD_ARCHIVE);
const asset = `orbit-${process.env.TARGET}.tar.gz`;
const trustedKeys = [
  { id: 'current', notAfter: '2099-12-31', revokedAt: null, publicKeyPem: publicKey },
  { id: 'expired', notAfter: '2000-01-01', revokedAt: null, publicKeyPem: expiredPublicKey },
  { id: 'revoked', notAfter: '2099-12-31', revokedAt: '2026-05-23', publicKeyPem: revokedPublicKey },
];

installer.verifyChecksumSignature(checksumText, signature, trustedKeys);
installer.verifyArchiveChecksum(asset, goodArchive, checksumText);
installer.validateArchiveMembers(process.env.GOOD_ARCHIVE);
withTempDir('npm-good-archive-extract', (dir) => {
  installer.extractTarGz(process.env.GOOD_ARCHIVE, dir);
  const extracted = path.join(dir, 'orbit');
  if (!fs.statSync(extracted).isFile()) {
    throw new Error('npm good archive extraction did not create a regular orbit file');
  }
});

const tamperedSignature = Buffer.from(signature);
tamperedSignature[0] = tamperedSignature[0] ^ 0xff;
expectThrow(
  () => installer.verifyChecksumSignature(checksumText, tamperedSignature, trustedKeys),
  /signature verification failed/,
  'npm signature failure'
);
expectThrow(
  () => installer.verifyChecksumSignature(checksumText, expiredSignature, trustedKeys),
  /expired release signing key expired/,
  'npm expired key rejection'
);
expectThrow(
  () => installer.verifyChecksumSignature(checksumText, revokedSignature, trustedKeys),
  /revoked release signing key revoked/,
  'npm revoked key rejection'
);
expectThrow(
  () => installer.verifyChecksumSignature(checksumText, untrustedSignature, trustedKeys),
  /no trusted release signing key matched/,
  'npm untrusted key rejection'
);
installer.verifyChecksumSignature(checksumText, signature, publicKey);
if (
  !Array.isArray(installer.TRUSTED_RELEASE_KEYS) ||
  installer.TRUSTED_RELEASE_KEYS.length !== 1 ||
  installer.TRUSTED_RELEASE_KEYS[0].id !== 'orbit-release-key-3'
) {
  throw new Error('npm installer must expose only the current release signing key');
}
expectThrow(
  () => installer.verifyArchiveChecksum(asset, goodArchive, `0000000000000000000000000000000000000000000000000000000000000000  ${asset}\n`),
  /checksum mismatch/,
  'npm checksum failure'
);
expectExtractFailure(
  process.env.SYMLINK_ARCHIVE,
  /symlink/,
  'npm symlink archive rejection'
);
expectThrow(
  () => installer.extractTarGz(process.env.TRAVERSAL_ARCHIVE, process.env.TMP_ROOT),
  /unsafe release archive member/,
  'npm traversal archive rejection'
);
NODE

if ORBIT_RELEASE_PUBLIC_KEY_FILE="$PUBLIC_KEY" \
  ROOT="$ROOT" \
  node -e 'const path = require("node:path"); const installer = require(path.join(process.env.ROOT, "npm/scripts/install-binary.js")); installer.acknowledgeTrustedPublicKeyOverride();' \
  > "$TMP_ROOT/npm-key-override-missing-ack.log" 2>&1; then
  echo "FAIL: npm installer accepted ORBIT_RELEASE_PUBLIC_KEY_FILE without acknowledgement" >&2
  exit 1
fi

ORBIT_RELEASE_PUBLIC_KEY_FILE="$PUBLIC_KEY" \
  ORBIT_RELEASE_PUBLIC_KEY_FILE_ACKNOWLEDGE_TRUST_CHANGE=1 \
  ROOT="$ROOT" \
  node -e 'const path = require("node:path"); const installer = require(path.join(process.env.ROOT, "npm/scripts/install-binary.js")); installer.acknowledgeTrustedPublicKeyOverride();' \
  > "$TMP_ROOT/npm-key-override-ack.log" 2>&1
grep -q "trusting replacement release signing key" "$TMP_ROOT/npm-key-override-ack.log"

if ORBIT_RELEASE_TRUSTED_KEYS_FILE="$TRUSTED_KEYS_FILE" \
  ROOT="$ROOT" \
  node -e 'const path = require("node:path"); const installer = require(path.join(process.env.ROOT, "npm/scripts/install-binary.js")); installer.acknowledgeTrustedKeysOverride();' \
  > "$TMP_ROOT/npm-keys-override-missing-ack.log" 2>&1; then
  echo "FAIL: npm installer accepted ORBIT_RELEASE_TRUSTED_KEYS_FILE without acknowledgement" >&2
  exit 1
fi

ORBIT_RELEASE_TRUSTED_KEYS_FILE="$TRUSTED_KEYS_FILE" \
  ORBIT_RELEASE_TRUSTED_KEYS_FILE_ACKNOWLEDGE_TRUST_CHANGE=1 \
  ROOT="$ROOT" \
  node -e 'const path = require("node:path"); const installer = require(path.join(process.env.ROOT, "npm/scripts/install-binary.js")); installer.acknowledgeTrustedKeysOverride();' \
  > "$TMP_ROOT/npm-keys-override-ack.log" 2>&1
grep -q "trusting replacement release signing key set" "$TMP_ROOT/npm-keys-override-ack.log"

echo "test-installer-security: ok"
