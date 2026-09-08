#!/bin/sh

set -eu

REPO="${ORBIT_INSTALL_REPO:-constellation-works/orbit}"
BINARY_NAME="orbit"
CHECKSUM_FILE="orbit-checksums.txt"
CHECKSUM_SIGNATURE_FILE="orbit-checksums.txt.sig"
INSTALL_DIR="${ORBIT_INSTALL_DIR:-$HOME/.orbit/bin}"

log() {
  printf '%s\n' "$*"
}

fail() {
  printf 'orbit installer: %s\n' "$*" >&2
  exit 1
}

warn() {
  printf 'orbit installer: %s\n' "$*" >&2
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || fail "required command not found: $1"
}

download() {
  url="$1"
  destination="$2"

  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url" -o "$destination"
    return
  fi

  if command -v wget >/dev/null 2>&1; then
    wget -qO "$destination" "$url"
    return
  fi

  fail "curl or wget is required to download Orbit releases"
}

# Key IDs are stable labels carried alongside the public key blocks below.
# The numeric suffix is a generation counter, not a date — IDs survive rotation
# so a key that has been the "successor" for a year is still readable by ID.
#
write_release_key_orbit_release_key_3() {
  destination="$1"

  cat > "$destination" <<'EOF'
-----BEGIN PUBLIC KEY-----
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
EOF
}

release_date_number() {
  value="$1"

  printf '%s' "$value" | awk '
    /^[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]$/ {
      gsub("-", "")
      print
      exit 0
    }
    { exit 1 }
  ' || fail "invalid release signing key date: $value"
}

write_builtin_trusted_key_records() {
  key_dir="${TMP_DIR}/trusted-release-keys"
  mkdir -p "$key_dir"

  current_key_path="${key_dir}/orbit-release-key-3.pub"
  write_release_key_orbit_release_key_3 "$current_key_path"

  printf 'orbit-release-key-3|2029-12-31||%s\n' "$current_key_path"
}

trusted_key_records() {
  if [ -n "${ORBIT_RELEASE_PUBLIC_KEY_FILE:-}" ] && [ -n "${ORBIT_RELEASE_TRUSTED_KEYS_FILE:-}" ]; then
    fail "ORBIT_RELEASE_PUBLIC_KEY_FILE and ORBIT_RELEASE_TRUSTED_KEYS_FILE cannot both be set"
  fi

  if [ -n "${ORBIT_RELEASE_TRUSTED_KEYS_FILE:-}" ]; then
    [ "${ORBIT_RELEASE_TRUSTED_KEYS_FILE_ACKNOWLEDGE_TRUST_CHANGE:-}" = "1" ] \
      || fail "ORBIT_RELEASE_TRUSTED_KEYS_FILE requires ORBIT_RELEASE_TRUSTED_KEYS_FILE_ACKNOWLEDGE_TRUST_CHANGE=1"
    [ -f "$ORBIT_RELEASE_TRUSTED_KEYS_FILE" ] || fail "ORBIT_RELEASE_TRUSTED_KEYS_FILE does not exist: $ORBIT_RELEASE_TRUSTED_KEYS_FILE"
    warn "ORBIT_RELEASE_TRUSTED_KEYS_FILE=$ORBIT_RELEASE_TRUSTED_KEYS_FILE set; trusting replacement release signing key set"
    cat "$ORBIT_RELEASE_TRUSTED_KEYS_FILE"
    return
  fi

  if [ -n "${ORBIT_RELEASE_PUBLIC_KEY_FILE:-}" ]; then
    [ "${ORBIT_RELEASE_PUBLIC_KEY_FILE_ACKNOWLEDGE_TRUST_CHANGE:-}" = "1" ] \
      || fail "ORBIT_RELEASE_PUBLIC_KEY_FILE requires ORBIT_RELEASE_PUBLIC_KEY_FILE_ACKNOWLEDGE_TRUST_CHANGE=1"
    [ -f "$ORBIT_RELEASE_PUBLIC_KEY_FILE" ] || fail "ORBIT_RELEASE_PUBLIC_KEY_FILE does not exist: $ORBIT_RELEASE_PUBLIC_KEY_FILE"
    warn "ORBIT_RELEASE_PUBLIC_KEY_FILE=$ORBIT_RELEASE_PUBLIC_KEY_FILE set; trusting replacement release signing key"
    warn "ORBIT_RELEASE_PUBLIC_KEY_FILE is deprecated; prefer ORBIT_RELEASE_TRUSTED_KEYS_FILE for the full trust set (key IDs, not_after, revoked_at)"
    printf 'override|||%s\n' "$ORBIT_RELEASE_PUBLIC_KEY_FILE"
    return
  fi

  write_builtin_trusted_key_records
}

