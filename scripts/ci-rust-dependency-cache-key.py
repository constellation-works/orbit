#!/usr/bin/env python3
"""Derive the `prefix-key` of the Rust dependency caches from the virtual root manifest.

Swatinem/rust-cache (v2.9.3) restores an exact key first, then the newest
archive whose key starts with `<prefix-key>-<shared-key>-<OS>-<arch>-<hash of
installed rustc versions and CARGO*/RUST*/CC* environment>`. Only the exact key
adds a hash of Cargo.lock's registry and git packages, the member manifests and
every `.cargo/config.toml` and `rust-toolchain(.toml)` below the workspace. The
action never reads a virtual workspace root, so the root inputs that nothing
else covers belong in `prefix-key`.

The prefix keeps every root setting except the tables below, which describe
only workspace members or are already recorded by the exact key:

- `[workspace] members`, `exclude`, `default-members`: the member manifests are
  in the exact key.
- `[workspace.package]`, `[workspace.lints]`: inherited by members only;
  dependencies build with `--cap-lints`, and the action prunes member units
  before it saves.
- `[workspace.metadata]`: Cargo ignores it.
- `[workspace.dependencies]`: Cargo.lock records each version and source.
  Feature and `default-features` choices are not in any key; Cargo's unit
  fingerprints rebuild a dependency whose features changed.

Build profiles, `[patch]`, `[replace]`, the resolver and any other root
setting stay in the prefix: changing them makes restored third-party units
unusable (or, for a path patch, is invisible to the lock hash), so they start
a new namespace. A manifest change outside them, such as adding a workspace
dependency or bumping the release version, keeps the namespace: a pull request
then restores the newest compatible archive and Cargo rebuilds only the units
whose inputs changed. TOML formatting and comments do not count.

Prints `prefix-key=<value>` and appends it to `$GITHUB_OUTPUT` when that is set.
Every rust-cache producer and seeder must take its `prefix-key` from this
helper so their keys stay identical.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import sys
import tomllib

# Bump when the canonical form below changes, so an old archive never shares a
# namespace with a differently derived one.
PREFIX_LABEL = "v2-rust"
MEMBER_ONLY_WORKSPACE_KEYS = frozenset(
    {"members", "exclude", "default-members", "package", "lints", "metadata", "dependencies"})


def dependency_build_identity(manifest: dict) -> dict:
    """Return the root-manifest settings that can make cached dependency units unusable."""
    identity = {key: value for key, value in manifest.items() if key != "workspace"}
    workspace = manifest.get("workspace", {})
    if isinstance(workspace, dict):
        identity["workspace"] = {key: value for key, value in workspace.items()
                                 if key not in MEMBER_ONLY_WORKSPACE_KEYS}
    else:
        identity["workspace"] = workspace
    return identity


def prefix_key(manifest_path: Path) -> str:
    with manifest_path.open("rb") as handle:
        manifest = tomllib.load(handle)
    canonical = json.dumps(dependency_build_identity(manifest), sort_keys=True,
                           separators=(",", ":"), ensure_ascii=False, default=str)
    return f"{PREFIX_LABEL}-{hashlib.sha256(canonical.encode()).hexdigest()}"


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--manifest", type=Path, default=Path("Cargo.toml"),
                        help="virtual workspace root manifest (default: Cargo.toml)")
    args = parser.parse_args(argv)
    try:
        value = prefix_key(args.manifest)
    except (OSError, tomllib.TOMLDecodeError) as error:
        sys.stderr.write(f"ci-rust-dependency-cache-key: cannot read {args.manifest}: {error}\n")
        return 1
    line = f"prefix-key={value}\n"
    sys.stdout.write(line)
    output = os.environ.get("GITHUB_OUTPUT")
    if output:
        with open(output, "a", encoding="utf-8") as handle:
            handle.write(line)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
