#!/usr/bin/env python3
"""Survey a release range for breaking changes and decide the version bump.

Read-only and offline except for `orbit task show`, which supplies each landed
task's tags and execution summary. Without Orbit (`--offline`, or the CLI is
missing or fails) tasks come from the `[ORB-NNNNN]` in commit subjects alone and
the decision rests on the detector.

Mechanical signals, each compared against the commit's first parent:

- schema-version   SUPPORTED_SCHEMA_VERSION increased
- layout-version   SUPPORTED_LAYOUT_VERSION increased
- cli-help         a command, flag or possible value gone from the help goldens
- mcp-schema       a non-additive change in the MCP tools/list snapshot
- persisted-field  a field added to a `#[serde(deny_unknown_fields)]` type in a
                   persisted-type crate (older binaries reject the new record)
- deprecated-key   an entry added to DEPRECATED_CONFIG_KEYS
- seeded-asset     a seeded routine, activity or job asset removed

The bump is minor when any landed task carries the `breaking-change` tag or the
detector found a signal, otherwise patch. Breaking Changes bullets come from
each breaking task's `Breaking:` execution-summary line.

Usage:
  scripts/release-breaking-changes.py [--range A..B] [--json] [--offline]
"""

import argparse
import json
import re
import subprocess
import sys
from decimal import Decimal
from pathlib import PurePosixPath

BREAKING_TAG = "breaking-change"

SCHEMA_REGISTRY = "crates/orbit-store/src/driver/sqlite/migration/ledger/registry.rs"
LAYOUT_REGISTRY = "crates/orbit-store/src/workflow/layout/registry.rs"
HELP_GOLDENS = "crates/orbit-cli/tests/help_goldens"
MCP_SNAPSHOT = "crates/orbit-cli/tests/snapshots/mcp_tools_list.json"
CONFIG_KEYS = "crates/orbit-config/src/registry/keys.rs"
SEEDED_ASSET_DIRS = tuple(f"crates/orbit-core/assets/{kind}/"
                          for kind in ("routines", "activities", "jobs"))
# Crates whose deny_unknown_fields types are records Orbit stores or exchanges
# between hosts (task envelope, relations, regression data, run state). Config,
# CLI and tool-input types elsewhere are read only from input the user writes.
PERSISTED_TYPE_ROOTS = ("crates/orbit-types/src/", "crates/orbit-store/src/")

TASK_ID = re.compile(r"\[([A-Z][A-Z0-9]*-\d+)\]")
VERSION_TAG = re.compile(r"^v(\d+)\.(\d+)\.(\d+)$")
BREAKING_LINE = re.compile(r"^\s*(?:[-*]\s+)?(?:\*\*)?Breaking:(?:\*\*)?\s*(.+?)\s*$")


class Git:
    """Reads commits and blobs from one repository."""

    def __init__(self, repo):
        self.repo = repo
        self._blobs = {}
        self._trees = {}

    def run(self, *args, check=True):
        result = subprocess.run(["git", "-C", self.repo, *args],
                                capture_output=True, text=True)
        if check and result.returncode != 0:
            raise SystemExit(f"release-breaking-changes: git {' '.join(args)} failed: "
                             f"{result.stderr.strip()}")
        return result

    def show(self, rev, path):
        """The text of `path` at `rev`, or None when it does not exist there."""
        key = (rev, path)
        if key not in self._blobs:
            result = self.run("show", f"{rev}:{path}", check=False)
            self._blobs[key] = result.stdout if result.returncode == 0 else None
        return self._blobs[key]

    def tree_files(self, rev, directory):
        """{path: text} for every file under `directory` at `rev`, cached by tree id."""
        tree = self.run("rev-parse", "--verify", "--quiet", f"{rev}:{directory}", check=False)
        tree_id = tree.stdout.strip()
        if tree.returncode != 0 or not tree_id:
            return {}
        if tree_id not in self._trees:
            listing = self.run("ls-tree", "-r", "--name-only", rev, "--", directory).stdout
            self._trees[tree_id] = {path: self.show(rev, path)
                                    for path in listing.splitlines() if path}
        return self._trees[tree_id]


# ---------------------------------------------------------------- detectors


def signal(kind, path, detail):
    return {"signal": kind, "file": path, "detail": detail}


def const_value(text, name):
    if text is None:
        return None
    match = re.search(rf"\b{name}\s*:\s*\w+\s*=\s*(\d+)\s*;", text)
    return int(match.group(1)) if match else None


def detect_version_bumps(git, parent, commit, changed):
    found = []
    for path, name, kind in ((SCHEMA_REGISTRY, "SUPPORTED_SCHEMA_VERSION", "schema-version"),
                             (LAYOUT_REGISTRY, "SUPPORTED_LAYOUT_VERSION", "layout-version")):
        if path not in changed:
            continue
        old = const_value(git.show(parent, path), name)
        new = const_value(git.show(commit, path), name)
        if old is not None and new is not None and new > old:
            found.append(signal(kind, path, f"{name} {old} → {new}"))
    return found


HELP_SECTION = re.compile(r"^([A-Z][^\n]*):\s*$")
HELP_COMMAND_ENTRY = re.compile(r"^  ([a-z][a-z0-9-]*)(?:\s{2,}\S.*)?$")
HELP_FLAG = re.compile(r"^\s+(?:(-[A-Za-z0-9]),\s+)?(--[a-z0-9][a-z0-9-]*)|^\s+(-[A-Za-z0-9])(?=[\s<]|$)")
HELP_INLINE_VALUES = re.compile(r"\[possible values: ([^\]]*)\]")
HELP_LISTED_VALUE = re.compile(r"^\s+- ([^\s:]+)(?::|\s*$)")
HELP_ARGUMENT = re.compile(r"^\s+[<\[]([A-Za-z0-9_.-]+)[>\]]")


