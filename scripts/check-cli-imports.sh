#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cli_src_dir="$repo_root/crates/orbit-cli/src"

forbidden=(
  "orbit_agent"
  "orbit_engine"
  "orbit_exec"
  "orbit_policy"
  "orbit_store"
  "orbit_tools"
)

# Match `<crate>::` references in orbit-cli sources, ignoring occurrences
# inside `"..."` string literals (e.g. tracing target labels like
# `"orbit_engine::activity_job::cli_runner"`) and `//` line comments. Block
# comments (`/* ... */`) are not handled — none exist in the current tree.
# Scan every current source once and retain the original per-crate diagnostics.
# The masking rules intentionally match the existing lexical approximation.
if ! command -v perl >/dev/null 2>&1; then
  echo "check-cli-imports: Perl is required; install it before running" >&2
  exit 1
fi

find "$cli_src_dir" -type f -name '*.rs' -print0 | perl -0 -e '
use strict;
use warnings;
use Encode qw(decode);
my ($root, @forbidden) = @ARGV;
my %matches;
while (my $file = <STDIN>) {
  chomp $file;
  open my $source, "<", $file or die "Cannot read $file: $!\n";
  local $/ = "\n";
  my $line = 0;
  while (my $text = <$source>) {
    $line++;
    chomp $text;
    $text =~ s/"[^"]*"/""/g;
    $text =~ s{//.*$}{};
    # GNU grep word boundaries recognize UTF-8 identifiers; keep that
    # behavior while retaining the original bytes in diagnostics.
    my $code = decode("UTF-8", $text);
    for my $crate (@forbidden) {
      if ($code =~ /\b\Q$crate\E::/) {
        my $relative = substr($file, length($root) + 1);
        push @{$matches{$crate}}, "$relative:$line:$text";
      }
    }
  }
  close $source or die "Cannot close $file: $!\n";
}
my $failed = 0;
for my $crate (@forbidden) {
  if ($matches{$crate}) {
    print "forbidden import reference found in orbit-cli: $crate\::\n";
    print "$_\n" for @{$matches{$crate}};
    $failed = 1;
  }
}
exit $failed;
' "$repo_root" "${forbidden[@]}"

echo "orbit-cli import guard passed"
