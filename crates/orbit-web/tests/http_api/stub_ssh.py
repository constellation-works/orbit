#!/usr/bin/env python3
"""Stub `ssh` with OpenSSH's `-L` argv contract, for the host-forward tests.

`ssh.json` beside this file configures it:

    {"log": "<path>",
     "hosts": {"<ssh target>": {"attach": <port or null>,
                                "spawn": <port or null>,
                                "exit": <status or null>,
                                "delay": <seconds>}}}

Every invocation first appends one JSON line to `log`: its pid, its mode
(`attach` for a bare `-N` forward, `spawn` when a remote command follows the
host), the host, the remote command and the full argv. Then:

- an unknown host exits 255, as ssh does when it cannot connect;
- `exit` sleeps `delay` seconds and exits with that status;
- otherwise it binds the `-L` listener and proxies every connection to the
  port configured for its mode. A null port closes each connection at once,
  which reads as "nothing is listening behind the forward".

It exits on SIGTERM, when its parent dies, or after LIFETIME seconds, so a
failed test cannot leak it.
"""
import json
import os
import socket
import sys
import threading
import time
from pathlib import Path

LIFETIME = 300


def main():
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
        time.sleep(host_config.get("delay", 0))
        sys.exit(host_config["exit"])
    target = host_config.get(record["mode"])
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("127.0.0.1", local_port))
    listener.listen(64)
    while True:
        client, _ = listener.accept()
        threading.Thread(target=proxy, args=(client, target), daemon=True).start()


def watchdog():
    parent = os.getppid()
    started = time.monotonic()

    def watch():
        while True:
            if os.getppid() != parent or time.monotonic() - started > LIFETIME:
                os._exit(0)
            time.sleep(0.2)

    threading.Thread(target=watch, daemon=True).start()


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
        client.close()
        return
    try:
        remote = socket.create_connection(("127.0.0.1", target), timeout=5)
        remote.settimeout(None)
    except OSError:
        client.close()
        return

    def pump(source, sink):
        try:
            while True:
                data = source.recv(65536)
                if not data:
                    break
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
