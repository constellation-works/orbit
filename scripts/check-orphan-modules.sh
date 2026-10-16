#!/usr/bin/env bash
set -euo pipefail

# Every src/**/*.rs file (excluding mod.rs/lib.rs/main.rs and anything under a
# tests/ dir) must be reachable from the module tree: declared as `mod <stem>`
# resolved through its enclosing module directories, or referenced by a direct
# `#[path = "..."]` attribute (Rust escapes, whitespace, comments and raw strings) in
# the crate's src tree.
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

# One process reads each physical owner once. All snapshots are scoped to this
# invocation; a later run observes edits without a cache identity or timestamp.
perl - <<'PERL'
use strict;
use warnings;
use Cwd qw(abs_path);
use Encode qw(encode);
use File::Basename qw(dirname basename);
use File::Find qw(find);
use File::Spec;

sub mask {
  my ($text) = @_;
  $text =~ s/[^\r\n]/ /g;
  return $text;
}

# Preserve the lexical masking rules for comments and literals. Attribute
# extraction keeps literals intact while treating comments as token whitespace.
sub strip_rust_non_code {
  my ($source, $keep_literals) = @_;
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
        $output .= $keep_literals ? $prefix : mask($prefix);
        $index += length($prefix);
        $state = 'raw_string';
        next;
      }

      if ($character eq '"') {
        $output .= $keep_literals ? $character : ' ';
        $index++;
        $state = 'string';
        next;
      }

      # Mask short character literals, including escaped characters, so a
      # quote inside one cannot change how the remainder of the owner is read.
      if ($character eq "'" && substr($source, $index) =~ /\A'(?:\\[\s\S]|[^\\\r\n']){1,4}'/) {
        my $literal = $&;
        $output .= $keep_literals ? $literal : mask($literal);
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
        $output .= $keep_literals ? $character : ' ';
        $index++;
        if ($index < $length) {
          my $escaped = substr($source, $index, 1);
          $output .= $keep_literals || $escaped eq "\n" ? $escaped : ' ';
          $index++;
        }
        next;
      }

      $output .= $keep_literals || $character eq "\n" ? $character : ' ';
      $index++;
      if ($character eq '"') {
        $state = 'code';
      }
      next;
    }

    my $terminator = '"' . ('#' x $raw_hashes);
    if (substr($source, $index, length($terminator)) eq $terminator) {
      $output .= $keep_literals ? $terminator : mask($terminator);
      $index += length($terminator);
      $state = 'code';
      next;
    }

    $output .= $keep_literals || $character eq "\n" ? $character : ' ';
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

