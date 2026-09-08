#!/usr/bin/env python3
"""Isolated Linux mechanism evidence, never a production sandbox or CI pass.

Prepare a NEW fixture directory, then run its probes with each backend:
  python3 live_read_probe.py prepare /tmp/orbit-live-read-UNIQUE
  python3 live_read_probe.py run /tmp/orbit-live-read-UNIQUE --backend landlock
  python3 live_read_probe.py run /tmp/orbit-live-read-UNIQUE --backend apparmor

AppArmor only STACKS an already operator-loaded profile. This script never loads
policy, creates namespaces, changes host configuration, or retries unconfined.
The baseline backend means no ADDITIONAL confinement; outer admission persists.
All negative reads use synthetic data. JSON distinguishes failures/unavailability.
"""

import argparse
import ctypes
import errno
import hashlib
import json
import mmap
import os
from pathlib import Path
import platform
import re
import shlex
import shutil
import signal
import subprocess
import sys
import time


MARKER = "ORBIT_SYNTHETIC_FORBIDDEN_BYTES"
GENERATED = "ORBIT_GENERATED_ALLOWED"
READ_EXEC = 13  # Landlock EXECUTE | READ_FILE | READ_DIR.
REFER = 1 << 13  # ABI 2: otherwise cross-directory link/rename is implicitly denied.
LIBC = ctypes.CDLL(None, use_errno=True)


class Ruleset(ctypes.Structure):
    _fields_ = [("handled", ctypes.c_uint64)]


class Rule(ctypes.Structure):
    _pack_ = 1
    _fields_ = [("access", ctypes.c_uint64), ("fd", ctypes.c_int32)]


def checked(value):
    if value < 0:
        number = ctypes.get_errno()
        raise OSError(number, os.strerror(number))
    return value


def landlock(grants):
    if platform.system() != "Linux" or platform.machine() != "x86_64":
        raise OSError(errno.ENOSYS, "probe supports Linux x86_64 only")
    abi = checked(LIBC.syscall(444, None, 0, 1))
    if abi < 2:
        raise OSError(errno.ENOSYS, "recovery probe requires Landlock ABI 2 REFER")
    attr = Ruleset(READ_EXEC | REFER)
    fd = checked(LIBC.syscall(444, ctypes.byref(attr), ctypes.sizeof(attr), 0))
    try:
        for grant in grants:
            anchor = os.open(grant["path"], os.O_PATH | os.O_CLOEXEC)
            try:
                rule = Rule(grant.get("access", READ_EXEC if grant["tree"] else 5), anchor)
                checked(LIBC.syscall(445, fd, 1, ctypes.byref(rule), 0))
            finally:
                os.close(anchor)
        checked(LIBC.prctl(38, 1, 0, 0, 0))
        checked(LIBC.syscall(446, fd, 0))
    finally:
        os.close(fd)


def invoke(argv, cwd, env, restrict=None, pass_fds=()):
    def setup():
        if restrict:
            try:
                restrict()
            except OSError as error:
                os.write(2, f"sandbox_setup: errno={error.errno}: {error}\n".encode())
                os._exit(125)

    started = time.monotonic()
    record = {"argv": argv}
    try:
        child = subprocess.Popen(
            argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True,
            preexec_fn=setup if restrict else None, pass_fds=pass_fds,
        )
        try:
            stdout, stderr = child.communicate(timeout=30)
            record.update(exit_code=child.returncode, stdout=stdout.decode(errors="replace"),
                          stderr=stderr.decode(errors="replace"))
        except subprocess.TimeoutExpired:
            os.killpg(child.pid, signal.SIGKILL)
            stdout, stderr = child.communicate()
            record.update(status="unavailable", reason="30 second timeout",
                          stdout=stdout.decode(errors="replace"),
                          stderr=stderr.decode(errors="replace"))
        finally:
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
    except OSError as error:
        record.update(status="unavailable", reason=str(error))
    record["duration_ms"] = round((time.monotonic() - started) * 1000, 3)
    if record.get("exit_code") == 125 and "sandbox_setup:" in record["stderr"]:
        record.update(status="unavailable", reason="sandbox setup denied or absent")
    return record