def help_surface(text):
    """(command path, {commands}, {flags}, {(option, value)}) for one help golden."""
    commands, flags, values = set(), set(), set()
    usage = None
    section = None
    option = None
    for line in text.splitlines():
        if usage is None:
            if line.startswith("Usage: "):
                words = []
                for word in line[len("Usage: "):].split():
                    if word[0] in "[<":
                        break
                    words.append(word)
                usage = " ".join(words)
            continue
        header = HELP_SECTION.match(line)
        if header:
            section = header.group(1).lower()
            option = None
            continue
        if section is None:
            continue
        if "option" in section or section == "arguments":
            flag = HELP_FLAG.match(line)
            argument = HELP_ARGUMENT.match(line) if section == "arguments" else None
            if flag:
                names = [name for name in flag.groups() if name]
                flags.update(names)
                option = names[-1]
            elif argument:
                option = f"<{argument.group(1)}>"
            if option:
                for listed in HELP_INLINE_VALUES.findall(line):
                    values.update((option, value.strip()) for value in listed.split(",")
                                  if value.strip())
                listed = HELP_LISTED_VALUE.match(line)
                if listed and not flag:
                    values.add((option, listed.group(1)))
        elif section not in ("examples", "usage"):
            entry = HELP_COMMAND_ENTRY.match(line)
            if entry:
                commands.add(entry.group(1))
    return usage, commands, flags, values


def help_inventory(files):
    """Every command, flag and possible value the help goldens show."""
    commands, flags, values = set(), set(), set()
    for path, text in files.items():
        if not path.endswith(".txt") or text is None:
            continue
        usage, subcommands, own_flags, own_values = help_surface(text)
        if not usage:
            continue
        commands.add(usage)
        commands.update(f"{usage} {name}" for name in subcommands)
        flags.update((usage, flag) for flag in own_flags)
        values.update((usage, option, value) for option, value in own_values)
    return commands, flags, values


def detect_help_removals(git, parent, commit, changed):
    if not any(path.startswith(HELP_GOLDENS + "/") for path in changed):
        return []
    old = help_inventory(git.tree_files(parent, HELP_GOLDENS))
    new = help_inventory(git.tree_files(commit, HELP_GOLDENS))
    removed_commands = old[0] - new[0]
    found = []
    for command in sorted(removed_commands):
        parent_command = command.rsplit(" ", 1)[0]
        if " " not in command or parent_command not in removed_commands:
            found.append(signal("cli-help", HELP_GOLDENS, f"command `{command}` removed"))
    # A subcommand golden repeats the program's global options; one that still
    # exists at the program root was moved, not removed.
    root_flags = {flag for usage, flag in new[1] if " " not in usage}
    root_values = {(option, value) for usage, option, value in new[2] if " " not in usage}
    removed_flags = {(usage, flag) for usage, flag in old[1] - new[1]
                     if usage not in removed_commands and flag not in root_flags}
    for usage, flag in sorted(removed_flags):
        found.append(signal("cli-help", HELP_GOLDENS, f"flag `{usage} {flag}` removed"))
    for usage, option, value in sorted(old[2] - new[2]):
        if (usage in removed_commands or (usage, option) in removed_flags
                or (option, value) in root_values):
            continue
        found.append(signal("cli-help", HELP_GOLDENS,
                            f"possible value `{value}` of `{usage} {option}` removed"))
    return found


def schema_facts(schema, path, facts):
    """Flatten the structural facts of a JSON schema; descriptions are skipped."""
    if isinstance(schema, bool):
        # Only `true` is an existing location: it accepts everything, so
        # requiring a child there rejects previously valid input. `false`
        # accepts nothing, so relaxing it to an object only admits input.
        if schema:
            facts.setdefault("schema_nodes", set()).add(path)
        return
    if not isinstance(schema, dict):
        return
    facts.setdefault("schema_nodes", set()).add(path)
    if "type" in schema:
        facts.setdefault("type", {})[path] = json.dumps(schema["type"], sort_keys=True)
    if "enum" in schema:
        facts.setdefault("enum", {})[path] = {json.dumps(v, sort_keys=True) for v in schema["enum"]}
    if "required" in schema and isinstance(schema["required"], list):
        facts.setdefault("required", {})[path] = set(schema["required"])
    properties = schema.get("properties")
    if isinstance(properties, dict):
        facts.setdefault("properties", {})[path] = set(properties)
        for name, child in properties.items():
            schema_facts(child, f"{path}.{name}", facts)
    for key in ("items", "additionalProperties"):
        if isinstance(schema.get(key), (dict, bool)):
            schema_facts(schema[key], f"{path}[{key}]", facts)
    for key in ("anyOf", "oneOf", "allOf"):
        branches = schema.get(key) or []
        if key in schema and key in ("anyOf", "oneOf") and isinstance(branches, list):
            facts.setdefault("alternatives", {})[(path, key)] = branches
        for index, child in enumerate(branches):
            schema_facts(child, f"{path}{{{key}{index}}}", facts)


