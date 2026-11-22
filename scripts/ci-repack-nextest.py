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
import errno
import os
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import sys
import tarfile
import tempfile
import threading
import time

try:
    import fcntl
except ImportError:  # pragma: no cover - non-POSIX platforms
    fcntl = None

COPY_BYTES = 8 * 1024 * 1024
# 1 MiB matches Linux's unprivileged pipe-max-size and stays cache-friendly while
# cutting Python's default 10 KiB / 16 KiB tarfile streaming loop and disk writes.
EXTRACT_BUFFER_BYTES = 1024 * 1024
PIPE_BYTES = 1024 * 1024


def part_paths(base, parts):
    return [base.with_name(f"{base.name}.{index}") for index in range(1, parts + 1)]


def temporary_in(directory):
    with tempfile.NamedTemporaryFile(dir=directory, prefix="nextest-repack-", delete=False) as handle:
        return Path(handle.name)


def enlarge_pipe(fd):
    """Best-effort Linux pipe capacity increase to reduce pipe round-trips."""
    if fcntl is None or not sys.platform.startswith("linux"):
        return
    try:
        fcntl.fcntl(fd, getattr(fcntl, "F_SETPIPE_SZ", 1031), PIPE_BYTES)
    except OSError:
        pass


def write_all(fd, view):
    """Write every byte in `view` to `fd`, returning the number of os.write calls."""
    written = 0
    total = len(view)
    calls = 0
    while written < total:
        count = os.write(fd, view[written:])
        if count <= 0:
            raise OSError("short write")
        written += count
        calls += 1
    return calls


def feed_sources_to_fd(sources, stdin_fd):
    """Concatenate `sources` into `stdin_fd` in order, using Linux sendfile when available."""
    use_sendfile = sys.platform.startswith("linux") and hasattr(os, "sendfile")
    for source in sources:
        fd = os.open(source, os.O_RDONLY)
        try:
            offset = 0
            if use_sendfile:
                size = os.fstat(fd).st_size
                while offset < size:
                    try:
                        sent = os.sendfile(stdin_fd, fd, offset, min(size - offset, COPY_BYTES))
                    except OSError as err:
                        if err.errno in (errno.EINVAL, errno.ENOSYS, errno.EOPNOTSUPP, errno.EPERM):
                            use_sendfile = False
                            break
                        raise
                    if sent == 0:
                        break
                    offset += sent
            if not use_sendfile:
                if offset:
                    os.lseek(fd, offset, os.SEEK_SET)
                while True:
                    chunk = os.read(fd, COPY_BYTES)
                    if not chunk:
                        break
                    write_all(stdin_fd, memoryview(chunk))
        finally:
            os.close(fd)


def copy_fd_range(src_fd, dst_fd, length):
    """Copy up to `length` bytes from `src_fd` to `dst_fd`, preferring copy_file_range."""
    remaining = length
    if hasattr(os, "copy_file_range"):
        while remaining > 0:
            try:
                copied = os.copy_file_range(src_fd, dst_fd, min(remaining, COPY_BYTES))
            except OSError as err:
                if err.errno in (errno.EXDEV, errno.EINVAL, errno.ENOSYS,
                                 errno.EOPNOTSUPP, errno.EPERM):
                    break
                raise
            if copied == 0:
                return
            remaining -= copied
    while remaining > 0:
        chunk = os.read(src_fd, min(remaining, COPY_BYTES))
        if not chunk:
            break
        write_all(dst_fd, memoryview(chunk))
        remaining -= len(chunk)


