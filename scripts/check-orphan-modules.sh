#!/usr/bin/env bash
set -euo pipefail

# Every src/**/*.rs file (excluding mod.rs/lib.rs/main.rs and anything under a
# tests/ dir) must be reachable from the module tree: declared as `mod <stem>`
# in its directory's owning file (mod.rs, lib.rs, or main.rs), in the sibling
# file that owns the directory as a module (`foo.rs` next to `foo/`), or
# referenced by a `#[path = "...stem.rs"]` attribute anywhere in the crate.
# Otherwise the file is never compiled, linted, or tested — see
# docs/design/orbit-cleanup/orbitenginecleanup.md §1/§11.
#
# Under the sibling-test layout (docs/design-patterns/test_layout.md), every
# src/**/tests/<name>.rs file must likewise be declared as `mod <name>` in its
# own tests/mod.rs (or reached via a `#[path]` attribute). A file that looks
# like a registered test but isn't compiles to nothing and silently drops its
# coverage — see F2026-08-108.

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

if ! command -v rg >/dev/null 2>&1; then
  echo "check-orphan-modules: ripgrep (rg) is required; install it before running" >&2
  exit 1
fi

if ! command -v perl >/dev/null 2>&1; then
  echo "check-orphan-modules: Perl is required; install it before running" >&2
  exit 1
fi

fail=0

# Replace comments and string/character literal contents with whitespace while
# preserving newlines, so declaration matching only sees Rust tokens. This is
# intentionally limited to the lexical forms relevant to the guard; it avoids
# treating examples in comments and literals as module registrations without
# adding a Rust parser dependency to this shell-only check.
strip_rust_non_code() {
  perl - "$1" <<'PERL'
use strict;
use warnings;

my $source = do { local $/; <> };
my $output = '';
my $index = 0;
my $length = length $source;
my $state = 'code';
my $block_depth = 0;
my $raw_hashes = 0;

sub mask {
  my ($text) = @_;
  $text =~ s/[^\r\n]/ /g;
  return $text;
}

while ($index < $length) {
  my $character = substr($source, $index, 1);

  if ($state eq 'code') {
    if (substr($source, $index, 2) eq '//') {
      $output .= '  ';
      $index += 2;
      $state = 'line_comment';
      next;
    }

    if (substr($source, $index, 2) eq '/*') {
      $output .= '  ';
      $index += 2;
      $block_depth = 1;
      $state = 'block_comment';
      next;
    }

    my $remaining = substr($source, $index);
    if ($remaining =~ /\Ar(#+)?"/) {
      my $prefix = $&;
      $raw_hashes = defined($1) ? length($1) : 0;
      $output .= mask($prefix);
      $index += length($prefix);
      $state = 'raw_string';
      next;
    }

    if ($character eq '"') {
      $output .= ' ';
      $index++;
      $state = 'string';
      next;
    }

    # Mask short character literals, including escaped characters, so a
    # quote inside one cannot change how the remainder of the owner is read.
    if ($character eq "'" && $remaining =~ /\A'(?:\\[\s\S]|[^\\\r\n']){1,4}'/) {
      my $literal = $&;
      $output .= mask($literal);
      $index += length($literal);
      next;
    }

    $output .= $character;
    $index++;
    next;
  }

  if ($state eq 'line_comment') {
    $output .= $character eq "\n" ? "\n" : ' ';
    $index++;
    $state = 'code' if $character eq "\n";
    next;
  }

  if ($state eq 'block_comment') {
    if (substr($source, $index, 2) eq '/*') {
      $output .= '  ';
      $index += 2;
      $block_depth++;
      next;
    }

    if (substr($source, $index, 2) eq '*/') {
      $output .= '  ';
      $index += 2;
      $block_depth--;
      if ($block_depth == 0) {
        $state = 'code';
      }
      next;
    }

    $output .= $character eq "\n" ? "\n" : ' ';
    $index++;
    next;
  }

  if ($state eq 'string') {
    if ($character eq '\\') {
      $output .= ' ';
      $index++;
      if ($index < $length) {
        my $escaped = substr($source, $index, 1);
        $output .= $escaped eq "\n" ? "\n" : ' ';
        $index++;
      }
      next;
    }

    $output .= $character eq "\n" ? "\n" : ' ';
    $index++;
    if ($character eq '"') {
      $state = 'code';
    }
    next;
  }

  my $terminator = '"' . ('#' x $raw_hashes);
  if (substr($source, $index, length($terminator)) eq $terminator) {
    $output .= mask($terminator);
    $index += length($terminator);
    $state = 'code';
    next;
  }

  $output .= $character eq "\n" ? "\n" : ' ';
  $index++;
}

print $output;
PERL
}

is_declared() {
  local stem="$1"
  local dir="$2"
  local file="$3"

  local owner
  for owner in \
    "$dir/mod.rs" \
    "$dir/lib.rs" \
    "$dir/main.rs" \
    "$(dirname "$dir")/$(basename "$dir").rs"; do
    if [[ -f "$owner" ]] && rg -q "\\bmod[[:space:]]+(r#)?${stem}\\b[[:space:]]*[;{]" < <(strip_rust_non_code "$owner"); then
      return 0
    fi
  done

  local crate_src="${file%%/src/*}/src"
  local base
  base="$(basename "$file")"
  if rg -q "#\\[path[[:space:]]*=[[:space:]]*\"[^\"]*${base}\"" --glob '*.rs' "$crate_src" 2>/dev/null; then
    return 0
  fi

  return 1
}

while IFS= read -r -d '' file; do
  case "$file" in
    # Sibling unit-test trees (docs/design-patterns/test_layout.md).
    */tests/*) continue ;;
    # Cargo auto-discovers every file directly under src/bin/ as its own
    # binary crate root ([[bin]] path or implicit) — no `mod` needed.
    */src/bin/*) continue ;;
  esac

  dir="$(dirname "$file")"
  base="$(basename "$file")"
  stem="${base%.rs}"

  if ! is_declared "$stem" "$dir" "$file"; then
    echo "orphan module: ${file} — no mod ${stem}; declaration and no matching #[path] attribute"
    fail=1
  fi
done < <(find crates/*/src -type f -name '*.rs' \
  ! -name 'mod.rs' ! -name 'lib.rs' ! -name 'main.rs' -print0)

# Sibling tests/<name>.rs files must themselves be declared in their owning
# tests/mod.rs (or via #[path]) — see docs/design-patterns/test_layout.md.
while IFS= read -r -d '' file; do
  dir="$(dirname "$file")"
  base="$(basename "$file")"
  stem="${base%.rs}"

  if ! is_declared "$stem" "$dir" "$file"; then
    echo "orphan test module: ${file} — no mod ${stem}; declaration in ${dir}/mod.rs and no matching #[path] attribute"
    fail=1
  fi
done < <(find crates/*/src -type f -path '*/tests/*.rs' ! -name 'mod.rs' -print0)

if [[ "$fail" -ne 0 ]]; then
  exit 1
fi

echo "orphan module guard passed"
