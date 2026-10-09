#!/usr/bin/env python3
"""Fail when a crate manifest declares a dependency its own sources never use.

A declaration counts as used when its import name (the table key with `-`
read as `_`) appears as a path (`name::`), as `use name`, or as
`extern crate name` in the sources that section links against:

- [dependencies]: src/. A normal dependency that only test targets use is
  reported as misplaced and belongs in [dev-dependencies].
- [dev-dependencies]: src/, tests/, benches/ and examples/.
- [build-dependencies]: build.rs.

[target.*] tables follow the same rules. A [workspace.dependencies] entry that
no member declares is reported as orphaned. A dependency kept only to enable a
feature goes in FEATURE_ONLY with its reason.

Usage: check-unused-dependencies.py [repo_root]
"""

import pathlib
import re
import sys
import tomllib


# (crate, dependency) pairs declared only to enable a feature. Each needs a
# reason. A stale entry (undeclared, or now referenced) fails the check.
FEATURE_ONLY = {}

SECTIONS = ("dependencies", "dev-dependencies", "build-dependencies")
SCOPES = {
    "dependencies": ("src",),
    "dev-dependencies": ("src", "tests", "benches", "examples"),
    "build-dependencies": ("build.rs",),
}
TEST_SCOPE = ("tests", "benches", "examples")


def rust_sources(crate_dir, scope):
    for entry in scope:
        path = crate_dir / entry
        if path.is_file():
            yield path
        elif path.is_dir():
            yield from path.rglob("*.rs")


def is_referenced(crate_dir, scope, dependency):
    name = re.escape(dependency.replace("-", "_"))
    pattern = re.compile(rf"(?<!\w){name}::|\buse\s+{name}\b|\bextern\s+crate\s+{name}\b")
    return any(pattern.search(source.read_text()) for source in rust_sources(crate_dir, scope))


def declarations(manifest):
    data = tomllib.loads(manifest.read_text())
    tables = [(section, section, data.get(section, {})) for section in SECTIONS]
    for target, groups in data.get("target", {}).items():
        for section in SECTIONS:
            label = f"target.{target}.{section}"
            tables.append((label, section, groups.get(section, {})))
    return tables


def check_crate(crate_dir, problems, used_feature_only, declared_names):
    crate = crate_dir.name
    manifest = crate_dir / "Cargo.toml"
    for label, section, table in declarations(manifest):
        for dependency in table:
            declared_names.add((crate, dependency))
            scope = SCOPES[section]
            key = (crate, dependency)
            if key in FEATURE_ONLY:
                used_feature_only.add(key)
                if is_referenced(crate_dir, scope, dependency):
                    problems.append(
                        f"{manifest}: {label}.{dependency} is referenced now; remove its FEATURE_ONLY entry"
                    )
                continue
            if is_referenced(crate_dir, scope, dependency):
                continue
            if section == "dependencies" and is_referenced(crate_dir, TEST_SCOPE, dependency):
                problems.append(
                    f"{manifest}: {label}.{dependency} is used only by test targets; "
                    "move it to [dev-dependencies]"
                )
            else:
                problems.append(f"{manifest}: {label}.{dependency} is unused; remove it")


def check_workspace(root):
    problems = []
    used_feature_only = set()
    declared_names = set()
    crate_dirs = sorted(path for path in (root / "crates").glob("*") if (path / "Cargo.toml").is_file())
    for crate_dir in crate_dirs:
        check_crate(crate_dir, problems, used_feature_only, declared_names)

    for crate, dependency in sorted(set(FEATURE_ONLY) - used_feature_only):
        problems.append(f"FEATURE_ONLY entry {crate}/{dependency} matches no declaration; remove it")

    workspace_manifest = root / "Cargo.toml"
    if workspace_manifest.is_file():
        workspace = tomllib.loads(workspace_manifest.read_text()).get("workspace", {})
        member_names = {dependency for _, dependency in declared_names}
        for dependency in workspace.get("dependencies", {}):
            if dependency not in member_names:
                problems.append(
                    f"{workspace_manifest}: [workspace.dependencies] {dependency} is declared by no member; remove it"
                )
    return problems


def main(argv):
    root = pathlib.Path(argv[1]) if len(argv) > 1 else pathlib.Path(__file__).resolve().parent.parent
    problems = check_workspace(root)
    for problem in problems:
        print(problem)
    if problems:
        return 1
    print("unused dependency guard passed")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
