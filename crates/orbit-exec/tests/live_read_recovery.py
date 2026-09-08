"""Local, synthetic recovery fixture for live_read_probe; never real credentials.

The HTTP server is a separate forked process, outside the tested restriction.
Only its loopback endpoint and one fixture credential file are admitted to clients.
"""

from contextlib import contextmanager
import ctypes
from functools import partial
from http.server import HTTPServer, SimpleHTTPRequestHandler
import json
import os
from pathlib import Path
import shlex
import signal
import subprocess
import tempfile


TOKEN = "orbit-local-fixture-token-not-a-real-credential"
API_RESPONSE = '{"fixture":"authenticated-recovery"}'


class FixtureHandler(SimpleHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_GET(self):
        authenticated = self.headers.get("Authorization") == "Bearer " + TOKEN
        with (Path(self.directory) / "requests.jsonl").open("a") as log:
            log.write(json.dumps({"path": self.path, "authenticated": authenticated}) + "\n")
        if not authenticated:
            self.send_error(401)
        elif self.path == "/api":
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(API_RESPONSE.encode())
        else:
            super().do_GET()


def prepare(root, manifest):
    server_root = root / "recovery-server"
    server_root.mkdir()
    credential = root / "recovery-token.txt"
    credential.write_text(TOKEN)
    credential.chmod(0o600)
    source = server_root / "source"
    source.mkdir()
    (source / "Cargo.toml").write_text(
        '[package]\nname="fixture-dependency"\nversion="0.1.0"\nedition="2021"\n'
        '[lib]\npath="lib.rs"\n')
    (source / "lib.rs").write_text('pub fn value() -> u8 { 42 }\n')
    env = manifest["environment"]
    git = manifest["programs"]["git"]
    commands = [
        [git, "init", "-q", str(source)],
        [git, "-C", str(source), "add", "Cargo.toml", "lib.rs"],
        [git, "-C", str(source), "-c", "user.name=Fixture", "-c",
         "user.email=fixture@invalid", "-c", "commit.gpgsign=false", "commit", "-qm", "fixture"],
        [git, "clone", "-q", "--bare", str(source), str(server_root / "dependency.git")],
        [git, "--git-dir", str(server_root / "dependency.git"), "update-server-info"],
    ]
    for argv in commands:
        subprocess.run(argv, env=env, check=True, capture_output=True, timeout=15)
    return credential, server_root


@contextmanager
def endpoint(server_root):
    server = HTTPServer(("127.0.0.1", 0), partial(FixtureHandler, directory=str(server_root)))
    parent_pid = os.getpid()
    pid = os.fork()
    if pid == 0:
        try:
            # This Linux-only fixture must not leave its server after collector death.
            libc = ctypes.CDLL(None, use_errno=True)
            if libc.prctl(1, signal.SIGKILL, 0, 0, 0) != 0 or os.getppid() != parent_pid:
                os._exit(125)
            server.serve_forever()
        finally:
            os._exit(0)
    address = f"http://127.0.0.1:{server.server_port}"
    server.server_close()
    try:
        yield address
    finally:
        os.kill(pid, signal.SIGTERM)
        os.waitpid(pid, 0)


def run(root, manifest, restriction, invoke, classify):
    """Use fresh clone/build/cache directories for each backend's recovery."""
    credential = root / "recovery-token.txt"
    server_root = root / "recovery-server"
    probes = {}
    workspace = Path(manifest["workspace"])
    programs = manifest["programs"]
    env = dict(manifest["environment"])
    # The caller's restriction receives this exact grant before any child starts.
    grant = {"path": str(credential), "tree": False, "directory": False,
             "access": 4, "purpose": "synthetic local endpoint credential only"}
    if grant not in manifest["host_read_grants"]:
        raise ValueError("local recovery credential must be declared during prepare")
    request_log = server_root / "requests.jsonl"
    previous_lines = len(request_log.read_text().splitlines()) if request_log.exists() else 0
    with endpoint(server_root) as address:
        project = Path(tempfile.mkdtemp(prefix="recovery-", dir=workspace))
        cargo_home = project / "cargo-home"
        cargo_home.mkdir()
        env["CARGO_HOME"] = str(cargo_home)
        (project / "Cargo.toml").write_text(
            '[package]\nname="recovery-client"\nversion="0.1.0"\nedition="2021"\n'
            '[dependencies]\nfixture-dependency={git="' + address + '/dependency.git"}\n'
            '[lib]\npath="lib.rs"\n')
        (project / "lib.rs").write_text(
            'pub fn recovered() -> u8 { fixture_dependency::value() }\n')
        env.update(CARGO_NET_GIT_FETCH_WITH_CLI="true", GIT_CONFIG_COUNT="1",
                   GIT_CONFIG_KEY_0="http.extraHeader")
        prefix = ('token=$(cat ' + shlex.quote(str(credential)) + '); '
                  'export GIT_CONFIG_VALUE_0="Authorization: Bearer $token"; ')
        commands = {
            "git_authenticated_local_clone": prefix + "exec " + shlex.join([
                programs["git"], "clone", address + "/dependency.git", str(project / "recovered-clone")]),
            "cargo_cold_local_dependency": prefix + "exec " + shlex.join([
                programs["cargo"] or "cargo", "check", "--manifest-path", str(project / "Cargo.toml")]),
            "gh_authenticated_local_api": prefix + 'export GH_TOKEN="$token"; exec ' +
                shlex.join([programs["gh"] or "gh", "api", address + "/api", "-H"]) +
                ' "Authorization: Bearer $token"',
        }
        for name, command in commands.items():
            program = "cargo" if name.startswith("cargo") else "gh" if name.startswith("gh") else "git"
            if not programs[program]:
                probes[name] = {"status": "unavailable", "reason": "program not installed"}
                continue
            result = classify(invoke([programs["sh"], "-c", command], workspace, env,
                                     restriction), "positive")
            if name.startswith("gh") and result["status"] == "pass":
                if result["stdout"].strip() != API_RESPONSE:
                    result.update(status="fail", reason="fixture API response missing")
            probes[name] = result
    return {"probes": probes, "endpoint": address, "credential_grant": grant,
            "requests": [json.loads(line) for line in request_log.read_text().splitlines()[previous_lines:]]
                        if request_log.exists() else [],
            "limits": "Loopback HTTP with synthetic auth; not TLS, real service auth or production admission."}
