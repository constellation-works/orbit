#!/usr/bin/env bash
# Fail the build when install.sh, npm/scripts/install-binary.js, and the
# shared Rust release trust set disagree on any trusted key's id, not_after,
# revoked_at, or PEM. npm/release-signing.pub must still appear as a subset
# of that set (L-0044).
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
canonical_key="$repo_root/npm/release-signing.pub"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

INSTALL_SH="install.sh"
INSTALL_JS="npm/scripts/install-binary.js"
RELEASE_RS="crates/orbit-common/src/security/release.rs"

die() {
  echo "check-installer-pubkey: $*" >&2
  exit 1
}

normalize_pem_text() {
  awk '
    {
      sub(/\r$/, "")
      sub(/[ \t]+$/, "")
    }
    /-----BEGIN PUBLIC KEY-----/ {
      in_key = 1
      print "-----BEGIN PUBLIC KEY-----"
      next
    }
    in_key && /-----END PUBLIC KEY-----/ {
      print "-----END PUBLIC KEY-----"
      in_key = 0
      next
    }
    in_key { print }
  '
}

# Parse RECORD streams from the extractors into $dest/$id/{not_after,revoked_at,pem}.
ingest_records() {
  local dest="$1"
  local label="$2"
  mkdir -p "$dest"
  : > "$dest/ids"

  local id="" not_after="" revoked_at=""
  local state=idle
  local pem_file=""

  while IFS= read -r line || [ -n "$line" ]; do
    case "$state" in
      idle)
        if [ "$line" = RECORD ]; then
          id=""
          not_after=""
          revoked_at=""
          state=fields
        fi
        ;;
      fields)
        case "$line" in
          id=*)
            id="${line#id=}"
            ;;
          not_after=*)
            not_after="${line#not_after=}"
            ;;
          revoked_at=*)
            revoked_at="${line#revoked_at=}"
            ;;
          -----BEGIN\ PUBLIC\ KEY-----)
            [ -n "$id" ] || die "extracted a PEM from $label with no id"
            if [ -e "$dest/$id" ]; then
              die "$label has duplicate key id '$id'"
            fi
            mkdir -p "$dest/$id"
            printf '%s' "$not_after" > "$dest/$id/not_after"
            printf '%s' "$revoked_at" > "$dest/$id/revoked_at"
            pem_file="$dest/$id/pem.raw"
            printf '%s\n' "$line" > "$pem_file"
            state=pem
            ;;
        esac
        ;;
      pem)
        printf '%s\n' "$line" >> "$pem_file"
        case "$line" in
          *-----END\ PUBLIC\ KEY-----*)
            normalize_pem_text < "$pem_file" > "$dest/$id/pem"
            rm -f "$pem_file"
            if ! grep -q -F -- "-----BEGIN PUBLIC KEY-----" "$dest/$id/pem"; then
              die "could not normalize PEM for '$id' in $label"
            fi
            printf '%s\n' "$id" >> "$dest/ids"
            state=idle
            ;;
        esac
        ;;
    esac
  done

  if [ "$state" != idle ]; then
    die "truncated trusted-key records from $label"
  fi

  sort -u -o "$dest/ids" "$dest/ids"

  local count
  count="$(wc -l < "$dest/ids" | tr -d ' ')"
  if [ "$count" -lt 1 ]; then
    die "expected at least one trusted key in $label, found $count"
  fi

  local rec_id na rev
  while IFS= read -r rec_id; do
    [ -n "$rec_id" ] || continue
    na="$(cat "$dest/$rec_id/not_after")"
    if ! [[ "$na" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]]; then
      die "$label key '$rec_id' has invalid not_after '$na'"
    fi
    rev="$(cat "$dest/$rec_id/revoked_at")"
    if [ -n "$rev" ] && ! [[ "$rev" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]]; then
      die "$label key '$rec_id' has invalid revoked_at '$rev'"
    fi
  done < "$dest/ids"
}

