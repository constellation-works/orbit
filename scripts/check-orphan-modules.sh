#!/usr/bin/env bash
set -euo pipefail

# Every src/**/*.rs file (excluding mod.rs/lib.rs/main.rs and anything under a
# tests/ dir) must be reachable from the module tree: declared as `mod <stem>`
# in its directory's owning file (mod.rs, lib.rs, or main.rs), in the sibling
# file that owns the directory as a module (`foo.rs` next to `foo/`), or
# referenced by a direct `#[path = "..."]` attribute in the crate's src tree,
# resolved relative to the declaring source file to the actual candidate file.
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

# One process reads each owner once. All snapshots are scoped to this
# invocation; a later run observes edits without a cache identity or timestamp.
perl - <<'PERL'
use strict;
use warnings;
use Cwd qw(abs_path);
use File::Basename qw(dirname basename);
use File::Find qw(find);
use File::Spec;
use Encode qw(decode);

sub mask {
  my ($text) = @_;
  $text =~ s/[^\r\n]/ /g;
  return $text;
}

# Preserve the lexical masking rules for comments and literals.
sub strip_rust_non_code {
  my ($source) = @_;
  my $output = '';
  my $index = 0;
  my $length = length $source;
  my $state = 'code';
  my $block_depth = 0;
  my $raw_hashes = 0;

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

      if ($character eq 'r' && substr($source, $index) =~ /\Ar(#+)?"/) {
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
      if ($character eq "'" && substr($source, $index) =~ /\A'(?:\\[\s\S]|[^\\\r\n']){1,4}'/) {
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

  return $output;
}

my (%sources, %masked, %crate_targets);
sub source_text {
  my ($path) = @_;
  if (!exists $sources{$path}) {
    open my $file, '<', $path or die "Cannot read $path: $!\n";
    local $/;
    $sources{$path} = <$file> // '';
    close $file or die "Cannot close $path: $!\n";
  }
  return $sources{$path};
}

sub is_declared {
  my ($file) = @_;
  my $dir = dirname($file);
  my $stem = decode('UTF-8', basename($file, '.rs'));
  for my $owner ("$dir/mod.rs", "$dir/lib.rs", "$dir/main.rs",
                 dirname($dir) . '/' . basename($dir) . '.rs') {
    next unless -f $owner;
    unless (exists $masked{$owner}) {
      $masked{$owner} = [split /\n/, decode('UTF-8', strip_rust_non_code(source_text($owner)))];
    }
    # ripgrep's original declaration search matched within each line.
    return 1 if grep { /\bmod[[:space:]]+(r#)?${stem}\b[[:space:]]*[;{]/ } @{$masked{$owner}};
  }

  my ($crate_src) = $file =~ m{^(.*?)/src/};
  $crate_src .= '/src';
  if (!exists $crate_targets{$crate_src}) {
    # Keep ripgrep's original hidden/ignore/glob selection for #[path] search.
    open my $paths, '-|', 'rg', '--files', '--glob', '*.rs', $crate_src
      or die "Cannot list $crate_src: $!\n";
    my @paths;
    while (my $path = <$paths>) { chomp $path; push @paths, $path; }
    close $paths or die "Cannot list $crate_src: ripgrep failed\n";
    my %targets;
    for my $path (@paths) {
      # Keep the existing literal/cfg scan, but index actual destinations once
      # per invocation. A basename elsewhere in the crate is not registration.
      my $source = source_text($path);
      while ($source =~ /#\[path[[:space:]]*=[[:space:]]*"([^"\n]*)"/g) {
        my $target = File::Spec->rel2abs($1, dirname($path));
        next unless -f $target;
        my $resolved = abs_path($target);
        $targets{$resolved} = 1 if defined $resolved;
      }
    }
    $crate_targets{$crate_src} = \%targets;
  }
  my $resolved = abs_path($file);
  return defined($resolved) && exists $crate_targets{$crate_src}{$resolved};
}

my (@modules, @tests);
find({ no_chdir => 1, wanted => sub {
  my $file = $File::Find::name;
  return unless -f $file && !-l $file && $file =~ /\.rs$/;
  my $base = basename($file);
  if ($file =~ m{/tests/}) {
    push @tests, $file unless $base eq 'mod.rs';
  } elsif ($file !~ m{/src/bin/} && $base !~ /^(?:mod|lib|main)\.rs$/) {
    push @modules, $file;
  }
}}, glob('crates/*/src'));

my $failed = 0;
for my $file (@modules, @tests) {
  next if is_declared($file);
  my $stem = basename($file, '.rs');
  if ($file =~ m{/tests/}) {
    print "orphan test module: $file — no mod $stem; declaration in " . dirname($file) . "/mod.rs and no matching #[path] attribute\n";
  } else {
    print "orphan module: $file — no mod $stem; declaration and no matching #[path] attribute\n";
  }
  $failed = 1;
}
exit 1 if $failed;
print "orphan module guard passed\n";
PERL
