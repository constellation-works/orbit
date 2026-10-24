#!/usr/bin/env python3
"""Repack the same nextest tar stream for transfer or native extraction.

Usage: ci-repack-nextest.py pack|unpack SOURCE DESTINATION [--parts N]
Requires zstd on PATH. Neither operation extracts members or replaces an
existing destination. Transfer frames use a bounded 512 MiB history; unpack
restores a small-window frame that nextest's default decoder accepts.

The transfer is split into N byte ranges, SOURCE.1 to SOURCE.N for unpack and
DESTINATION.1 to DESTINATION.N for pack, so the shards can download them as
separate artifacts in parallel. Unpack reads every part in order and fails
unless their concatenation decodes completely.
"""

import argparse
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time

COPY_BYTES = 8 * 1024 * 1024


def part_paths(base, parts):
    return [base.with_name(f"{base.name}.{index}") for index in range(1, parts + 1)]


def temporary_in(directory):
    with tempfile.NamedTemporaryFile(dir=directory, prefix="nextest-repack-", delete=False) as handle:
        return Path(handle.name)


def recode(zstd, sources, output, options):
    """Decode the concatenated sources into one encoder writing `output`."""
    decoder = subprocess.Popen([zstd, "-q", "-dc", "-M512MB"], stdin=subprocess.PIPE,
                               stdout=subprocess.PIPE)
    encoder = None
    try:
        encoder = subprocess.Popen([zstd, "-q", "-c", *options], stdin=decoder.stdout, stdout=output)
        decoder.stdout.close()
        try:
            for source in sources:
                with source.open("rb") as part:
                    shutil.copyfileobj(part, decoder.stdin, COPY_BYTES)
        except BrokenPipeError:
            pass  # The decoder exited early; its status below reports why.
        finally:
            try:
                decoder.stdin.close()
            except BrokenPipeError:
                pass
        # Both stages must succeed. In particular, a truncated input must not
        # publish an apparently valid encoding of its partial tar.
        decoded = decoder.wait()
        encoded = encoder.wait()
        if decoded or encoded:
            raise RuntimeError(f"zstd failed: decoder={decoded} encoder={encoded}")
    finally:
        for child in (decoder, encoder):
            if child is not None and child.poll() is None:
                child.kill()
                child.wait()


def split(source, destinations, temporaries):
    """Copy `source` into consecutive byte ranges, one new temporary per destination."""
    size = source.stat().st_size
    each = -(-size // len(destinations))
    with source.open("rb") as whole:
        for destination in destinations:
            temporary = temporary_in(destination.parent)
            temporaries.append(temporary)
            with temporary.open("wb") as part:
                remaining = each
                while remaining > 0:
                    chunk = whole.read(min(remaining, COPY_BYTES))
                    if not chunk:
                        break
                    part.write(chunk)
                    remaining -= len(chunk)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("pack", "unpack"))
    parser.add_argument("source", type=Path)
    parser.add_argument("destination", type=Path)
    parser.add_argument("--parts", type=int, default=1)
    args = parser.parse_args()
    if args.parts < 1:
        parser.error("--parts must be at least 1")
    zstd = shutil.which("zstd")
    if zstd is None:
        parser.error("zstd is required on PATH")
    if args.mode == "pack":
        sources = [args.source]
        destinations = part_paths(args.destination, args.parts)
        # The runner's zstd compresses on one thread unless asked; the pack
        # sits between the test build and every shard's download.
        options = ["-6", "--long=29", "-T0"]
    else:
        sources = part_paths(args.source, args.parts)
        destinations = [args.destination]
        options = ["-1"]
    if not all(source.is_file() for source in sources):
        parser.error("source must be an existing compressed archive with every part")
    if any(os.path.lexists(destination) for destination in destinations):
        parser.error("destination already exists; refusing to replace it")

    started = time.monotonic()
    temporaries = []
    published = []
    try:
        whole = temporary_in(destinations[0].parent)
        temporaries.append(whole)
        with whole.open("wb") as output:
            recode(zstd, sources, output, options)
        if args.mode == "pack":
            split(whole, destinations, temporaries)
            staged = temporaries[1:]
        else:
            staged = [whole]
        # link publishes without overwriting a destination created since the
        # initial check. Every temporary is on its destination's filesystem.
        for temporary, destination in zip(staged, destinations):
            os.link(temporary, destination)
            published.append(destination)
        print(f"nextest-repack: mode={args.mode} parts={args.parts} "
              f"input_bytes={sum(source.stat().st_size for source in sources)} "
              f"output_bytes={sum(destination.stat().st_size for destination in destinations)} "
              f"wall_seconds={time.monotonic() - started:.3f}")
        published = []
    finally:
        # A failure part-way through publication withdraws the parts this
        # run linked, so no partial transfer is left for an upload.
        for destination in published:
            destination.unlink()
        for temporary in temporaries:
            temporary.unlink()


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError) as error:
        print(f"nextest-repack: {error}", file=sys.stderr)
        sys.exit(1)