# Decode Rust string escapes explicitly, keeping paths as UTF-8 bytes like
# source_text and filesystem names. Perl's string unescaping has different rules.
sub rust_string_value {
  my ($literal) = @_;
  $literal =~ s/\r\n/\n/g;
  my %escapes = ('n' => "\n", 'r' => "\r", 't' => "\t", '0' => "\0",
                 '\\' => '\\', '"' => '"', "'" => "'");
  my $value = '';
  pos($literal) = 0;
  while (pos($literal) < length($literal)) {
    if ($literal =~ /\G([^\\]+)/gc) {
      $value .= $1;
    } elsif ($literal =~ /\G\\([nrt0\\"'])/gc) {
      $value .= $escapes{$1};
    } elsif ($literal =~ /\G\\x([0-7][0-9a-fA-F])/gc) {
      $value .= chr(hex($1));
    } elsif ($literal =~ /\G\\u\{([0-9a-fA-F][0-9a-fA-F_]*)\}/gc) {
      my $digits = $1;
      $digits =~ s/_//g;
      return undef if length($digits) > 6;
      my $scalar = hex($digits);
      return undef if $scalar > 0x10ffff || ($scalar >= 0xd800 && $scalar <= 0xdfff);
      $value .= encode('UTF-8', chr($scalar));
    } elsif ($literal =~ /\G\\\n[\x09\x0a\x0d\x20]*/gc) {
      # Rust's string continuation skips the following ASCII whitespace.
    } else {
      return undef;
    }
  }
  return $value;
}

# Track only lexical attributes, module openings and braces, not cfg or macro
# expansion. Outside inline modules a path is relative to the source directory.
# The declaring module supplies the directory for ordinary children: an ordinary
# file module adds its stem, while a redirected file owns its containing directory.
sub index_module_targets {
  my ($path, $directory, $inline_directory, $targets, $pending) = @_;
  my $source = source_text($path);
  my $code = source_code($path);
  my @scopes = ([$directory, $inline_directory]);
  my $module_path;

  # Identifier bytes include UTF-8; keep offsets aligned with the source literals.
  # Rust permits whitespace/comments between # and [. Comments are already
  # masked as whitespace here and in the attribute's literal-preserving view.
  while ($code =~ /#[[:space:]]*\[|(?:\bpub(?:[[:space:]]*\([^)]*\))?[[:space:]]+)?\bmod[[:space:]]+(?:r#)?([[:word:]\x80-\xff]+)[[:space:]]*([;{])|[{}]|\S/g) {
    my ($token, $module, $ending, $start) = ($&, $1, $2, $-[0]);
    if ($token =~ /\A#[[:space:]]*\[\z/) {
      my $depth = 1;
      while ($depth && $code =~ /[\[\]]/g) {
        $depth += $& eq '[' ? 1 : -1;
      }
      last if $depth;
      my $attribute = strip_rust_non_code(substr($source, $start, pos($code) - $start), 1);
      if ($attribute =~ /\A#[[:space:]]*\[[[:space:]]*path[[:space:]]*=[[:space:]]*"((?:\\[\s\S]|[^"\\])*)"[[:space:]]*\]\z/) {
        $module_path = rust_string_value($1);
      } elsif ($attribute =~ /\A#[[:space:]]*\[[[:space:]]*path[[:space:]]*=[[:space:]]*r(\#*)"(.*?)"\1[[:space:]]*\]\z/s) {
        # Raw path contents are literal; the closing delimiter must repeat
        # the opening hash count. Read only attributes found in masked code,
        # so path-like text inside comments and other literals stays inert.
        $module_path = $2;
        $module_path =~ s/\r\n/\n/g;
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
          if (defined $resolved) {
            $targets->{$resolved} = 1;
            # Canonical identity is for caching/registration, not ownership:
            # rustc resolves children beside the logical symlink name.
            my $source_directory = abs_path(dirname($target));
            my $child_directory = defined($module_path) || basename($target) eq 'mod.rs'
              ? $source_directory
              : File::Spec->catdir($source_directory, basename($target, '.rs'));
            push @$pending, [$resolved, $source_directory, $child_directory];
          }
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
    open my $paths, '-|', 'rg', '--files', '--null', '--hidden', '--no-ignore', '--glob', '*.rs', $crate_src
      or die "Cannot list $crate_src: $!\n";
    my @paths;
    {
      local $/ = "\0";
      while (my $path = <$paths>) { chomp $path; push @paths, $path; }
    }
    close $paths or die "Cannot list $crate_src: ripgrep failed\n";
    my (%targets, %indexed);
    # Resolve owners before their children so redirected files never acquire a
    # guessed stem directory. Keep disconnected owners visible as before, after
    # walking declarations from crate/directory roots (including ignored ones).
    @paths = sort {
      (basename($b) =~ /^(?:mod|lib|main)\.rs$/ ? 1 : 0)
        <=> (basename($a) =~ /^(?:mod|lib|main)\.rs$/ ? 1 : 0)
        || $a cmp $b
    } @paths;
    for my $path (@paths) {
      my $resolved = abs_path($path);
      next if !defined($resolved) || exists $indexed{$resolved};
      my $source_directory = abs_path(dirname($path));
      my $directory = basename($path) =~ /^(?:mod|lib|main)\.rs$/
        ? $source_directory : File::Spec->catdir($source_directory, basename($path, '.rs'));
      my @pending = ([$resolved, $source_directory, $directory]);
      while (my $owner = pop @pending) {
        my ($owner_path, $owner_source_directory, $owner_directory) = @$owner;
        # A file can be loaded both ordinarily and through #[path]. Index both
        # real contexts, but never revisit cycles or add a default to a redirect.
        # Source directories are canonical and child directories add only a
        # finite source stem. Thus growing directory-symlink spellings converge
        # on the same context, while nested uses of one physical owner survive.
        next if $indexed{$owner_path}{$owner_source_directory}{$owner_directory}++;
        index_module_targets($owner_path, $owner_source_directory, $owner_directory,
                             \%targets, \@pending);
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