def safe_path(path):
    path = str(Path(path).resolve())
    if not re.fullmatch(r"/[A-Za-z0-9_./-]+", path):
        raise ValueError(f"fixture profile does not support path metacharacters: {path!r}")
    return path


def prepare(root):
    root.mkdir(mode=0o700)  # Refuse to overwrite or adopt an existing directory.
    workspace = root / "workspace"
    workspace.mkdir()
    (root / "outside.txt").write_text(MARKER)
    (workspace / "existing.env").write_text(MARKER)
    (workspace / "allowed.txt").write_text(GENERATED)
    os.link(workspace / "existing.env", workspace / "preexisting-alias.txt")
    (workspace / "worker.py").write_bytes(Path(__file__).read_bytes())
    for name in ["home", "cargo-home", "tmp", "src", "gh-config"]:
        (workspace / name).mkdir()
    (workspace / "Cargo.toml").write_text(
        '[package]\nname = "sandbox-probe"\nversion = "0.1.0"\nedition = "2021"\n'
    )
    (workspace / "src/main.rs").write_text('fn main() { println!("probe"); }\n')
    (workspace / "Makefile").write_text(
        'all:\n\tprintf ORBIT_GENERATED_ALLOWED > make-output.txt\n\tcat make-output.txt\n'
    )
    subprocess.run(["git", "init", "-q", str(workspace)], check=True)

    programs = {name: shutil.which(name) for name in
                ["git", "rg", "cargo", "rustup", "make", "gh", "cc", "cat", "sh"]}
    programs["python"] = sys.executable
    grants = []

    def grant(path, purpose, tree=None, executable=False):
        candidate = Path(path)
        if candidate.exists():
            is_tree = candidate.is_dir() if tree is None else tree
            if is_tree:
                access = READ_EXEC
            elif candidate.is_dir():
                access = 8  # Directory listing only, no inherited file reads.
            elif executable:
                access = 5
            else:
                access = 4
            entry = {"path": safe_path(candidate), "tree": is_tree,
                     "directory": candidate.is_dir(), "access": access, "purpose": purpose}
            if not any(item["path"] == entry["path"] for item in grants):
                grants.append(entry)

    for program in programs.values():
        if program:
            grant(program, "exact executable (symlinks resolved)", executable=True)
    for path in ["/usr/lib/x86_64-linux-gnu", "/usr/lib/python3.12",
                 "/usr/lib/git-core", "/usr/share/git-core"]:
        grant(path, "distribution runtime libraries or git helpers/templates")
    for path in ["/etc/ld.so.cache", "/dev/null", "/dev/urandom"]:
        grant(path, "loader or exact runtime device")
    for path in ["/etc/nsswitch.conf", "/etc/hosts", "/etc/resolv.conf",
                 "/etc/gai.conf", "/etc/ssl/certs/ca-certificates.crt"]:
        grant(path, "explicit resolver or CA file; canonical target only")

    rustup_home = Path(os.environ.get("RUSTUP_HOME", str(Path.home() / ".rustup")))
    grant(rustup_home / "settings.toml", "rustup selection (no home tree grant)")
    grant(rustup_home / "toolchains", "installed toolchain names only", tree=False)
    if programs["rustup"]:
        query = subprocess.run([programs["rustup"], "which", "rustc"],
                               capture_output=True, text=True, timeout=15)
        if query.returncode == 0:
            toolchain = Path(query.stdout.strip()).parent.parent
            grant(toolchain / "bin", "selected installed toolchain executables")
            grant(toolchain / "lib", "selected installed toolchain libraries/sysroot")

    profile_name = "orbit-live-read-" + hashlib.sha256(str(root).encode()).hexdigest()[:16]
    env = {"PATH": ":".join(dict.fromkeys(
               str(Path(p).parent) for p in programs.values() if p)),
           "HOME": str(workspace / "home"), "CARGO_HOME": str(workspace / "cargo-home"),
           "RUSTUP_HOME": str(rustup_home), "RUSTUP_NO_UPDATE_CHECK": "1",
           "TMPDIR": str(workspace / "tmp"), "GH_CONFIG_DIR": str(workspace / "gh-config"),
           "GIT_CONFIG_NOSYSTEM": "1", "GIT_TERMINAL_PROMPT": "0", "LC_ALL": "C",
           "GIT_SSL_CAINFO": "/etc/ssl/certs/ca-certificates.crt",
           "PYTHONDONTWRITEBYTECODE": "1"}
    manifest = {"schemaVersion": 1, "root": str(root), "workspace": str(workspace),
                "profile": profile_name, "programs": programs, "host_read_grants": grants,
                "environment": env}
    import live_read_recovery

    credential, _server_root = live_read_recovery.prepare(root, manifest)
    grants.append({"path": str(credential), "tree": False, "directory": False,
                   "access": 4, "purpose": "synthetic local endpoint credential only"})
    (root / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")

    # A fixed fixture policy, NOT an implementation of Orbit glob evaluation.
    # No include abstractions: every host read is reviewable in the manifest.
    lines = [f"profile {profile_name} flags=(mediate_deleted) {{",
             "  network inet stream,", "  network inet6 stream,",
             "  network inet dgram,", "  network inet6 dgram,",
             f'  signal (send, receive) peer={profile_name},',
             f'  "{workspace}/" rw,', f'  "{workspace}/**" rwklm,',
             f'  "{workspace}/**" ix,',
             f'  deny "{workspace}/*.env" rmx,',
             f'  deny "{workspace}/**/*.env" rmx,',
             f'  deny "{workspace}/.env" rmx,',
             f'  deny "{workspace}/**/.env" rmx,']
    for entry in grants:
        path = entry["path"]
        if entry["tree"]:
            lines += [f'  "{path}/" r,', f'  "{path}/**" rmix,']
        else:
            if path == "/dev/null":
                permission = "rw"
            elif entry["access"] & 1:
                permission = "rmix"
            else:
                permission = "r"
            suffix = "/" if entry["directory"] else ""
            lines.append(f'  "{path}{suffix}" {permission},')
    lines.append("}")
    (root / "profile.apparmor").write_text("\n".join(lines) + "\n")
    return manifest


def child_probe(action, root, inherited_fd):
    workspace = root / "workspace"
    target = root / "outside.txt"
    try:
        if action == "preexisting_alias":
            target = workspace / "preexisting-alias.txt"
        elif action == "existing_denied":
            target = workspace / "existing.env"
        elif action in ["create_denied", "create_allowed"]:
            target = workspace / (".env" if action == "create_denied" else "generated.txt")
            target.write_text(MARKER if action == "create_denied" else GENERATED)
        elif action in ["rename_denied", "rename_open_fd", "rename_mmap", "rename_cached"]:
            source = workspace / "rename-source.txt"
            source.write_text(MARKER)
            target = workspace / "renamed.env"
            if action != "rename_denied":
                inherited_fd = os.open(source, os.O_RDONLY)
            if action == "rename_mmap":
                mapping = mmap.mmap(inherited_fd, 0, access=mmap.ACCESS_READ)
            if action == "rename_cached":
                cached = os.read(inherited_fd, 4096).decode()
            source.rename(target)
        elif action == "symlink_outside":
            target = workspace / "outside-link.txt"
            target.symlink_to(root / "outside.txt")
        elif action == "hardlink_denied":
            target = workspace / "hardlink-allowed.txt"
            try:
                os.link(workspace / "existing.env", target)
            except OSError as error:
                return {"operation": "link", "errno": error.errno, "outcome": "denied"}
        elif action == "descendant_outside":
            pid = os.fork()
            if pid:
                _, status = os.waitpid(pid, 0)
                return {"outcome": "descendant_complete", "wait_status": status}
            os.setsid()
        elif action == "allowed":
            target = workspace / "allowed.txt"
    except OSError as error:
        return {"outcome": "setup_failed", "errno": error.errno, "reason": str(error)}

    try:
        if action == "rename_cached":
            content = cached
        elif action == "rename_mmap":
            content = mapping[:].decode()
        elif action in ["inherited_fd", "rename_open_fd"]:
            content = os.read(inherited_fd, 4096).decode()
        else:
            with target.open("rb") as source:
                content = source.read(4096).decode()
        result = {"outcome": "read", "content": content}
    except OSError as error:
        result = {"outcome": "denied", "operation": "open/read", "errno": error.errno}
    if action == "descendant_outside":
        os.write(1, (json.dumps(result) + "\n").encode())
        os._exit(0)
    return result


def classify(record, expected):
    """Classify completed bodies without mistaking setup failures for denial."""
    leaked = MARKER in record.get("stdout", "") or MARKER in record.get("stderr", "")
    if expected == "deny" and leaked:
        record.update(status="fail", reason="forbidden bytes reached output")
        return record
    if record.get("status") == "unavailable":
        return record
    if expected == "positive":
        record["status"] = "pass" if record["exit_code"] == 0 else "fail"
        return record
    if expected not in {"allow", "deny", "observation"}:
        raise ValueError(f"unknown expectation: {expected}")
    try:
        outcomes = [json.loads(line) for line in record["stdout"].splitlines()]
        if not all(isinstance(item, dict) for item in outcomes):
            raise ValueError("worker records must be objects")
        if any(item.get("wait_status") != 0 for item in outcomes
               if item.get("outcome") == "descendant_complete"):
            raise ValueError("descendant did not exit successfully")
        outcomes = [item for item in outcomes if item.get("outcome") != "descendant_complete"]
    except (ValueError, AttributeError, KeyError):
        outcomes = []
    if record.get("exit_code") != 0 or not outcomes or any(
        item.get("outcome") == "setup_failed" for item in outcomes
    ):
        record.update(status="unavailable", reason="probe body did not complete")
    elif expected == "observation":
        record["status"] = "observed"
    elif expected == "deny":
        denied = all(item.get("outcome") == "denied" and item.get("errno") in
                     [errno.EACCES, errno.EPERM] for item in outcomes)
        record["status"] = "pass" if denied and not leaked else "fail"
    else:
        record["status"] = "pass" if all(item.get("content") == GENERATED
                                         for item in outcomes) else "fail"
    return record


def contract_assessment(probes):
    """Keep stronger semantics visible without choosing a weaker contract."""
    groups = {
        "pathname_acquisition": ["outside", "existing_denied", "create_denied",
                                 "rename_denied", "symlink_outside", "descendant_outside"],
        "alias_provenance": ["hardlink_denied", "preexisting_alias"],
        "descriptor_acquisition": ["inherited_fd"],
        "later_access_revocation": ["rename_open_fd", "rename_mmap"],
        "previously_acquired_bytes": ["rename_cached"],
        "generated_files": ["allowed", "create_allowed"],
    }
    assessment = {}
    for group, names in groups.items():
        statuses = []
        for name in names:
            record = dict(probes.get(name, {"status": "unavailable"}))
            if record.get("status") == "observed":
                record.pop("status")
            expected = "allow" if group == "generated_files" else "deny"
            statuses.append(classify(record, expected)["status"])
        assessment[group] = ("fail" if "fail" in statuses else
                             "unavailable" if "unavailable" in statuses else "pass")
    return assessment


def run(root, backend, local_recovery=False):
    manifest = json.loads((root / "manifest.json").read_text())
    if manifest["root"] != str(root):
        raise ValueError("fixture was moved; prepare a new profile instead")
    workspace = Path(manifest["workspace"])
    env = manifest["environment"]
    programs = manifest["programs"]
    if workspace.resolve() != root / "workspace":
        raise ValueError("workspace must be the prepared fixture child directory")
    grants = manifest["host_read_grants"] + [
        {"path": str(workspace), "tree": True, "access": READ_EXEC | REFER}
    ]
    restriction = None
    if backend == "landlock":
        restriction = lambda: landlock(grants)
    elif backend == "apparmor":
        # Explicit stack API preserves every outer profile. No change_profile fallback.
        def restriction():
            apparmor = ctypes.CDLL("libapparmor.so.1", use_errno=True)
            try:
                apparmor.aa_stack_onexec.argtypes = [ctypes.c_char_p]
            except AttributeError as error:
                raise OSError(errno.ENOSYS, "AppArmor stacking API unavailable") from error
            checked(apparmor.aa_stack_onexec(manifest["profile"].encode()))

    results = {"schemaVersion": 1, "backend": backend, "kernel": platform.release(),
               "architecture": platform.machine(), "manifest": manifest, "probes": {}}
    probes = results["probes"]
    actions = ["outside", "existing_denied", "create_denied", "rename_denied",
               "symlink_outside", "hardlink_denied", "descendant_outside", "allowed",
               "create_allowed", "inherited_fd", "rename_open_fd", "rename_mmap",
               "preexisting_alias", "rename_cached"]
    for action in actions:
        # Only remove paths this probe itself creates; fixtures are run-owned.
        for name in [".env", "generated.txt", "rename-source.txt", "renamed.env",
                     "outside-link.txt", "hardlink-allowed.txt"]:
            (workspace / name).unlink(missing_ok=True)
        worker = [programs["python"], str(workspace / "worker.py"), "child", str(root),
                  "--action", action]
        fds = ()
        if action == "inherited_fd":
            fd = os.open(root / "outside.txt", os.O_RDONLY)
            fds = (fd,)
            worker += ["--inherited-fd", str(fd)]
        argv = [programs["git"], "-c", "alias.orbitsecurityprobe=!" + shlex.join(worker),
                "orbitsecurityprobe"]
        try:
            record = invoke(argv, workspace, env, restriction, fds)
        finally:
            for fd in fds:
                os.close(fd)
        if action in ["allowed", "create_allowed"]:
            expected = "allow"
        elif action in ["inherited_fd", "rename_open_fd", "rename_mmap", "rename_cached"]:
            expected = "observation"
        else:
            expected = "deny"
        probes[action] = classify(record, expected)

    commands = {
        "git_status": [programs["git"], "status", "--porcelain"],
        "rg_generated": [programs["rg"], "--no-mmap", GENERATED, "allowed.txt"],
        "make_generated": [programs["make"], "-s"],
        "cargo_check_offline": [programs["cargo"], "check", "--offline"],
        "gh_version": [programs["gh"], "--version"],
        "gh_public_api_without_credentials": [programs["gh"], "api", "meta"],
        "git_network_tls": [programs["git"], "ls-remote", "https://github.com/git/git.git", "HEAD"],
    }
    if local_recovery:
        import live_read_recovery

        commands.pop("gh_public_api_without_credentials")
        commands.pop("git_network_tls")
        try:
            results["local_recovery"] = live_read_recovery.run(
                root, manifest, restriction, invoke, classify)
        except OSError as error:
            results["local_recovery"] = {"probes": {
                name: {"status": "unavailable", "reason": str(error)} for name in
                ["git_authenticated_local_clone", "cargo_cold_local_dependency",
                 "gh_authenticated_local_api"]}}
        probes.update(results["local_recovery"]["probes"])
    for name, argv in commands.items():
        if not argv[0]:
            probes[name] = {"status": "unavailable", "reason": "program not installed"}
        else:
            probes[name] = classify(invoke(argv, workspace, env, restriction), "positive")
    counts = {status: sum(item["status"] == status for item in probes.values())
              for status in ["pass", "fail", "unavailable", "observed"]}
    results["counts"] = counts
    results["contract_assessment"] = contract_assessment(probes)
    results["complete_contract_proven"] = False  # Operator race/platform/credential gates remain.
    return results


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=["prepare", "run", "child"])
    parser.add_argument("root", type=Path)
    parser.add_argument("--backend", choices=["baseline", "landlock", "apparmor"],
                        default="apparmor")
    parser.add_argument("--local-recovery", action="store_true",
                        help="use authenticated loopback fixtures instead of public network probes")
    parser.add_argument("--action")
    parser.add_argument("--inherited-fd", type=int, default=-1)
    args = parser.parse_args()
    root = Path(safe_path(args.root))
    if args.operation == "prepare":
        result = prepare(root)
    elif args.operation == "child":
        result = child_probe(args.action, root, args.inherited_fd)
    else:
        result = run(root, args.backend, args.local_recovery)
    print(json.dumps(result, indent=None if args.operation == "child" else 2))
    if args.operation == "run":
        # Evidence collection must not look like a green full-contract test.
        if result["counts"]["fail"]:
            return 1
        if result["counts"]["unavailable"]:
            return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