def mcp_tools(text):
    if text is None:
        return {}
    data = json.loads(text)
    tools = data.get("tools", []) if isinstance(data, dict) else data
    result = {}
    for tool in tools:
        facts = {}
        for key in ("inputSchema", "outputSchema"):
            if key in tool:
                schema_facts(tool[key], key, facts)
        result[tool["name"]] = facts
    return result


def schema_without_required(schema):
    """Return a branch's structure without its required-property constraints."""
    if isinstance(schema, list):
        return [schema_without_required(value) for value in schema]
    if isinstance(schema, dict):
        return {key: schema_without_required(value) for key, value in schema.items()
                if key != "required"}
    return schema


def schema_required_names(schema):
    """Collect required property names anywhere in an alternative branch."""
    if isinstance(schema, list):
        return set().union(*(schema_required_names(value) for value in schema))
    if not isinstance(schema, dict):
        return set()
    required = schema.get("required", [])
    names = set(required) if isinstance(required, list) else set()
    for value in schema.values():
        names.update(schema_required_names(value))
    return names


def schema_property_names(schema):
    """Top-level property names one alternative branch declares."""
    properties = schema.get("properties") if isinstance(schema, dict) else None
    return set(properties) if isinstance(properties, dict) else set()


def match_schema_alternatives(before, after):
    """Map unchanged, tightened or edited old alternatives to their new positions."""
    matches, old_left, new_left = {}, set(range(len(before))), set(range(len(after)))
    before_json = [json.dumps(branch, sort_keys=True) for branch in before]
    after_json = [json.dumps(branch, sort_keys=True) for branch in after]

    # Exact branch content is the strongest identity and handles insertions or
    # reorderings without relying on array positions.
    for old_index, encoded in enumerate(before_json):
        candidates = [index for index in new_left if after_json[index] == encoded]
        if candidates:
            new_index = min(candidates, key=lambda index: (abs(index - old_index), index))
            matches[new_index] = old_index
            old_left.remove(old_index)
            new_left.remove(new_index)

    # A tightening changes `required` but keeps at least one required property
    # from the original branch. Pair those remaining branches by shared
    # constraints, preferring the same non-required schema shape.
    before_required = [schema_required_names(branch) for branch in before]
    after_required = [schema_required_names(branch) for branch in after]
    before_shapes = [json.dumps(schema_without_required(branch), sort_keys=True)
                     for branch in before]
    after_shapes = [json.dumps(schema_without_required(branch), sort_keys=True)
                    for branch in after]
    candidates = []
    for old_index in old_left:
        for new_index in new_left:
            overlap = len(before_required[old_index] & after_required[new_index])
            same_shape = before_shapes[old_index] == after_shapes[new_index]
            if overlap or (same_shape and
                           (not before_required[old_index] or not after_required[new_index])):
                candidates.append((same_shape, overlap,
                                   -abs(old_index - new_index), old_index, new_index))
    for _, _, _, old_index, new_index in sorted(candidates, reverse=True):
        if old_index in old_left and new_index in new_left:
            matches[new_index] = old_index
            old_left.remove(old_index)
            new_left.remove(new_index)

    # An edit can change both the required names and the shape of a branch.
    # Pair the leftovers that still share property names, most shared first;
    # otherwise the edited branch reads as inserted and its removed or newly
    # required properties go unreported.
    before_properties = [schema_property_names(branch) for branch in before]
    after_properties = [schema_property_names(branch) for branch in after]
    candidates = []
    for old_index in old_left:
        for new_index in new_left:
            shared = len(before_properties[old_index] & after_properties[new_index])
            if shared:
                candidates.append((shared, -abs(old_index - new_index), old_index, new_index))
    for _, _, old_index, new_index in sorted(candidates, reverse=True):
        if old_index in old_left and new_index in new_left:
            matches[new_index] = old_index
            old_left.remove(old_index)
            new_left.remove(new_index)

    # With just one branch left on each side, an edit can replace every
    # property name. Pair that remainder after the stronger identity matches;
    # extra inserted branches stay unmatched, and multiple leftovers remain
    # ambiguous rather than being paired by position.
    if len(old_left) == len(new_left) == 1:
        matches[next(iter(new_left))] = next(iter(old_left))
    return matches


def remap_alternative_path(path, path_map):
    """Translate current branch prefixes to their matched baseline prefixes."""
    if not path_map:
        return path
    # One substitution pass: replacing each prefix in turn would chain a swapped
    # pair (0 to 1, then 1 to 0) back onto itself. Longer prefixes match first.
    pattern = "|".join(re.escape(current) for current in sorted(path_map, key=len, reverse=True))
    return re.sub(pattern, lambda match: path_map[match.group(0)], path)


def under_unmatched_alternative(path, unmatched):
    return any(path.startswith(prefix) for prefix in unmatched)


