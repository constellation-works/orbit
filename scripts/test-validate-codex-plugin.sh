#!/usr/bin/env bash
# Exercise the repository-owned Codex plugin validator at its supported
# Python interpreter boundary.
set -euo pipefail

repo_root="${1:-${ORBIT_REPO_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}}"
validator="$repo_root/scripts/validate-codex-plugin.sh"

if ! command -v python3 >/dev/null 2>&1; then
  echo "test-validate-codex-plugin: required binary 'python3' not on PATH" >&2
  exit 2
fi

python3 - "$validator" <<'PY'
import ast
import sys
from pathlib import Path

validator = Path(sys.argv[1])
text = validator.read_text(encoding="utf-8")
marker = "<<'PY'\n"
start = text.find(marker)
if start < 0:
    print("test-validate-codex-plugin: validator embedded Python not found", file=sys.stderr)
    raise SystemExit(1)
start += len(marker)
end = text.find("\nPY", start)
if end < 0:
    print("test-validate-codex-plugin: validator embedded Python not terminated", file=sys.stderr)
    raise SystemExit(1)
source = text[start:end]
try:
    tree = ast.parse(source, filename=str(validator), feature_version=(3, 9))
except SyntaxError as exc:
    print(f"test-validate-codex-plugin: validator is not Python 3.9 compatible: {exc}", file=sys.stderr)
    raise SystemExit(1)
first = tree.body[0] if tree.body else None
has_future = (
    isinstance(first, ast.ImportFrom)
    and first.module == "__future__"
    and any(alias.name == "annotations" for alias in first.names)
)
if not has_future:
    print("test-validate-codex-plugin: validator must defer annotations for Python 3.9", file=sys.stderr)
    raise SystemExit(1)
PY

fixture_root="$(mktemp -d "${TMPDIR:-/tmp}/orbit-codex-plugin.XXXXXX")"
trap 'rm -rf -- "$fixture_root"' EXIT

# Every fixture, including the per-case clones below, lives under fixture_root
# so concurrent runs sharing one TMPDIR never collide and the trap removes them.
base_fixture="$fixture_root/base"
"$repo_root/scripts/sync-plugin-skills.sh" --check >/dev/null
mkdir -p "$base_fixture/.agents/plugins"
cp -R "$repo_root/plugin" "$base_fixture/plugin"
cp "$repo_root/.agents/plugins/marketplace.json" "$base_fixture/.agents/plugins/marketplace.json"

"$validator" "$base_fixture"

python3 - "$repo_root" "$validator" "$base_fixture" "$fixture_root" <<'PY'
from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

repo_root = Path(sys.argv[1]).resolve()
validator = Path(sys.argv[2])
base_fixture = Path(sys.argv[3])
fixture_root = Path(sys.argv[4])
errors: list[str] = []


def run_validator(root: Path, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(validator), str(root)],
        check=False,
        capture_output=True,
        text=True,
        env=env,
    )


def clone_fixture(name: str) -> Path:
    dest = fixture_root / name
    shutil.copytree(base_fixture, dest, symlinks=True)
    return dest


def expect_failure(root: Path, needle: str, label: str, env: dict[str, str] | None = None) -> None:
    result = run_validator(root, env)
    if result.returncode == 0:
        errors.append(f"{label}: expected validator failure")
        return
    combined = result.stdout + result.stderr
    if "Traceback" in combined:
        errors.append(f"{label}: validator leaked a traceback\n{combined}")
        return
    if needle not in combined:
        errors.append(f"{label}: stderr/stdout missing {needle!r}\n{combined}")


broken = clone_fixture("codex-stale-latest-pin")
payload = json.loads((broken / "plugin" / ".codex-plugin" / "plugin.json").read_text(encoding="utf-8"))
payload["mcpServers"]["orbit"]["args"] = ["-y", "@orbit-tools/cli@latest", "mcp", "serve"]
(broken / "plugin" / ".codex-plugin" / "plugin.json").write_text(json.dumps(payload), encoding="utf-8")
expect_failure(broken, "stale launch pin", "stale @latest pin")

broken = clone_fixture("codex-missing-orbit-args")
payload = json.loads((broken / "plugin" / ".codex-plugin" / "plugin.json").read_text(encoding="utf-8"))
del payload["mcpServers"]["orbit"]["args"]
(broken / "plugin" / ".codex-plugin" / "plugin.json").write_text(json.dumps(payload), encoding="utf-8")
expect_failure(broken, "mcpServers.orbit.args", "absent orbit args")

