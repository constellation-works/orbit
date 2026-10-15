#!/usr/bin/env bash
set -euo pipefail

# Every src/**/*.rs file (excluding mod.rs/lib.rs/main.rs and anything under a
# tests/ dir) must be reachable from the module tree: declared as `mod <stem>`
# resolved through its enclosing module directories, or referenced by a direct
# `#[path = "..."]` attribute (also raw strings) in the crate's src tree.
# An explicit path replaces the ordinary destination; only the actual candidate
# file is registered.
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

sub source_code {
  my ($path) = @_;
  $masked{$path} = strip_rust_non_code(source_text($path)) unless exists $masked{$path};
  return $masked{$path};
}

# Track only lexical attributes, module openings and braces, not cfg or macro
# expansion. Outside inline modules a path is relative to the source directory;
# ordinary declarations and inline modules also add an ordinary file's stem.
sub index_module_targets {
  my ($path, $targets) = @_;
  my $source = source_text($path);
  my $code = source_code($path);
  my $directory = dirname($path);
  my $inline_directory = basename($path) =~ /^(?:mod|lib|main)\.rs$/
    ? $directory : File::Spec->catdir($directory, basename($path, '.rs'));
  my @scopes = ([$directory, $inline_directory]);
  my $module_path;

  # Identifier bytes include UTF-8; keep offsets aligned with the source literals.
  while ($code =~ /#\[|(?:\bpub(?:[[:space:]]*\([^)]*\))?[[:space:]]+)?\bmod[[:space:]]+(?:r#)?([[:word:]\x80-\xff]+)[[:space:]]*([;{])|[{}]|\S/g) {
    my ($token, $module, $ending, $start) = ($&, $1, $2, $-[0]);
    if ($token eq '#[') {
      my $depth = 1;
      while ($depth && $code =~ /[\[\]]/g) {
        $depth += $& eq '[' ? 1 : -1;
      }
      last if $depth;
      my $attribute = substr($source, $start, pos($code) - $start);
      if ($attribute =~ /\A#\[path[[:space:]]*=[[:space:]]*"([^"\n]*)"/) {
        $module_path = $1;
      } elsif ($attribute =~ /\A#\[path[[:space:]]*=[[:space:]]*r(\#*)"(.*?)"\1/s) {
        # Raw path contents are literal; the closing delimiter must repeat
        # the opening hash count. Read only attributes found in masked code,
        # so path-like text inside comments and other literals stays inert.
        $module_path = $2;
      }
      next;
    }
    if (defined($module) && $ending eq ';') {
      # A path override registers only its destination, never the same-name
      # default file. Both production and sibling-test checks use this index.
      my @destinations = defined($module_path)
        ? (File::Spec->rel2abs($module_path, $scopes[-1][0]))
        : (File::Spec->catfile($scopes[-1][1], "$module.rs"),
           File::Spec->catfile($scopes[-1][1], $module, 'mod.rs'));
      for my $target (@destinations) {
        if (-f $target) {
          my $resolved = abs_path($target);
          $targets->{$resolved} = 1 if defined $resolved;
        }
      }
    } elsif (defined($module) && $ending eq '{') {
      # An inline module's explicit path replaces its default directory, also
      # suppressing the file-stem component for a non-mod.rs source file.
      my $child = defined($module_path)
        ? File::Spec->rel2abs($module_path, $scopes[-1][0])
        : File::Spec->catdir($scopes[-1][1], $module);
      push @scopes, [$child, $child];
    } elsif ($token eq '{') {
      push @scopes, $scopes[-1];
    } elsif ($token eq '}') {
      pop @scopes if @scopes > 1;
    }
    undef $module_path;
  }
}

sub is_declared {
  my ($file) = @_;
  my ($crate_src) = $file =~ m{^(.*?)/src/};
  $crate_src .= '/src';
  if (!exists $crate_targets{$crate_src}) {
    # Candidate discovery checks the physical source tree, including ignored
    # directories. Its owners must use the same scope: ignore rules (and
    # ripgrep-version differences in negations) cannot hide declarations.
    open my $paths, '-|', 'rg', '--files', '--hidden', '--no-ignore', '--glob', '*.rs', $crate_src
      or die "Cannot list $crate_src: $!\n";
    my @paths;
    while (my $path = <$paths>) { chomp $path; push @paths, $path; }
    close $paths or die "Cannot list $crate_src: ripgrep failed\n";
    my %targets;
    for my $path (@paths) {
      # Index actual destinations once per invocation, regardless of cfg. A
      # basename elsewhere in the crate is not registration.
      index_module_targets($path, \%targets);
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
  if ($file =~ m{/tests/}) {
    print "orphan test module: $file — no module declaration resolves to this file\n";
  } else {
    print "orphan module: $file — no module declaration resolves to this file\n";
  }
  $failed = 1;
}
exit 1 if $failed;
print "orphan module guard passed\n";
PERL