def alternative_path_map(before, after):
    """Pair anyOf/oneOf branches by content; track inserted and removed branches.

    Returns (path_map, unmatched, removed): `path_map` translates a surviving
    current branch prefix to its baseline prefix, `unmatched` holds inserted
    current branch prefixes, and `removed` holds (baseline prefix, keyword)
    pairs with no surviving counterpart (outermost only).
    """
    path_map, unmatched = {}, set()
    matched_baseline = set()
    alternatives = after.get("alternatives", {})
    for (parent, keyword), new_branches in sorted(
            alternatives.items(), key=lambda item: (item[0][0].count("{"), item[0][0])):
        if under_unmatched_alternative(parent, unmatched):
            old_branches = []
            baseline_parent = remap_alternative_path(parent, path_map)
        else:
            baseline_parent = remap_alternative_path(parent, path_map)
            old_branches = before.get("alternatives", {}).get((baseline_parent, keyword), [])
        matches = match_schema_alternatives(old_branches, new_branches)
        for new_index in range(len(new_branches)):
            current = f"{parent}{{{keyword}{new_index}}}"
            if new_index in matches:
                baseline = f"{baseline_parent}{{{keyword}{matches[new_index]}}}"
                path_map[current] = baseline
                matched_baseline.add(baseline)
            else:
                unmatched.add(current)
    branch_content = {}
    for (parent, keyword), branches in before.get("alternatives", {}).items():
        for old_index, branch in enumerate(branches):
            branch_content[f"{parent}{{{keyword}{old_index}}}"] = (parent, keyword, branch)
    surviving_content = set()
    for baseline in matched_baseline:
        if baseline in branch_content:
            parent, keyword, branch = branch_content[baseline]
            surviving_content.add((parent, keyword, json.dumps(branch, sort_keys=True)))
    surviving_applicator_locations = {
        remap_alternative_path(parent, path_map)
        for parent, keyword in alternatives
        if keyword in ("anyOf", "oneOf")
        and not under_unmatched_alternative(parent, unmatched)
    }
    removed = []
    for baseline in sorted(branch_content,
                           key=lambda path: (path.count("{"), path)):
        parent, keyword, branch = branch_content[baseline]
        if baseline in matched_baseline:
            continue
        if under_unmatched_alternative(baseline, {path for path, _ in removed}):
            continue
        # Removing the whole anyOf/oneOf keyword relaxes the schema. It is not
        # equivalent to removing a branch while an applicator still constrains
        # the same location. Count either keyword here: replacing anyOf with
        # oneOf (or vice versa) can still tighten the accepted schema.
        if parent not in surviving_applicator_locations:
            continue
        # Deduping identical branches changes nothing for anyOf and only
        # admits input for oneOf, so an identical survivor keeps it additive.
        if (parent, keyword, json.dumps(branch, sort_keys=True)) in surviving_content:
            continue
        removed.append((baseline, keyword))
    return path_map, unmatched, removed


def _schema_type_names(schema):
    if isinstance(schema, dict) and "type" in schema:
        names = schema["type"]
        return set(names) if isinstance(names, list) else {names}
    return None


def _schema_enum_values(schema):
    if isinstance(schema, dict) and isinstance(schema.get("enum"), list):
        return schema["enum"]
    return None


def _json_value_key(value):
    """Compare JSON values using JSON Schema's numeric equality rules."""
    if isinstance(value, bool):
        return ("boolean", value)
    if value is None:
        return ("null",)
    if isinstance(value, (int, float)):
        return ("number", Decimal(str(value)))
    if isinstance(value, str):
        return ("string", value)
    if isinstance(value, list):
        return ("array", tuple(_json_value_key(item) for item in value))
    if isinstance(value, dict):
        return ("object", tuple(sorted((key, _json_value_key(item))
                                        for key, item in value.items())))
    return (type(value).__name__, value)


def _json_type_name(value):
    if value is None:
        return "null"
    if isinstance(value, bool):
        return "boolean"
    if isinstance(value, int):
        return "integer"
    if isinstance(value, float):
        return "integer" if value.is_integer() else "number"
    if isinstance(value, str):
        return "string"
    if isinstance(value, list):
        return "array"
    return "object"


def _json_value_matches_types(value, names):
    name = _json_type_name(value)
    return name in names or (name == "integer" and "number" in names)


def _schema_const_value(schema):
    if isinstance(schema, dict) and "const" in schema:
        return _json_value_key(schema["const"])
    return None


def schemas_disjoint(first, second):
    """Whether two schemas provably accept no common instance.

    Conservative: returns False (possibly overlapping) unless a direct
    contradiction is found — disjoint declared types, enum or const values
    that cannot match the other branch's enum, const or declared type,
    incompatible constraints on a required property of an object, or a
    required property a closed object forbids.
    """
    if isinstance(first, bool) or isinstance(second, bool):
        # `false` accepts nothing; `true` overlaps everything.
        return first is False or second is False
    if not isinstance(first, dict) or not isinstance(second, dict):
        return False
    first_types, second_types = _schema_type_names(first), _schema_type_names(second)
    if (first_types is not None and second_types is not None
            and not (first_types & second_types
                     or ("integer" in first_types and "number" in second_types)
                     or ("number" in first_types and "integer" in second_types))):
        return True
    first_enum, second_enum = _schema_enum_values(first), _schema_enum_values(second)
    if (first_enum is not None and second_enum is not None
            and not any(_json_value_key(left) == _json_value_key(right)
                        for left in first_enum for right in second_enum)):
        return True
    for enum_values, other_types in ((first_enum, second_types), (second_enum, first_types)):
        if enum_values is not None and other_types is not None:
            if not any(_json_value_matches_types(value, other_types)
                       for value in enum_values):
                return True
    for schema, other_types in ((first, second_types), (second, first_types)):
        if "const" in schema and other_types is not None:
            if not _json_value_matches_types(schema["const"], other_types):
                return True
    first_const, second_const = _schema_const_value(first), _schema_const_value(second)
    if first_const is not None and second_const is not None and first_const != second_const:
        return True
    for const, enum_values in ((first_const, second_enum), (second_const, first_enum)):
        if (const is not None and enum_values is not None
                and not any(const == _json_value_key(value) for value in enum_values)):
            return True
    first_props = first.get("properties") if isinstance(first.get("properties"), dict) else {}
    second_props = second.get("properties") if isinstance(second.get("properties"), dict) else {}
    if first_types == {"object"} or second_types == {"object"}:
        first_required = set(first.get("required", [])) if isinstance(
            first.get("required"), list) else set()
        second_required = set(second.get("required", [])) if isinstance(
            second.get("required"), list) else set()
        for name in first_props.keys() & second_props.keys():
            if (name in first_required or name in second_required
                    ) and schemas_disjoint(first_props[name], second_props[name]):
                return True
    for owner, other in ((first, second), (second, first)):
        owner_patterns = owner.get("patternProperties")
        if (_schema_type_names(owner) == {"object"}
                and owner.get("additionalProperties") is False
                and isinstance(owner.get("properties"), dict)):
            if isinstance(owner_patterns, dict) and owner_patterns:
                continue
            required = other.get("required")
            required = set(required) if isinstance(required, list) else set()
            if not required <= set(owner["properties"]):
                return True
    return False