broken = clone_fixture("codex-malformed-orbit-args")
payload = json.loads((broken / "plugin" / ".codex-plugin" / "plugin.json").read_text(encoding="utf-8"))
payload["mcpServers"]["orbit"]["args"] = "mcp serve"
(broken / "plugin" / ".codex-plugin" / "plugin.json").write_text(json.dumps(payload), encoding="utf-8")
expect_failure(broken, "mcpServers.orbit.args", "malformed orbit args")

broken = clone_fixture("codex-empty-orbit-args")
payload = json.loads((broken / "plugin" / ".codex-plugin" / "plugin.json").read_text(encoding="utf-8"))
payload["mcpServers"]["orbit"]["args"] = []
(broken / "plugin" / ".codex-plugin" / "plugin.json").write_text(json.dumps(payload), encoding="utf-8")
expect_failure(broken, "mcpServers.orbit must launch", "empty orbit args")

broken = clone_fixture("codex-helper-only")
payload = json.loads((broken / "plugin" / ".codex-plugin" / "plugin.json").read_text(encoding="utf-8"))
payload["mcpServers"] = {"helper": {"command": "true", "args": []}}
(broken / "plugin" / ".codex-plugin" / "plugin.json").write_text(json.dumps(payload), encoding="utf-8")
expect_failure(broken, "mcpServers.orbit", "absent orbit server")

broken = clone_fixture("codex-missing-manifest")
(broken / "plugin" / ".codex-plugin" / "plugin.json").unlink()
expect_failure(broken, "plugin/.codex-plugin/plugin.json is missing", "missing Codex manifest")

broken = clone_fixture("codex-nonutf8-manifest")
(broken / "plugin" / ".codex-plugin" / "plugin.json").write_bytes(b"\xff\xfe\x00bad")
expect_failure(broken, "not valid JSON", "non-UTF8 Codex manifest")

broken = clone_fixture("codex-nonutf8-skill")
(broken / "plugin" / "skills" / "orbit" / "SKILL.md").write_bytes(b"\xff\xfe\x00bad")
expect_failure(broken, "cannot be read", "non-UTF8 SKILL.md")

broken = clone_fixture("codex-owner-drift")
payload = json.loads((broken / "plugin" / ".codex-plugin" / "plugin.json").read_text(encoding="utf-8"))
payload["author"] = {"name": "danieljhkim", "url": "https://github.com/danieljhkim"}
(broken / "plugin" / ".codex-plugin" / "plugin.json").write_text(json.dumps(payload), encoding="utf-8")
expect_failure(broken, "constellation-works", "owner drift")

broken = clone_fixture("codex-malformed-urls")
manifest_path = broken / "plugin" / ".codex-plugin" / "plugin.json"
payload = json.loads(manifest_path.read_text(encoding="utf-8"))
payload["author"]["url"] = "https://["
for field in ("websiteURL", "privacyPolicyURL", "termsOfServiceURL"):
    payload["interface"][field] = "https://["
manifest_path.write_text(json.dumps(payload), encoding="utf-8")
for field in ("author.url", "interface.websiteURL", "interface.privacyPolicyURL", "interface.termsOfServiceURL"):
    expect_failure(broken, f"plugin.json.{field} must be an absolute https:// URL", field)

# HOME=/ is not a user-specific home. A substring match would reject every
# slash-containing value, including the @orbit-tools/cli pin.
root_home = dict(os.environ)
root_home["HOME"] = "/"
root_result = run_validator(base_fixture, root_home)
if root_result.returncode != 0:
    errors.append(
        "valid package when HOME is /: expected validator success, "
        f"got {root_result.returncode}\n{root_result.stdout + root_result.stderr}"
    )

embedded_home = "/srv/orbit-plugin-home"
embedded = clone_fixture("embedded-home")
manifest_path = embedded / "plugin" / ".codex-plugin" / "plugin.json"
manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
pinned = manifest["mcpServers"]["orbit"]["args"]
manifest["mcpServers"]["orbit"]["args"] = [
    arg if "@orbit-tools/cli@" not in arg else arg + embedded_home for arg in pinned
]
manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
home_env = dict(os.environ)
home_env["HOME"] = embedded_home
expect_failure(embedded, "user-specific absolute home path", "embedded home in launch arg", home_env)

if errors:
    print("test-validate-codex-plugin: failed", file=sys.stderr)
    for error in errors:
        print(f"- {error}", file=sys.stderr)
    raise SystemExit(1)

print("test-validate-codex-plugin: current package and validator cases passed")
PY