# install.sh: metadata comes only from printf 'id|not_after|revoked_at|...'
# record lines (not comments). PEMs come from write_release_key_* functions.
extract_install_sh() {
  awk '
    function fail(msg) {
      printf "check-installer-pubkey: %s\n", msg > "/dev/stderr"
      exit 1
    }

    /^write_release_key_[A-Za-z0-9_]+[[:space:]]*\(/ {
      name = $0
      sub(/^write_release_key_/, "", name)
      sub(/[[:space:]]*\(.*/, "", name)
      gsub(/_/, "-", name)
      current_fn_id = name
    }

    /-----BEGIN PUBLIC KEY-----/ {
      if (current_fn_id == "") next
      in_pem = 1
      pem = "-----BEGIN PUBLIC KEY-----\n"
      next
    }

    in_pem && /-----END PUBLIC KEY-----/ {
      pem = pem "-----END PUBLIC KEY-----\n"
      if (current_fn_id in pems) fail("install.sh has duplicate PEM for " current_fn_id)
      pems[current_fn_id] = pem
      in_pem = 0
      pem = ""
      next
    }

    in_pem {
      pem = pem $0 "\n"
      next
    }

    {
      trimmed = $0
      sub(/^[ \t]+/, "", trimmed)
      if (trimmed ~ /^#/) next
    }

    /printf[ \t]+'\''/ {
      if (!match($0, /printf[ \t]+'\''/)) next
      rest = substr($0, RSTART + RLENGTH)
      q = index(rest, "'\''")
      if (q < 2) next
      fmt = substr(rest, 1, q - 1)
      n = split(fmt, fields, "|")
      if (n < 4) next
      id = fields[1]
      if (id !~ /^orbit-release-/) next
      if (id in rec_ids) fail("install.sh has duplicate record line for " id)
      rec_ids[id] = 1
      rec_not_after[id] = fields[2]
      rec_revoked[id] = fields[3]
    }

    END {
      for (id in rec_ids) {
        if (!(id in pems)) fail("install.sh record line for " id " has no matching write_release_key_* PEM")
        print "RECORD"
        print "id=" id
        print "not_after=" rec_not_after[id]
        print "revoked_at=" rec_revoked[id]
        printf "%s", pems[id]
      }
      for (id in pems) {
        if (!(id in rec_ids)) fail("install.sh PEM for " id " has no printf record line")
      }
    }
  ' "$1"
}

extract_install_js() {
  awk '
    function fail(msg) {
      printf "check-installer-pubkey: %s\n", msg > "/dev/stderr"
      exit 1
    }

    function single_quoted_after(line, key,   rest, q) {
      if (!match(line, key "[ \t]*'\''")) return ""
      rest = substr(line, RSTART + RLENGTH)
      q = index(rest, "'\''")
      if (q < 2) return ""
      return substr(rest, 1, q - 1)
    }

    /const TRUSTED_RELEASE_KEYS/ { in_list = 1 }
    in_list && /^\]\);/ { in_list = 0 }

    in_list && /id:[ \t]*'\''orbit-release-/ {
      id = single_quoted_after($0, "id:")
    }

    in_list && /notAfter:[ \t]*'\''/ {
      not_after = single_quoted_after($0, "notAfter:")
    }

    in_list && /revokedAt:[ \t]*/ {
      if (!match($0, /revokedAt:[ \t]*/)) next
      rest = substr($0, RSTART + RLENGTH)
      if (rest ~ /^null/) {
        revoked = ""
      } else if (index(rest, "'\''") == 1) {
        rest = substr(rest, 2)
        q = index(rest, "'\''")
        if (q < 1) fail("npm/scripts/install-binary.js has a malformed revokedAt")
        revoked = substr(rest, 1, q - 1)
      } else {
        fail("npm/scripts/install-binary.js has a malformed revokedAt")
      }
    }

    in_list && /-----BEGIN PUBLIC KEY-----/ {
      in_pem = 1
      pem = "-----BEGIN PUBLIC KEY-----\n"
      next
    }

    in_pem && /-----END PUBLIC KEY-----/ {
      pem = pem "-----END PUBLIC KEY-----\n"
      if (id == "") fail("npm/scripts/install-binary.js PEM is missing an id")
      print "RECORD"
      print "id=" id
      print "not_after=" not_after
      print "revoked_at=" revoked
      printf "%s", pem
      id = ""
      not_after = ""
      revoked = ""
      pem = ""
      in_pem = 0
      next
    }

    in_pem { pem = pem $0 "\n" }
  ' "$1"
}

extract_release_rs() {
  awk '
    function fail(msg) {
      printf "check-installer-pubkey: %s\n", msg > "/dev/stderr"
      exit 1
    }

    function quoted(line,   rest, q) {
      if (!match(line, /"/)) return ""
      rest = substr(line, RSTART + 1)
      q = index(rest, "\"")
      if (q < 2) return ""
      return substr(rest, 1, q - 1)
    }

    /^const [A-Za-z0-9_]+_PEM:/ {
      name = $2
      sub(/:$/, "", name)
      const_name = name
      in_const = 1
    }

    in_const && /-----BEGIN PUBLIC KEY-----/ {
      in_const_body = 1
      const_pem[const_name] = "-----BEGIN PUBLIC KEY-----\n"
      next
    }

    in_const_body && /-----END PUBLIC KEY-----/ {
      const_pem[const_name] = const_pem[const_name] "-----END PUBLIC KEY-----\n"
      in_const_body = 0
      in_const = 0
      next
    }

    in_const_body {
      const_pem[const_name] = const_pem[const_name] $0 "\n"
      next
    }

    /pub const TRUSTED_RELEASE_KEYS/ { in_list = 1 }

    in_list && /TrustedReleaseKey[ \t]*\{/ {
      in_key = 1
      kid = ""
      kna = ""
      krev = ""
      kpem = ""
      next
    }

    in_key && /id:[ \t]*"/ { kid = quoted($0) }

    in_key && /not_after:[ \t]*"/ { kna = quoted($0) }

    in_key && /revoked_at:[ \t]*/ {
      if (!match($0, /revoked_at:[ \t]*/)) next
      rest = substr($0, RSTART + RLENGTH)
      if (rest ~ /^None/) {
        krev = ""
      } else if (match(rest, /^Some\("/)) {
        rest = substr(rest, RLENGTH + 1)
        q = index(rest, "\"")
        if (q < 1) fail("shared Rust release trust set has a malformed revoked_at")
        krev = substr(rest, 1, q - 1)
      } else {
        fail("shared Rust release trust set has a malformed revoked_at")
      }
    }

    in_key && /public_key_pem:[ \t]*/ {
      if (!match($0, /public_key_pem:[ \t]*/)) next
      rest = substr($0, RSTART + RLENGTH)
      sub(/[ \t]*,.*/, "", rest)
      sub(/[ \t]+$/, "", rest)
      kpem = rest
    }

    in_key && /^[[:space:]]*\}/ {
      if (kid == "" || kna == "" || kpem == "") {
        fail("shared Rust release trust set has an incomplete TrustedReleaseKey")
      }
      if (!(kpem in const_pem)) {
        fail("shared Rust release trust set references unknown PEM const " kpem)
      }
      print "RECORD"
      print "id=" kid
      print "not_after=" kna
      print "revoked_at=" krev
      printf "%s", const_pem[kpem]
      in_key = 0
      next
    }

    in_list && /^];/ { in_list = 0 }
  ' "$1"
}

assert_contains_canonical_key() {
  local label="$1"
  local records="$2"
  local rec_id

  while IFS= read -r rec_id; do
    [ -n "$rec_id" ] || continue
    if cmp -s "$canonical_norm" "$records/$rec_id/pem"; then
      return 0
    fi
  done < "$records/ids"

  die "$label must include npm/release-signing.pub"
}

report_field_mismatch() {
  local id="$1"
  local field="$2"
  local d1="$3"
  local l1="$4"
  local d2="$5"
  local l2="$6"
  local d3="$7"
  local l3="$8"
  local f1="$d1/$id/$field"
  local f2="$d2/$id/$field"
  local f3="$d3/$id/$field"

  if cmp -s "$f1" "$f2" && cmp -s "$f1" "$f3"; then
    return 0
  fi

  if cmp -s "$f1" "$f2"; then
    die "$l3 $field for $id does not match"
  fi
  if cmp -s "$f1" "$f3"; then
    die "$l2 $field for $id does not match"
  fi
  if cmp -s "$f2" "$f3"; then
    die "$l1 $field for $id does not match"
  fi
  die "$field for $id differs across $l1, $l2, and $l3"
}

compare_record_sets() {
  local d1="$1"
  local l1="$2"
  local d2="$3"
  local l2="$4"
  local d3="$5"
  local l3="$6"
  local id

  sort -u "$d1/ids" "$d2/ids" "$d3/ids" > "$tmp_dir/all-ids"

  while IFS= read -r id; do
    [ -n "$id" ] || continue
    [ -d "$d1/$id" ] || die "$l1 is missing key id '$id'"
    [ -d "$d2/$id" ] || die "$l2 is missing key id '$id'"
    [ -d "$d3/$id" ] || die "$l3 is missing key id '$id'"

    report_field_mismatch "$id" "not_after" "$d1" "$l1" "$d2" "$l2" "$d3" "$l3"
    report_field_mismatch "$id" "revoked_at" "$d1" "$l1" "$d2" "$l2" "$d3" "$l3"
    report_field_mismatch "$id" "pem" "$d1" "$l1" "$d2" "$l2" "$d3" "$l3"
  done < "$tmp_dir/all-ids"
}

[ -f "$canonical_key" ] || die "canonical key file missing: npm/release-signing.pub"
canonical_norm="$tmp_dir/canonical.pem"
normalize_pem_text < "$canonical_key" > "$canonical_norm"
grep -q -F -- "-----BEGIN PUBLIC KEY-----" "$canonical_norm" || die "could not read npm/release-signing.pub"

install_dir="$tmp_dir/install.sh"
npm_dir="$tmp_dir/install-binary.js"
rust_dir="$tmp_dir/release.rs"
install_stream="$tmp_dir/install.sh.records"
npm_stream="$tmp_dir/install-binary.js.records"
rust_stream="$tmp_dir/release.rs.records"

extract_install_sh "$repo_root/$INSTALL_SH" > "$install_stream"
extract_install_js "$repo_root/$INSTALL_JS" > "$npm_stream"
extract_release_rs "$repo_root/$RELEASE_RS" > "$rust_stream"
ingest_records "$install_dir" "$INSTALL_SH" < "$install_stream"
ingest_records "$npm_dir" "$INSTALL_JS" < "$npm_stream"
ingest_records "$rust_dir" "$RELEASE_RS" < "$rust_stream"

compare_record_sets \
  "$install_dir" "$INSTALL_SH" \
  "$npm_dir" "$INSTALL_JS" \
  "$rust_dir" "$RELEASE_RS"

# L-0044: Keep every release checksum-signature consumer on the canonical signing key.
assert_contains_canonical_key "$INSTALL_SH" "$install_dir"
assert_contains_canonical_key "$INSTALL_JS" "$npm_dir"
assert_contains_canonical_key "shared Rust release trust set" "$rust_dir"

echo "check-installer-pubkey: ok"