verify_checksum_signature() {
  checksum_path="$1"
  signature_path="$2"
  records_path="${TMP_DIR}/trusted-release-key-records.txt"
  today_number="$(release_date_number "$(date -u '+%Y-%m-%d')")"

  trusted_key_records > "$records_path"

  while IFS='|' read -r key_id not_after revoked_at public_key_path; do
    case "$key_id" in
      "" | \#*)
        continue
        ;;
    esac

    [ -n "$public_key_path" ] || fail "trusted release signing key ${key_id} has no public key path"
    [ -f "$public_key_path" ] || fail "trusted release signing key ${key_id} public key does not exist: $public_key_path"

    if openssl dgst -sha256 -verify "$public_key_path" -signature "$signature_path" "$checksum_path" >/dev/null 2>&1; then
      if [ -n "$revoked_at" ]; then
        fail "release checksum signature was made by revoked release signing key ${key_id} (revoked ${revoked_at})"
      fi
      if [ -n "$not_after" ] && [ "$today_number" -gt "$(release_date_number "$not_after")" ]; then
        fail "release checksum signature was made by expired release signing key ${key_id} (not_after ${not_after})"
      fi
      log "Authenticated ${CHECKSUM_FILE} with release signing key ${key_id}"
      return
    fi
  done < "$records_path"

  fail "release checksum signature verification failed for ${CHECKSUM_FILE}: no trusted release signing key matched"
}

compute_sha256() {
  file="$1"

  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$file" | awk '{print $1}'
    return
  fi

  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$file" | awk '{print $1}'
    return
  fi

  if command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$file" | awk '{print $NF}'
    return
  fi

  fail "sha256sum, shasum, or openssl is required to verify downloads"
}

