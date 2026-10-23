#!/usr/bin/env python3
"""Repack the same nextest tar stream for transfer or native extraction.

Usage: ci-repack-nextest.py pack|unpack SOURCE DESTINATION
Requires zstd on PATH. Neither operation extracts members or replaces an
existing destination. Transfer frames use a bounded 512 MiB history; unpack
restores a small-window frame that nextest's default decoder accepts.
"""

import argparse
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("pack", "unpack"))
    parser.add_argument("source", type=Path)
    parser.add_argument("destination", type=Path)
    args = parser.parse_args()
    zstd = shutil.which("zstd")
    if zstd is None:
        parser.error("zstd is required on PATH")
    if not args.source.is_file():
        parser.error("source must be an existing compressed archive")
    if os.path.lexists(args.destination):
        parser.error("destination already exists; refusing to replace it")

    started = time.monotonic()
    options = ["-6", "--long=29"] if args.mode == "pack" else ["-1"]
    decoder = None
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(dir=args.destination.parent, prefix="nextest-repack-",
                                         delete=False) as output:
            temporary = Path(output.name)
            # Both stages must succeed. In particular, a truncated input must
            # not publish an apparently valid encoding of its partial tar.
            decoder = subprocess.Popen([zstd, "-q", "-dc", "-M512MB", "--", str(args.source)],
                                       stdout=subprocess.PIPE)
            encoder = subprocess.run([zstd, "-q", "-c", *options],
                                     stdin=decoder.stdout, stdout=output)
            decoder.stdout.close()
            decoded = decoder.wait()
            if decoded or encoder.returncode:
                raise RuntimeError(f"zstd failed: decoder={decoded} encoder={encoder.returncode}")
        # link publishes without overwriting a destination created since the
        # initial check. The temporary is on the same filesystem.
        os.link(temporary, args.destination)
        print(f"nextest-repack: mode={args.mode} input_bytes={args.source.stat().st_size} "
              f"output_bytes={args.destination.stat().st_size} "
              f"wall_seconds={time.monotonic() - started:.3f}")
    finally:
        if decoder is not None and decoder.poll() is None:
            decoder.kill()
            decoder.wait()
        if temporary is not None:
            temporary.unlink()


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError) as error:
        print(f"nextest-repack: {error}", file=sys.stderr)
        sys.exit(1)