def translated_facts(values, path_map, unmatched):
    result = {}
    for path, value in values.items():
        if not under_unmatched_alternative(path, unmatched):
            result[remap_alternative_path(path, path_map)] = value
    return result


def detect_mcp_changes(git, parent, commit, changed):
    if MCP_SNAPSHOT not in changed:
        return []
    old, new = mcp_tools(git.show(parent, MCP_SNAPSHOT)), mcp_tools(git.show(commit, MCP_SNAPSHOT))
    found = []

    def report(detail):
        found.append(signal("mcp-schema", MCP_SNAPSHOT, detail))

    for name in sorted(old):
        if name not in new:
            report(f"tool `{name}` removed")
            continue
        before, after = old[name], new[name]
        path_map, unmatched, removed = alternative_path_map(before, after)
        for baseline, keyword in removed:
            report(f"`{name}` `{baseline}` {keyword} alternative removed")
        for current in sorted(unmatched, key=lambda path: (path.count("{"), path)):
            if under_unmatched_alternative(
                    current, {path for path in unmatched if path != current}):
                continue
            for (parent, keyword), new_branches in after.get("alternatives", {}).items():
                prefix = f"{parent}{{{keyword}"
                if not current.startswith(prefix) or keyword != "oneOf":
                    continue
                try:
                    new_index = int(current[len(prefix):-1])
                    branch = new_branches[new_index]
                except (ValueError, IndexError):
                    continue
                baseline_parent = remap_alternative_path(parent, path_map)
                old_branches = before.get("alternatives", {}).get(
                    (baseline_parent, keyword), [])
                if not old_branches:
                    continue
                if any(not schemas_disjoint(branch, old) for old in old_branches):
                    report(f"`{name}` `{current}` oneOf alternative "
                           f"overlaps an existing alternative")
                break
        after_properties = translated_facts(after.get("properties", {}), path_map, unmatched)
        after_types = translated_facts(after.get("type", {}), path_map, unmatched)
        after_enums = translated_facts(after.get("enum", {}), path_map, unmatched)
        after_nodes = translated_facts(
            {path: True for path in after.get("schema_nodes", set())}, path_map, unmatched)
        for path, names in sorted(before.get("properties", {}).items()):
            if path not in after_properties:
                continue
            for prop in sorted(names - after_properties[path]):
                report(f"`{name}` property `{path}.{prop}` removed or renamed")
        for path, kind in sorted(before.get("type", {}).items()):
            if path in after_types and after_types[path] != kind:
                report(f"`{name}` `{path}` type {kind} → {after_types[path]}")
        for path, values in sorted(before.get("enum", {}).items()):
            if path in after_enums:
                for value in sorted(values - after_enums[path]):
                    report(f"`{name}` `{path}` enum value {value} removed")
        # allOf branches all apply, so their required sets union at their
        # instance location (branch renumbering is not a new constraint).
        # anyOf/oneOf alternatives retain their positional branch paths:
        # tightening an existing alternative can reject previously valid
        # input, while requirements at a new branch path stay additive.
        # A removed alternative rejects the inputs only it accepted.
        # Inserted anyOf branches only admit input, but oneOf demands
        # exactly one match, so an inserted oneOf alternative overlapping
        # an existing one steals the single match a formerly valid input
        # had; provably disjoint insertions stay additive.
        def location(raw):
            return re.sub(r"\{allOf\d+\}", "", raw)

        def required_by_location(facts):
            grouped = {}
            for raw, req in facts.get("required", {}).items():
                grouped.setdefault(location(raw), set()).update(req)
            return grouped

        before_locations = {location(p) for p in before.get("schema_nodes", set())}
        before_required = required_by_location(before)
        translated_after = dict(after)
        translated_after["required"] = translated_facts(after.get("required", {}), path_map, unmatched)
        after_locations = {location(p) for p in after_nodes}
        for loc, required in sorted(required_by_location(translated_after).items()):
            if loc not in before_locations or loc not in after_locations:
                continue
            for prop in sorted(required - before_required.get(loc, set())):
                report(f"`{name}` property `{loc}.{prop}` newly required")
    return found


RUST_ITEM = re.compile(
    r"^[ \t]*(?:pub(?:\([^)\n]*\))?[ \t]+)?(?P<kind>struct|enum)[ \t]+"
    r"(?P<name>[A-Za-z_][A-Za-z0-9_]*)[^{;]*\{", re.M)
