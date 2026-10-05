#!/usr/bin/env python3
"""Build and inspect local npm release evidence without running lifecycle scripts."""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys
import tarfile
import tempfile
import tomllib
from pathlib import Path


ASSERTIONS = ["npm-package-builds-without-publish", "npm-version-contract-is-checked"]
RUNTIME_FILES = ["package.json", "bin/orbit.js", "scripts/install-binary.js",
                 "release-signing.pub", "README.md", "LICENSE"]


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def local_contract(repo):
    package = json.loads((repo / "npm/package.json").read_text())
    server = json.loads((repo / "server.json").read_text())
    cargo = tomllib.loads((repo / "Cargo.toml").read_text())
    registry_packages = server.get("packages", [])
    if len(registry_packages) != 1:
        raise ValueError("server.json must declare exactly one npm package")
    registry = registry_packages[0]
    versions = {"cargo": cargo["workspace"]["package"]["version"],
                "npm": package.get("version"), "server": server.get("version"),
                "registry_package": registry.get("version")}
    if not all(isinstance(value, str) and value for value in versions.values()) or len(set(versions.values())) != 1:
        raise ValueError(f"local version drift: {versions}")
    if (package.get("name") != "@orbit-tools/cli" or registry.get("identifier") != package["name"]
            or package.get("mcpName") != "io.github.constellation-works/orbit"
            or server.get("name") != package["mcpName"]):
        raise ValueError("local npm/registry identity mismatch")
    if (registry.get("registryType") != "npm" or registry.get("transport") != {"type": "stdio"}
            or registry.get("packageArguments") != [
                {"type": "positional", "value": "mcp"}, {"type": "positional", "value": "serve"}]
            or "--operator" in json.dumps(server)):
        raise ValueError("registry must retain the non-operator npm stdio launch")
    if (package.get("bin") != {"orbit": "bin/orbit.js"}
            or package.get("scripts", {}).get("postinstall") != "node scripts/install-binary.js"):
        raise ValueError("npm runtime entry points changed")
    inputs = {path: digest(repo / path) for path in
              ["Cargo.toml", "server.json", *[f"npm/{name}" for name in RUNTIME_FILES]]}
    return package, versions, inputs


def inspect_archive(repo, archive):
    files = {}
    with tarfile.open(archive, "r:gz") as packed:
        for member in packed.getmembers():
            if member.isdir():
                continue
            name = member.name.removeprefix("package/")
            if (not member.name.startswith("package/") or not member.isfile()
                    or name in files or Path(name).is_absolute() or ".." in Path(name).parts):
                raise ValueError(f"unexpected packed runtime entry: {member.name}")
            content = packed.extractfile(member).read()
            source = (repo / "npm" / name).read_bytes()
            if name == "package.json":
                if json.loads(content) != json.loads(source):
                    raise ValueError("packed identity differs from local package.json")
            elif content != source:
                raise ValueError(f"packed file differs from candidate: {name}")
            files[name] = {"size": len(content), "sha256": hashlib.sha256(content).hexdigest()}
    missing = set(RUNTIME_FILES) - files.keys()
    if missing:
        raise ValueError(f"missing packed runtime files: {sorted(missing)}")
    if any(files[name]["size"] == 0 for name in RUNTIME_FILES):
        raise ValueError("packed runtime files must be nonempty")
    return files


def pack_row(stdout):
    """Return the single package row from `npm pack --json`.

    npm 11 prints a one-element array; npm 12 prints an object keyed by package name.
    """
    rows = json.loads(stdout)
    if isinstance(rows, dict):
        if len(rows) != 1:
            raise ValueError("npm pack must return exactly one package")
        (name, row), = rows.items()
        if not isinstance(row, dict) or row.get("name") != name:
            raise ValueError("npm pack package key differs from its name")
        return row
    if not isinstance(rows, list) or len(rows) != 1 or not isinstance(rows[0], dict):
        raise ValueError("npm pack must return exactly one package")
    return rows[0]


def validate_evidence(repo, body):
    """Recheck retained evidence against the consumer's current candidate."""
    package, versions, inputs = local_contract(repo)
    if (body.get("schema_version") != 1 or body.get("assertions") != ASSERTIONS
            or body.get("versions") != versions or body.get("input_sha256") != inputs):
        raise ValueError("missing or stale candidate npm evidence")
    archive = (repo / body["archive"]["path"]).resolve()
    if not archive.is_relative_to((repo / ".orbit/tmp").resolve()):
        raise ValueError("npm archive evidence must remain in candidate scratch")
    if digest(archive) != body["archive"]["sha256"]:
        raise ValueError("retained npm archive hash mismatch")
    files = inspect_archive(repo, archive)
    if body.get("packed_files") != files:
        raise ValueError("retained npm runtime inspection mismatch")
    pack = body["pack"]
    expected_command = ["npm", "pack", "./npm", "--ignore-scripts", "--offline", "--json",
                        "--pack-destination", str(archive.parent)]
    if pack.get("command") != expected_command or pack.get("exit_code") != 0:
        raise ValueError("npm evidence did not build the local package without scripts")
    row = pack_row(pack["stdout"])
    if (row.get("name") != package["name"] or row.get("version") != package["version"]
            or row.get("filename") != archive.name
            or {item["path"]: item["size"] for item in row["files"]}
            != {name: item["size"] for name, item in files.items()}):
        raise ValueError("npm pack identity/file inventory differs from retained archive")


def main():
    repo = Path.cwd().resolve()
    body = {"schema_version": 1, "assertions": []}
    try:
        _, versions, inputs = local_contract(repo)
        scratch = repo / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        destination = Path(tempfile.mkdtemp(prefix="npm-package-", dir=scratch))
        command = ["npm", "pack", "./npm", "--ignore-scripts", "--offline", "--json",
                   "--pack-destination", str(destination)]
        env = {**os.environ, "npm_config_cache": str(destination / "cache"),
               "npm_config_ignore_scripts": "true", "npm_config_audit": "false"}
        process = subprocess.run(command, cwd=repo, env=env, capture_output=True, text=True, timeout=120)
        body["pack"] = {"command": command, "exit_code": process.returncode,
                        "stdout": process.stdout, "stderr": process.stderr}
        if process.returncode != 0:
            raise ValueError("local npm pack failed")
        row = pack_row(process.stdout)
        if Path(row["filename"]).name != row["filename"]:
            raise ValueError("npm pack returned an invalid archive filename")
        archive = destination / row["filename"]
        body.update({"versions": versions, "input_sha256": inputs,
                     "archive": {"path": str(archive.relative_to(repo)), "sha256": digest(archive)},
                     "packed_files": inspect_archive(repo, archive), "assertions": ASSERTIONS})
        validate_evidence(repo, body)
    except (OSError, ValueError, KeyError, TypeError, AttributeError, tarfile.TarError,
            subprocess.SubprocessError) as error:
        body.update({"assertions": [], "error": str(error)})
        print(json.dumps(body, sort_keys=True))
        return 1
    print(json.dumps(body, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
