#!/usr/bin/env python3
"""Stub `ssh` with OpenSSH's `-L` argv contract, for the host-forward tests.

`ssh.json` beside this file configures it:

    {"log": "<path>",
     "release": "<path>",
     "hosts": {"<ssh target>": {"attach": <port or null>,
                                "spawn": <port or null>,
                                "exit": <status or null>,
                                "delay": <seconds>,
                                "hold": <bool>,
                                "bind_gate": <"first" or "every" or null>,
                                "bind_delay": <seconds>}}}

Every invocation first appends one JSON line to `log`: its pid, its mode
(`attach` for a bare `-N` forward, `spawn` when a remote command follows the
host), the host, the remote command and the full argv. Then:

- an unknown host exits 255, as ssh does when it cannot connect;
- `exit` sleeps `delay` seconds and exits with that status, never binding.
  With `hold` it first blocks until the `release` file exists, so a test
  decides when that ssh finishes;
- `bind_gate` holds either the first invocation or every invocation until
  `<release>-<pid>` exists. Tests take the selected port before releasing it,
  without choosing a scheduler delay. Bind syscall results (pid, mode, port,
  errno and parent) are recorded separately in `bind-events.jsonl`;
- otherwise it sleeps `bind_delay` seconds (authentication), then binds the
  `-L` listener. When the port is taken it exits 255 at once, as ssh does
  with `ExitOnForwardFailure=yes`. Once bound it runs `-o LocalCommand`
  through `$SHELL` when `-o PermitLocalCommand=yes` is given, as ssh does
  after setting up its forwards, and proxies every connection to the port
  configured for its mode. A null port closes each connection at once,
  which reads as "nothing is listening behind the forward".

It exits on SIGTERM, when its parent dies, or after LIFETIME seconds, so a
failed test cannot leak it.

`phases.jsonl` records child, bind, LocalCommand and proxy transitions with
monotonic timestamps. Callers print this evidence before cleaning up a failed
fixture; an absent child entry must not be confused with an identity timeout.
"""
import json
import os
import socket
import subprocess
import sys
import threading
import time
from pathlib import Path

LIFETIME = 300


def phase(stage, **details):
    path = Path(sys.argv[0]).resolve().parent / "phases.jsonl"
    event = {"stage": stage, "pid": os.getpid(), "parent": os.getppid(),
             "monotonic_ns": time.monotonic_ns(), **details}
    fd = os.open(path, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o600)
    try:
        os.write(fd, (json.dumps(event) + "\n").encode())
    finally:
        os.close(fd)


def main():
    phase("child_started")
    config = json.loads((Path(sys.argv[0]).resolve().parent / "ssh.json").read_text())
    argv = sys.argv[1:]
    local_port = parse_forward(argv)
    separator = argv.index("--")
    host = argv[separator + 1]
    command = " ".join(argv[separator + 2:])
    record = {
        "pid": os.getpid(),
        "mode": "attach" if "-N" in argv else "spawn",
        "host": host,
        "command": command,
        "argv": argv,
    }
    fd = os.open(config["log"], os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o644)
    os.write(fd, (json.dumps(record) + "\n").encode())
    os.close(fd)
    watchdog()
    host_config = config["hosts"].get(host)
    if host_config is None:
        sys.stderr.write(f"stub ssh: unknown host {host}\n")
        sys.exit(255)
    if host_config.get("exit") is not None:
        if host_config.get("hold"):
            wait_for(Path(config["release"]))
        time.sleep(host_config.get("delay", 0))
        sys.exit(host_config["exit"])
    time.sleep(host_config.get("bind_delay", 0))
    # A file handshake chooses the allocation/bind interleaving without a
    # scheduling delay. Each held child has its own release file.
    gate = host_config.get("bind_gate")
    hold_bind = gate == "every"
    if gate == "first":
        try:
            (Path(sys.argv[0]).resolve().parent / "bind-claimed").touch(exist_ok=False)
            hold_bind = True
        except FileExistsError:
            pass
    if hold_bind:
        phase("bind_gate")
        wait_for(Path(config["release"] + f"-{os.getpid()}"))
    target = host_config.get(record["mode"])
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    try:
        phase("bind_start", port=local_port)
        listener.bind(("127.0.0.1", local_port))
    except OSError as error:
        phase("bind_failed", port=local_port, errno=error.errno)
        bind_event(config, record, local_port, error.errno)
        sys.stderr.write(
            f"bind [127.0.0.1]:{local_port}: {error.strerror}\n"
            "Could not request local forwarding.\n"
        )
        sys.exit(255)
    listener.listen(64)
    phase("bound", port=local_port)
    bind_event(config, record, local_port, None)
    run_local_command(options(argv))
    phase("proxy_ready")
    while True:
        client, _ = listener.accept()
        phase("proxy_accepted")
        threading.Thread(target=proxy, args=(client, target), daemon=True).start()


def bind_event(config, record, port, error):
    """Keep syscall ownership evidence separate from invocation identities."""
    path = Path(config["log"]).with_name("bind-events.jsonl")
    fd = os.open(path, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o644)
    event = {"pid": record["pid"], "mode": record["mode"], "port": port,
             "errno": error, "parent": os.getppid()}
    os.write(fd, (json.dumps(event) + "\n").encode())
    os.close(fd)


def watchdog():
    parent = os.getppid()
    started = time.monotonic()

    def watch():
        while True:
            if os.getppid() != parent or time.monotonic() - started > LIFETIME:
                os._exit(0)
            time.sleep(0.2)

    threading.Thread(target=watch, daemon=True).start()


def wait_for(path):
    while not path.exists():
        time.sleep(0.02)


def options(argv):
    """Every `-o Key=value` before the destination, keyed in lower case."""
    found = {}
    for index, arg in enumerate(argv):
        if arg == "--":
            break
        if arg == "-o" and index + 1 < len(argv):
            key, _, value = argv[index + 1].partition("=")
            found.setdefault(key.lower(), value)
    return found


def run_local_command(found):
    command = found.get("localcommand")
    if command is None or found.get("permitlocalcommand") != "yes":
        return
    shell = os.environ.get("SHELL") or "/bin/sh"
    phase("local_command_start", shell=shell)
    result = subprocess.run([shell, "-c", command.replace("%%", "%")], check=False)
    phase("local_command_end", status=result.returncode)


def parse_forward(argv):
    for index, arg in enumerate(argv):
        if arg == "-L" and index + 1 < len(argv):
            parts = argv[index + 1].split(":")
            if len(parts) == 4:
                return int(parts[1])
    sys.stderr.write("stub ssh: expected -L host:local:host:remote\n")
    sys.exit(255)


def proxy(client, target):
    if target is None:
        phase("remote_absent")
        client.close()
        return
    try:
        remote = socket.create_connection(("127.0.0.1", target), timeout=5)
        remote.settimeout(None)
    except OSError as error:
        phase("remote_failed", errno=error.errno)
        client.close()
        return
    phase("remote_connected", port=target)

    def pump(source, sink):
        first = source is client
        try:
            while True:
                data = source.recv(65536)
                if not data:
                    break
                if first:
                    phase("probe_request", request=data.split(b"\r\n", 1)[0].decode("ascii", "replace"))
                    first = False
                sink.sendall(data)
        except OSError:
            pass
        try:
            sink.shutdown(socket.SHUT_WR)
        except OSError:
            pass

    left = threading.Thread(target=pump, args=(client, remote), daemon=True)
    right = threading.Thread(target=pump, args=(remote, client), daemon=True)
    left.start()
    right.start()
    left.join()
    right.join()
    client.close()
    remote.close()


if __name__ == "__main__":
    main()