RUST_FIELD = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:r#)?([a-z_][a-z0-9_]*)\s*:(?!:)")
RUST_VARIANT = re.compile(r"^\s*([A-Z][A-Za-z0-9_]*)\s*\{")


def strip_rust_comments(text):
    text = re.sub(r"/\*.*?\*/", "", text, flags=re.S)
    return "\n".join(re.sub(r"//.*$", "", line) for line in text.splitlines())


def item_attributes(text, start):
    """The attribute and comment lines directly above the item starting at `start`."""
    lines = text[:start].splitlines()
    attrs = []
    for line in reversed(lines):
        code = re.sub(r"//.*$", "", line).strip()
        if code.endswith((";", "{", "}")):
            break
        attrs.append(code)
    return " ".join(reversed(attrs))


def deny_unknown_types(text):
    """{type name: {field}} for each deny_unknown_fields struct or enum in a Rust file."""
    if text is None:
        return {}

    def field_is_skipped(attributes):
        # Mask Rust string literals before looking for serde attributes so a
        # doc or rename value containing an attribute example stays text.
        outside_strings = list(attributes)
        raw_string_prefix = re.compile(r'(?:br|r)(?P<hashes>#*)"')
        index = 0
        while index < len(attributes):
            raw_string = raw_string_prefix.match(attributes, index)
            if raw_string:
                closing = '"' + raw_string.group("hashes")
                end = attributes.find(closing, raw_string.end())
                end = len(attributes) if end < 0 else end + len(closing)
                for position in range(index, end):
                    if attributes[position] != "\n":
                        outside_strings[position] = " "
                index = end
                continue
            if attributes[index] == '"':
                end = index + 1
                escaped = False
                while end < len(attributes):
                    char = attributes[end]
                    end += 1
                    if escaped:
                        escaped = False
                    elif char == "\\":
                        escaped = True
                    elif char == '"':
                        break
                for position in range(index, end):
                    if attributes[position] != "\n":
                        outside_strings[position] = " "
                index = end
                continue
            index += 1

        attribute_text = "".join(outside_strings)
        # Match a serde attribute anywhere outside string literals: masking
        # above already hides quoted examples, so no line-start anchor is
        # needed, and anchoring would miss `#[serde(...)]` that shares its
        # line with another attribute.
        serde_attribute = re.compile(r"#\s*\[\s*serde\s*\(")
        for match in serde_attribute.finditer(attribute_text):
            arguments, argument, depth = [], [], 1
            index = match.end()
            in_string = escaped = False
            while index < len(attributes) and depth:
                char = attributes[index]
                if in_string:
                    if escaped:
                        escaped = False
                    elif char == "\\":
                        escaped = True
                    elif char == '"':
                        in_string = False
                elif char == '"':
                    in_string = True
                elif char == "(":
                    depth += 1
                elif char == ")":
                    depth -= 1
                    if depth == 0:
                        arguments.append("".join(argument))
                        break
                    argument.append(char)
                elif char == "," and depth == 1:
                    arguments.append("".join(argument))
                    argument = []
                else:
                    argument.append(char)
                index += 1
            if any(argument.split("=", 1)[0].strip() in {"skip", "skip_serializing"}
                   for argument in arguments):
                return True
        return False

    types = {}
    for match in RUST_ITEM.finditer(text):
        attrs = item_attributes(text, match.start())
        if not re.search(r"\bdeny_unknown_fields\b", attrs):
            continue
        depth, start, index = 1, match.end(), match.end()
        while index < len(text) and depth:
            depth += {"{": 1, "}": -1}.get(text[index], 0)
            index += 1
        body = strip_rust_comments(text[start:index - 1])
        fields, depth, variant = set(), 1, None
        pending_attributes, attribute_buffer = [], None
        for line in body.splitlines():
            stripped = line.strip()
            if attribute_buffer is not None:
                attribute_buffer += "\n" + line
                if "]" in line:
                    pending_attributes.append(attribute_buffer)
                    attribute_buffer = None
                continue
            if stripped.startswith("#"):
                if "]" in stripped:
                    pending_attributes.append(line)
                else:
                    attribute_buffer = line
                continue
            if match.group("kind") == "struct" and depth == 1:
                field = RUST_FIELD.match(line)
                if field:
                    if not field_is_skipped("\n".join(pending_attributes)):
                        fields.add(field.group(1))
                    pending_attributes = []
                elif stripped:
                    pending_attributes = []
            elif match.group("kind") == "enum":
                if depth == 1:
                    opened = RUST_VARIANT.match(line)
                    variant_skipped = field_is_skipped("\n".join(pending_attributes))
                    variant = opened.group(1) if opened and not variant_skipped else None
                    if opened and not variant_skipped and "}" in line:  # `Variant { a: A, b: B },`
                        inline = line[opened.end():line.rindex("}")]
                        fields.update(f"{variant}.{field.group(1)}"
                                      for part in inline.split(",")
                                      if (field := RUST_FIELD.match(part)))
                    pending_attributes = []
                elif depth == 2 and variant:
                    field = RUST_FIELD.match(line)
                    if field:
                        if not field_is_skipped("\n".join(pending_attributes)):
                            fields.add(f"{variant}.{field.group(1)}")
                        pending_attributes = []
                    elif stripped:
                        pending_attributes = []
            depth += line.count("{") - line.count("}")
        types[match.group("name")] = fields
    return types