class TarPipeStream:
    """Forward-only stream over a raw pipe using one reusable buffer.

    TarFile parses ustar/GNU/PAX headers and sparse maps from this stream, while
    member payloads stream straight into destination descriptors in large chunks
    without tarfile._Stream's 10 KiB reads, 16 KiB copy loop or bytes.join slices.
    """

    def __init__(self, raw, bufsize=EXTRACT_BUFFER_BYTES):
        self.raw = raw
        self.pos = 0
        self.buf = bytearray(bufsize)
        self.mv = memoryview(self.buf)
        self.head = 0
        self.tail = 0

    def tell(self):
        return self.pos

    def _read_some(self):
        self.head = 0
        count = self.raw.readinto(self.mv)
        self.tail = count or 0
        return self.tail

    def read(self, size=-1):
        if size == 0:
            return b""
        if size < 0:
            out = bytearray(self.mv[self.head:self.tail])
            self.pos += self.tail - self.head
            self.head = self.tail = 0
            while True:
                count = self.raw.readinto(self.mv)
                if not count:
                    break
                self.pos += count
                out.extend(self.mv[:count])
            return bytes(out)
        avail = self.tail - self.head
        if avail >= size:
            data = bytes(self.mv[self.head:self.head + size])
            self.head += size
            self.pos += size
            return data
        out = bytearray()
        if avail > 0:
            out.extend(self.mv[self.head:self.tail])
            self.head = self.tail = 0
        while len(out) < size:
            count = self._read_some()
            if count == 0:
                break
            take = min(size - len(out), count)
            out.extend(self.mv[:take])
            self.head = take
        self.pos += len(out)
        return bytes(out)

    def seek(self, offset):
        if offset < self.pos:
            raise tarfile.StreamError("seeking backwards is not allowed")
        remaining = offset - self.pos
        while remaining > 0:
            avail = self.tail - self.head
            if avail == 0:
                if self._read_some() == 0:
                    break
                avail = self.tail - self.head
            take = min(remaining, avail)
            self.head += take
            self.pos += take
            remaining -= take
        return self.pos

    def copy_to_fd(self, dst_fd, length):
        remaining = length
        if remaining == 0:
            return 0
        avail = self.tail - self.head
        if avail >= remaining:
            calls = write_all(dst_fd, self.mv[self.head:self.head + remaining])
            self.head += remaining
            self.pos += remaining
            return calls
        cap = len(self.buf)
        calls = 0
        while remaining > 0:
            avail = self.tail - self.head
            if avail > 0 and self.head > 0:
                self.buf[:avail] = self.mv[self.head:self.tail]
            self.head = 0
            self.tail = avail
            target = min(remaining, cap)
            while self.tail < target:
                count = self.raw.readinto(self.mv[self.tail:])
                if not count:
                    raise tarfile.ReadError("unexpected end of data")
                self.tail += count
            calls += write_all(dst_fd, self.mv[:target])
            self.head = target
            self.pos += target
            remaining -= target
        return calls

    def drain(self):
        self.head = self.tail = 0
        while self.raw.readinto(self.mv):
            pass


def recode(zstd, sources, output, options):
    """Decode the concatenated sources into one encoder writing `output`."""
    direct_source = sources[0].open("rb") if len(sources) == 1 else None
    decoder = None
    encoder = None
    try:
        decoder = subprocess.Popen(
            [zstd, "-q", "-dc", "-M512MB"],
            stdin=direct_source if direct_source is not None else subprocess.PIPE,
            stdout=subprocess.PIPE,
        )
        if direct_source is not None:
            direct_source.close()
            direct_source = None
        else:
            enlarge_pipe(decoder.stdin.fileno())
        enlarge_pipe(decoder.stdout.fileno())
        encoder = subprocess.Popen([zstd, "-q", "-c", *options], stdin=decoder.stdout, stdout=output)
        decoder.stdout.close()
        if decoder.stdin is not None:
            try:
                feed_sources_to_fd(sources, decoder.stdin.fileno())
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
        if direct_source is not None:
            direct_source.close()
        for child in (decoder, encoder):
            if child is not None and child.poll() is None:
                child.kill()
                child.wait()


