#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
canonical_key="$repo_root/npm/release-signing.pub"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

extract_pem_blocks() {
  local source="$1"
  local destination="$2"

  awk '
    /-----BEGIN PUBLIC KEY-----/ {
      in_key = 1
      print "-----BEGIN PUBLIC KEY-----"
      next
    }
    in_key && /-----END PUBLIC KEY-----/ {
      print "-----END PUBLIC KEY-----"
      found = 1
      in_key = 0
      next
    }
    in_key { print }
    END { if (!found) exit 1 }
  ' "$source" > "$destination"
}

assert_contains_canonical_key() {
  local label="$1"
  local extracted_keys="$2"

  awk '
    NR == FNR {
      canonical = canonical $0 "\n"
      next
    }
    {
      extracted = extracted $0 "\n"
    }
    END {
      exit(index(extracted, canonical) ? 0 : 1)
    }
  ' "$canonical_key" "$extracted_keys" || {
    echo "check-installer-pubkey: $label must include npm/release-signing.pub" >&2
    exit 1
  }
}

install_keys="$tmp_dir/install-sh-keys.pem"
npm_keys="$tmp_dir/npm-install-keys.pem"
rust_trust_set_keys="$tmp_dir/rust-release-trust-set-keys.pem"
extract_pem_blocks "$repo_root/install.sh" "$install_keys" || {
  echo "check-installer-pubkey: could not extract release signing public keys from install.sh" >&2
  exit 1
}
extract_pem_blocks "$repo_root/npm/scripts/install-binary.js" "$npm_keys" || {
  echo "check-installer-pubkey: could not extract release signing public keys from install-binary.js" >&2
  exit 1
}
extract_pem_blocks "$repo_root/crates/orbit-common/src/security/release.rs" "$rust_trust_set_keys" || {
  echo "check-installer-pubkey: could not extract release signing public keys from the shared Rust release trust set" >&2
  exit 1
}

install_key_count="$(awk '/-----BEGIN PUBLIC KEY-----/ { count++ } END { print count + 0 }' "$install_keys")"
npm_key_count="$(awk '/-----BEGIN PUBLIC KEY-----/ { count++ } END { print count + 0 }' "$npm_keys")"
rust_trust_set_key_count="$(awk '/-----BEGIN PUBLIC KEY-----/ { count++ } END { print count + 0 }' "$rust_trust_set_keys")"
if [ "$install_key_count" -lt 2 ]; then
  echo "check-installer-pubkey: expected at least two public key blocks in install.sh, found $install_key_count" >&2
  exit 1
fi
if [ "$npm_key_count" -lt 2 ]; then
  echo "check-installer-pubkey: expected at least two public key blocks in install-binary.js, found $npm_key_count" >&2
  exit 1
fi
if [ "$rust_trust_set_key_count" -lt 2 ]; then
  echo "check-installer-pubkey: expected at least two public key blocks in the shared Rust release trust set, found $rust_trust_set_key_count" >&2
  exit 1
fi

assert_contains_canonical_key "install.sh" "$install_keys"
assert_contains_canonical_key "install-binary.js" "$npm_keys"
# L-0044: Keep every release checksum-signature consumer on the canonical signing key.
assert_contains_canonical_key "shared Rust release trust set" "$rust_trust_set_keys"

# Read the canonical key IDs out of install-binary.js (the structured source of
# truth), then require each ID to appear in install.sh. This avoids hardcoding
# specific key IDs in the guardrail — every legitimate rotation that updates
# both files in lockstep continues to pass, while a drift between the two
# installers fails closed.
npm_key_ids="$(awk "/id:[[:space:]]*'orbit-release-/ { match(\$0, /'[^']+'/); if (RSTART > 0) print substr(\$0, RSTART + 1, RLENGTH - 2) }" "$repo_root/npm/scripts/install-binary.js")"
if [ -z "$npm_key_ids" ]; then
  echo "check-installer-pubkey: could not extract any orbit-release-* key IDs from install-binary.js" >&2
  exit 1
fi

npm_id_count=0
while IFS= read -r key_id; do
  [ -n "$key_id" ] || continue
  npm_id_count=$((npm_id_count + 1))
  if ! grep -q -F "$key_id" "$repo_root/install.sh"; then
    echo "check-installer-pubkey: key ID '$key_id' is in install-binary.js but missing from install.sh" >&2
    exit 1
  fi
done <<EOF
$npm_key_ids
EOF

if [ "$npm_id_count" -lt 2 ]; then
  echo "check-installer-pubkey: expected at least two orbit-release-* key IDs in install-binary.js, found $npm_id_count" >&2
  exit 1
fi

echo "check-installer-pubkey: ok"