def persisted_type_file(path):
    parts = PurePosixPath(path).parts
    return (path.endswith(".rs") and path.startswith(PERSISTED_TYPE_ROOTS)
            and "tests" not in parts and not path.endswith(("/tests.rs", "_tests.rs")))


def detect_persisted_fields(git, parent, commit, changed, renamed_from):
    paths = [path for path in changed if persisted_type_file(path)]
    if not paths:
        return []
    old, locations = {}, {}
    for path in paths:
        for name, fields in deny_unknown_types(git.show(parent, renamed_from.get(path, path))).items():
            old.setdefault(name, set()).update(fields)
    new = {}
    for path in paths:
        for name, fields in deny_unknown_types(git.show(commit, path)).items():
            new.setdefault(name, set()).update(fields)
            locations.setdefault(name, path)
    found = []
    for name in sorted(new):
        if name in old:
            for field in sorted(new[name] - old[name]):
                found.append(signal("persisted-field", locations[name],
                                    f"field `{field}` added to deny_unknown_fields type `{name}`"))
    return found


def deprecated_keys(text):
    if text is None:
        return set()
    match = re.search(r"DEPRECATED_CONFIG_KEYS\s*:[^=]*=\s*&\[(.*?)\n\];", text, re.S)
    if not match:
        return set()
    return set(re.findall(r"\(\s*\"([^\"]+)\"", strip_rust_comments(match.group(1))))


def detect_deprecated_keys(git, parent, commit, changed):
    if CONFIG_KEYS not in changed:
        return []
    added = deprecated_keys(git.show(commit, CONFIG_KEYS)) - deprecated_keys(git.show(parent, CONFIG_KEYS))
    return [signal("deprecated-key", CONFIG_KEYS, f"`{key}` added to DEPRECATED_CONFIG_KEYS")
            for key in sorted(added)]


def detect_seeded_removals(deleted):
    return [signal("seeded-asset", path, "seeded asset removed")
            for path in sorted(deleted) if path.startswith(SEEDED_ASSET_DIRS)]


def detect_commit(git, commit):
    """Every signal one commit carries, against its first parent."""
    parent = f"{commit}^"
    if git.run("rev-parse", "--verify", "--quiet", parent, check=False).returncode != 0:
        return []
    status = git.run("diff", "--name-status", "-M", parent, commit).stdout
    changed, deleted, renamed_from = set(), set(), {}
    for line in status.splitlines():
        fields = line.split("\t")
        code = fields[0][:1]
        if code == "R":
            renamed_from[fields[2]] = fields[1]
            changed.update(fields[1:3])
            deleted.add(fields[1])
        elif code == "D":
            deleted.add(fields[1])
            changed.add(fields[1])
        else:
            changed.add(fields[-1])
    return (detect_version_bumps(git, parent, commit, changed)
            + detect_help_removals(git, parent, commit, changed)
            + detect_mcp_changes(git, parent, commit, changed)
            + detect_persisted_fields(git, parent, commit, changed, renamed_from)
            + detect_deprecated_keys(git, parent, commit, changed)
            + detect_seeded_removals(deleted))


# ---------------------------------------------------------------- tasks


def load_task(orbit, repo, task_id):
    """The task's title, tags and execution summary from Orbit, or None."""
    try:
        result = subprocess.run(
            [orbit, "task", "show", task_id, "--json",
             "--fields", "id,title,status,tags,execution_summary"],
            cwd=repo, capture_output=True, text=True, timeout=120)
    except (OSError, subprocess.TimeoutExpired):
        return None
    if result.returncode != 0:
        return None
    try:
        record = json.loads(result.stdout)
    except json.JSONDecodeError:
        return None
    return record if isinstance(record, dict) else None


def breaking_notes(summary):
    return [match.group(1) for line in (summary or "").splitlines()
            if (match := BREAKING_LINE.match(line))]


def changelog_bullets(task):
    reference = f"([{task['id']}])" if task["id"] else f"(commit {task['commits'][0]})"
    theme = task["title"] or task["id"] or task["commits"][0]
    if task["breaking_notes"]:
        return [f"- **{theme}**: {note} {reference}" for note in task["breaking_notes"]]
    who = task["id"] or f"commit {task['commits'][0]}"
    return [f"- **{theme}**: TODO: {who} has no `Breaking:` note; say what breaks and "
            f"the migration (`x removed → use y`). {reference}"]


# ---------------------------------------------------------------- survey


def default_range(git):
    described = git.run("describe", "--tags", "--abbrev=0", "--match", "v[0-9]*.[0-9]*.[0-9]*",
                        "HEAD", check=False)
    if described.returncode != 0:
        raise SystemExit("release-breaking-changes: no vX.Y.Z tag reaches HEAD; pass --range")
    return f"{described.stdout.strip()}..HEAD"


def next_versions(base):
    match = VERSION_TAG.match(base)
    if not match:
        return None, {}
    major, minor, patch = (int(part) for part in match.groups())
    return (f"{major}.{minor}.{patch}",
            {"minor": f"{major}.{minor + 1}.0", "patch": f"{major}.{minor}.{patch + 1}"})