def extract(zstd, sources, destination):
    """Decode the concatenated sources and extract the tar stream under `destination`."""
    decoder = subprocess.Popen([zstd, "-q", "-dc", "-M512MB"], stdin=subprocess.PIPE,
                               stdout=subprocess.PIPE, bufsize=0)
    enlarge_pipe(decoder.stdin.fileno())
    enlarge_pipe(decoder.stdout.fileno())
    fed = []

    def feed():
        # This process reads the decoder's output, so the parts are written
        # to its input from a second thread, in order.
        try:
            feed_sources_to_fd(sources, decoder.stdin.fileno())
            fed.append(True)
        except (BrokenPipeError, OSError):
            pass  # The decoder exited early; its status below reports why.
        finally:
            try:
                decoder.stdin.close()
            except (BrokenPipeError, OSError):
                pass

    feeder = threading.Thread(target=feed)
    feeder.start()
    members = 0
    write_ops = 0
    dest_str = os.fspath(destination)
    created_dirs = set()
    open_flags = os.O_WRONLY | os.O_CREAT | os.O_TRUNC | getattr(os, "O_NOFOLLOW", 0)
    utime_supports_fd = os.utime in os.supports_fd
    has_fchmod = hasattr(os, "fchmod")
    try:
        stream = TarPipeStream(decoder.stdout, EXTRACT_BUFFER_BYTES)
        try:
            with tarfile.TarFile(fileobj=stream, mode="r", stream=True) as archive:
                for member in archive:
                    parts = PurePosixPath(member.name).parts
                    if (not parts or parts[0] != "target" or ".." in parts
                            or not (member.isfile() or member.isdir())):
                        raise RuntimeError(f"refusing archive member {member.name!r}")
                    filtered = tarfile.data_filter(member, dest_str)
                    if filtered is None:
                        continue
                    target_path = os.path.join(dest_str, filtered.name.replace("/", os.sep)).rstrip(os.sep)
                    if filtered.isdir():
                        os.makedirs(target_path, exist_ok=True)
                        created_dirs.add(target_path)
                        if filtered.mode is not None:
                            os.chmod(target_path, filtered.mode)
                        if filtered.mtime is not None:
                            os.utime(target_path, (filtered.mtime, filtered.mtime))
                    else:
                        parent = os.path.dirname(target_path)
                        if parent and parent not in created_dirs:
                            os.makedirs(parent, exist_ok=True)
                            created_dirs.add(parent)
                        stream.seek(filtered.offset_data)
                        fd = os.open(target_path, open_flags, 0o600)
                        try:
                            if filtered.sparse is not None:
                                if filtered.size < 0:
                                    raise tarfile.ReadError("invalid sparse member size")
                                for offset, size in filtered.sparse:
                                    if offset < 0 or size < 0:
                                        raise tarfile.ReadError("invalid sparse member range")
                                    if size > 0:
                                        os.lseek(fd, offset, os.SEEK_SET)
                                        write_ops += stream.copy_to_fd(fd, size)
                                os.ftruncate(fd, filtered.size)
                            else:
                                write_ops += stream.copy_to_fd(fd, filtered.size)
                            if filtered.mode is not None and has_fchmod:
                                os.fchmod(fd, filtered.mode)
                            if filtered.mtime is not None and utime_supports_fd:
                                os.utime(fd, (filtered.mtime, filtered.mtime))
                        finally:
                            os.close(fd)
                        if filtered.mode is not None and not has_fchmod:
                            os.chmod(target_path, filtered.mode)
                        if filtered.mtime is not None and not utime_supports_fd:
                            os.utime(target_path, (filtered.mtime, filtered.mtime))
                    members += 1
        except (tarfile.TarError, ValueError) as error:
            raise RuntimeError(f"tar stream invalid: {error}") from error
        # Read past the end-of-archive marker, so a truncated or corrupt
        # frame after it still fails the decoder.
        stream.drain()
        decoded = decoder.wait()
        feeder.join()
        if decoded or not fed:
            raise RuntimeError(f"zstd failed: decoder={decoded} fed={bool(fed)}")
        if members == 0:
            raise RuntimeError("the archive holds no members")
        return members, write_ops
    finally:
        if decoder.poll() is None:
            decoder.kill()
            decoder.wait()
        feeder.join()


def split_transfer_in_place(source, destinations, temporaries):
    """Keep the first range in the run-owned staging file, copying only the rest.

    `source` is the encoder's temporary, already first in `temporaries`, never
    the input archive. Truncate it only after staging every remaining range.
    The balanced byte ranges and their publication are unchanged.
    """
    size = source.stat().st_size
    each = -(-size // len(destinations))
    with source.open("r+b", buffering=0) as whole:
        whole.seek(each)
        src_fd = whole.fileno()
        for destination in destinations[1:]:
            temporary = temporary_in(destination.parent)
            temporaries.append(temporary)
            with temporary.open("wb", buffering=0) as part:
                copy_fd_range(src_fd, part.fileno(), each)
        whole.truncate(min(each, size))
    return max(0, size - each)


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
        members, write_ops = extract(zstd, sources, args.destination)
        print(f"nextest-repack: mode=extract parts={args.parts} "
              f"input_bytes={sum(source.stat().st_size for source in sources)} "
              f"members={members} write_ops={write_ops} "
              f"wall_seconds={time.monotonic() - started:.3f}")
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
        copied_bytes = split_transfer_in_place(whole, destinations, temporaries)
        staged = temporaries
        # link publishes without overwriting a destination created since the
        # initial check. Every temporary is on its destination's filesystem.
        for temporary, destination in zip(staged, destinations):
            os.link(temporary, destination)
            published.append(destination)
        print(f"nextest-repack: mode={args.mode} parts={args.parts} "
              f"input_bytes={sum(source.stat().st_size for source in sources)} "
              f"output_bytes={sum(destination.stat().st_size for destination in destinations)} "
              f"split_copied_bytes={copied_bytes} "
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
