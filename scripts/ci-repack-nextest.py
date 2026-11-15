#!/usr/bin/env python3
"""Repack a nextest archive's tar stream for transfer, or extract the transfer.

Usage: ci-repack-nextest.py pack|extract SOURCE DESTINATION [--parts N]
Requires zstd on PATH. Transfer frames use a bounded 512 MiB history, which
nextest's own decoder does not accept.

Pack re-encodes the archive SOURCE for transfer and never replaces an
existing destination. Extract decodes the transfer once and writes its
members under the existing directory DESTINATION, as `cargo nextest run
--archive-file --extract-to DESTINATION --extract-overwrite` would, without
re-encoding the stream for nextest and decoding it a second time. Only
regular files and directories under `target/` are admitted, through
tarfile's `data` filter (no absolute or escaping paths, links, devices or
set-id bits); anything else, or a stream that does not decode completely,
fails. A shard then runs nextest from the extraction's
`target/nextest/binaries-metadata.json` and `cargo-metadata.json`.

The transfer is split into N byte ranges, DESTINATION.1 to DESTINATION.N for
pack and SOURCE.1 to SOURCE.N for extract, so the shards can download them as
separate artifacts in parallel. Extract reads every part in order and fails
unless their concatenation decodes completely.
"""

import argparse
import os
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import sys
import tarfile
import tempfile
import threading
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


def extract(zstd, sources, destination):
    """Decode the concatenated sources and extract the tar stream under `destination`."""
    decoder = subprocess.Popen([zstd, "-q", "-dc", "-M512MB"], stdin=subprocess.PIPE,
                               stdout=subprocess.PIPE)
    fed = []

    def feed():
        # This process reads the decoder's output, so the parts are written
        # to its input from a second thread, in order.
        try:
            for source in sources:
                with source.open("rb") as part:
                    shutil.copyfileobj(part, decoder.stdin, COPY_BYTES)
            fed.append(True)
        except (BrokenPipeError, OSError):
            pass  # The decoder exited early; its status reports why.
        finally:
            try:
                decoder.stdin.close()
            except BrokenPipeError:
                pass

    feeder = threading.Thread(target=feed)
    feeder.start()
    members = 0
    try:
        try:
            with tarfile.open(fileobj=decoder.stdout, mode="r|") as archive:
                for member in archive:
                    parts = PurePosixPath(member.name).parts
                    if (not parts or parts[0] != "target" or ".." in parts
                            or not (member.isfile() or member.isdir())):
                        raise RuntimeError(f"refusing archive member {member.name!r}")
                    archive.extract(member, destination, filter="data")
                    members += 1
        except tarfile.TarError as error:
            raise RuntimeError(f"tar stream invalid: {error}") from error
        # Read past the end-of-archive marker, so a truncated or corrupt
        # frame after it still fails the decoder.
        while decoder.stdout.read(COPY_BYTES):
            pass
        decoded = decoder.wait()
        feeder.join()
        if decoded or not fed:
            raise RuntimeError(f"zstd failed: decoder={decoded} fed={bool(fed)}")
        if members == 0:
            raise RuntimeError("the archive holds no members")
        return members
    finally:
        if decoder.poll() is None:
            decoder.kill()
            decoder.wait()
        feeder.join()


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
    parser.add_argument("mode", choices=("pack", "extract"))
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
        # Keep the long-distance deduplication but spend less time encoding:
        # level 3 saved ~20% of pack + restore time on a CI-profile archive,
        # for ~10% more transfer bytes. Use every core on the hosted runner.
        options = ["-3", "--long=29", "-T0"]
    else:
        sources = part_paths(args.source, args.parts)
    if not all(source.is_file() for source in sources):
        parser.error("source must be an existing compressed archive with every part")

    started = time.monotonic()
    if args.mode == "extract":
        if not args.destination.is_dir():
            parser.error("destination must be an existing directory")
        members = extract(zstd, sources, args.destination)
        print(f"nextest-repack: mode=extract parts={args.parts} "
              f"input_bytes={sum(source.stat().st_size for source in sources)} "
              f"members={members} wall_seconds={time.monotonic() - started:.3f}")
        return
    if any(os.path.lexists(destination) for destination in destinations):
        parser.error("destination already exists; refusing to replace it")

    temporaries = []
    published = []
    try:
        whole = temporary_in(destinations[0].parent)
        temporaries.append(whole)
        with whole.open("wb") as output:
            recode(zstd, sources, output, options)
        split(whole, destinations, temporaries)
        staged = temporaries[1:]
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