validate_archive_members() {
  archive_path="$1"
  member_list="${TMP_DIR}/archive-members.txt"
  member=""
  member_count=0

  tar -tzf "$archive_path" > "$member_list" || fail "could not inspect release archive"
  while IFS= read -r archive_member; do
    member_count=$((member_count + 1))
    if [ "$member_count" -eq 1 ]; then
      member="$archive_member"
    fi
  done < "$member_list"
  [ "$member_count" = "1" ] || fail "release archive must contain only ${BINARY_NAME}"

  case "$member" in
    "$BINARY_NAME")
      ;;
    "" | /* | *"/.."* | "../"* | *"/../"* | *".."* )
      fail "unsafe release archive member: ${member:-<empty>}"
      ;;
    *)
      fail "unexpected release archive member: $member"
      ;;
  esac
}

normalize_tag() {
  version="$1"

  case "$version" in
    v*)
      printf '%s' "$version"
      ;;
    *)
      printf 'v%s' "$version"
      ;;
  esac
}

resolve_target() {
  os="$(uname -s 2>/dev/null || true)"
  arch="$(uname -m 2>/dev/null || true)"

  case "${os}/${arch}" in
    Darwin/arm64 | Darwin/aarch64)
      printf 'aarch64-apple-darwin'
      ;;
    Darwin/x86_64 | Darwin/amd64)
      printf 'x86_64-apple-darwin'
      ;;
    Linux/x86_64 | Linux/amd64)
      printf 'x86_64-unknown-linux-gnu'
      ;;
    Linux/arm64 | Linux/aarch64)
      printf 'aarch64-unknown-linux-gnu'
      ;;
    *)
      fail "unsupported platform: ${os:-unknown}/${arch:-unknown}"
      ;;
  esac
}

need_cmd awk
need_cmd date
need_cmd install
need_cmd mktemp
need_cmd openssl
need_cmd tar

TARGET="$(resolve_target)"
ARCHIVE_NAME="orbit-${TARGET}.tar.gz"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/orbit-install.XXXXXX")"

cleanup() {
  rm -rf "$TMP_DIR"
}

trap cleanup EXIT HUP INT TERM

if [ -n "${ORBIT_INSTALL_BASE_URL:-}" ]; then
  BASE_URL="${ORBIT_INSTALL_BASE_URL%/}"
  VERSION_LABEL="${ORBIT_VERSION:-custom}"
elif [ -n "${ORBIT_VERSION:-}" ]; then
  RELEASE_TAG="$(normalize_tag "$ORBIT_VERSION")"
  BASE_URL="https://github.com/${REPO}/releases/download/${RELEASE_TAG}"
  VERSION_LABEL="$RELEASE_TAG"
else
  BASE_URL="https://github.com/${REPO}/releases/latest/download"
  VERSION_LABEL="latest"
fi

ARCHIVE_PATH="${TMP_DIR}/${ARCHIVE_NAME}"
CHECKSUM_PATH="${TMP_DIR}/${CHECKSUM_FILE}"
SIGNATURE_PATH="${TMP_DIR}/${CHECKSUM_SIGNATURE_FILE}"

log "Downloading Orbit ${VERSION_LABEL} for ${TARGET}..."
download "${BASE_URL}/${CHECKSUM_FILE}" "$CHECKSUM_PATH"
download "${BASE_URL}/${CHECKSUM_SIGNATURE_FILE}" "$SIGNATURE_PATH"
verify_checksum_signature "$CHECKSUM_PATH" "$SIGNATURE_PATH"
download "${BASE_URL}/${ARCHIVE_NAME}" "$ARCHIVE_PATH"

EXPECTED_SHA="$(awk -v asset="$ARCHIVE_NAME" '$2 == asset { print $1 }' "$CHECKSUM_PATH")"
[ -n "$EXPECTED_SHA" ] || fail "checksum entry for ${ARCHIVE_NAME} was not found in ${CHECKSUM_FILE}"

ACTUAL_SHA="$(compute_sha256 "$ARCHIVE_PATH")"

if [ "$EXPECTED_SHA" != "$ACTUAL_SHA" ]; then
  fail "checksum verification failed for ${ARCHIVE_NAME} (expected ${EXPECTED_SHA}, got ${ACTUAL_SHA})"
fi

mkdir -p "$INSTALL_DIR"
validate_archive_members "$ARCHIVE_PATH"
tar -xzf "$ARCHIVE_PATH" -C "$TMP_DIR" "$BINARY_NAME"
[ ! -L "${TMP_DIR}/${BINARY_NAME}" ] || fail "release archive member is a symlink: ${BINARY_NAME}"
[ -f "${TMP_DIR}/${BINARY_NAME}" ] || fail "release archive did not contain ${BINARY_NAME}"

install -m 755 "${TMP_DIR}/${BINARY_NAME}" "${INSTALL_DIR}/${BINARY_NAME}"

log "Installed Orbit to ${INSTALL_DIR}/${BINARY_NAME}"
"${INSTALL_DIR}/${BINARY_NAME}" --version

case ":$PATH:" in
  *:"$INSTALL_DIR":*)
    ;;
  *)
    log
    log "Add Orbit to your PATH by adding this line to your shell profile:"
    log "  export PATH=\"${INSTALL_DIR}:\$PATH\""
    ;;
esac