def survey(repo, revision_range, orbit, offline):
    git = Git(repo)
    revision_range = revision_range or default_range(git)
    if ".." not in revision_range:
        raise SystemExit("release-breaking-changes: --range must be A..B")
    base, head = revision_range.split("..", 1)
    head = head or "HEAD"
    log = git.run("log", "--no-merges", "--reverse", "--pretty=%H%x09%s",
                  f"{base}..{head}").stdout
    commits, tasks, signals = [], {}, []
    for line in log.splitlines():
        sha, _, subject = line.partition("\t")
        ids = TASK_ID.findall(subject)
        task_id = ids[-1] if ids else None
        commits.append({"commit": sha[:10], "subject": subject, "task": task_id})
        key = task_id or sha[:10]
        task = tasks.setdefault(key, {"id": task_id, "title": None, "commits": [],
                                      "tags": [], "tagged": False, "signals": [],
                                      "breaking_notes": [], "subject": subject})
        task["commits"].append(sha[:10])
        for found in detect_commit(git, sha):
            found.update(commit=sha[:10], task=task_id, subject=subject)
            signals.append(found)
            task["signals"].append(found)

    source, unread = "commit-subjects", []
    if not offline:
        loaded = 0
        for task in tasks.values():
            if not task["id"]:
                continue
            record = load_task(orbit, repo, task["id"])
            if record is None:
                unread.append(task["id"])
                continue
            loaded += 1
            task["title"] = record.get("title") or None
            task["tags"] = list(record.get("tags") or [])
            task["tagged"] = BREAKING_TAG in task["tags"]
            task["breaking_notes"] = breaking_notes(record.get("execution_summary"))
        if loaded:
            source = "orbit"
        else:
            unread = []
    for task in tasks.values():
        if not task["title"]:
            subject = re.sub(r"\s*\(#\d+\)\s*$", "", task["subject"])
            task["title"] = TASK_ID.sub("", subject).strip() or None

    breaking = [task for task in tasks.values() if task["tagged"] or task["signals"]]
    bump = "minor" if breaking else "patch"
    previous, candidates = next_versions(base)
    reasons = []
    for task in breaking:
        label = task["id"] or f"commit {task['commits'][0]}"
        if task["tagged"]:
            reasons.append(f"{label} is tagged {BREAKING_TAG}")
        if task["signals"]:
            reasons.append(f"{label}: {', '.join(sorted({s['signal'] for s in task['signals']}))}")

    def summary(task):
        return {"id": task["id"], "title": task["title"], "commits": task["commits"],
                "tagged": task["tagged"], "breaking_notes": task["breaking_notes"],
                "signals": [{key: s[key] for key in ("signal", "file", "detail", "commit")}
                            for s in task["signals"]]}

    return {
        "range": f"{base}..{head}",
        "commits": len(commits),
        "task_source": source,
        "tasks_unread": unread,
        "previous_version": previous,
        "bump": bump,
        "next_version": candidates.get(bump),
        "reasons": reasons,
        "signals": signals,
        "breaking": [summary(task) for task in breaking],
        "detected_untagged": [task["id"] or f"commit {task['commits'][0]}"
                              for task in breaking if task["signals"] and not task["tagged"]],
        "tagged_without_signal": [task["id"] for task in breaking
                                  if task["tagged"] and not task["signals"]],
        "changelog_breaking": [bullet for task in breaking for bullet in changelog_bullets(task)],
    }


def render_text(report):
    lines = [f"Range: {report['range']} ({report['commits']} commits; "
             f"task data from {report['task_source']})"]
    if report["task_source"] != "orbit":
        lines.append("  Orbit was not read: breaking-change tags and Breaking: notes are unknown.")
    elif report["tasks_unread"]:
        lines.append("  Orbit could not read " + ", ".join(report["tasks_unread"])
                     + ": their tags and Breaking: notes are unknown.")
    version = f" → {report['next_version']}" if report["next_version"] else ""
    lines.append(f"Bump: {report['bump']}{version}")
    for reason in report["reasons"]:
        lines.append(f"  - {reason}")
    lines.append("")
    lines.append("Breaking candidates:" if report["breaking"] else "Breaking candidates: none")
    for task in report["breaking"]:
        label = task["id"] or f"commit {task['commits'][0]}"
        state = "tagged" if task["tagged"] else "detected, untagged"
        lines.append(f"  {label} [{state}] {task['title'] or ''}".rstrip())
        for found in task["signals"]:
            lines.append(f"    {found['signal']}: {found['detail']} ({found['file']} @ {found['commit']})")
    if report["detected_untagged"]:
        lines.append("")
        lines.append(f"Detected, untagged (tag {BREAKING_TAG} or reject): "
                     + ", ".join(report["detected_untagged"]))
    if report["changelog_breaking"]:
        lines.append("")
        lines.append("### Breaking Changes (draft)")
        lines.append("")
        lines.extend(report["changelog_breaking"])
    return "\n".join(lines) + "\n"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--range", help="git range A..B (default: last vX.Y.Z tag..HEAD)")
    parser.add_argument("--repo", default=".", help="repository to survey (default: .)")
    parser.add_argument("--json", action="store_true", help="print the report as JSON")
    parser.add_argument("--offline", action="store_true",
                        help="don't call Orbit; take tasks from commit subjects only")
    parser.add_argument("--orbit-bin", default="orbit", help="orbit CLI to read tasks with")
    args = parser.parse_args(argv)
    report = survey(args.repo, args.range, args.orbit_bin, args.offline)
    if args.json:
        sys.stdout.write(json.dumps(report, indent=2, ensure_ascii=False) + "\n")
    else:
        sys.stdout.write(render_text(report))
    return 0


if __name__ == "__main__":
    sys.exit(main())
