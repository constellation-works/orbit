#!/usr/bin/env python3
"""Validate CodeQL data-extension row schemas used by this repository.

Conflict recovery can merge two independent barrier paths into one YAML list
row. The file still parses, but CodeQL rejects the tuple against the
extensible predicate. This check is the local schema gate: YAML parse
success is not treated as validation.
"""

from __future__ import annotations

from argparse import ArgumentParser
from pathlib import Path
import re
import sys
from typing import Any


# Pinned to the CodeQL rust-all data-extension contract:
# https://codeql.github.com/docs/codeql-language-guides/customizing-library-models-for-rust/
RUST_ALL_PACK = "codeql/rust-all"
RUST_ALL_PREDICATES = {
    "sourceModel": ("path", "output", "kind", "provenance"),
    "sinkModel": ("path", "input", "kind", "provenance"),
    "summaryModel": ("path", "input", "output", "kind", "provenance"),
    "neutralModel": ("path", "kind", "provenance"),
    "barrierModel": ("path", "output", "kind", "provenance"),
    "barrierGuardModel": ("path", "input", "acceptingValue", "kind", "provenance"),
}
SUPPORTED_PACKS = {RUST_ALL_PACK: RUST_ALL_PREDICATES}
EXTENSIONS_ROOT = Path(".github/codeql/extensions")
PACK_FILENAME = "codeql-pack.yml"
GENERATED_EXTENSION_GLOB = "extensions/**/*.yaml"
CANONICAL_CALLABLE = re.compile(r"^orbit_[a-z0-9_]+(?:::[A-Za-z_][A-Za-z0-9_]*)+$")
ACCESS_PATH = re.compile(
    r"^(?:ReturnValue|Argument\[(?:self|[0-9]+)\])"
    r"(?:\.(?:Future|Field\[[A-Za-z0-9_:()]+\]))*$"
)


class YamlError(Exception):
    """Restricted YAML subset could not be loaded."""

    def __init__(self, message: str, line: int | None = None) -> None:
        self.line = line
        if line is None:
            super().__init__(message)
        else:
            super().__init__(f"line {line}: {message}")


class LogicalLine:
    __slots__ = ("indent", "text", "line")

    def __init__(self, indent: int, text: str, line: int) -> None:
        self.indent = indent
        self.text = text
        self.line = line


def _strip_comment(line: str) -> str:
    in_single = False
    in_double = False
    index = 0
    while index < len(line):
        char = line[index]
        if char == "\\" and in_double:
            index += 2
            continue
        if char == "'" and not in_double:
            in_single = not in_single
        elif char == '"' and not in_single:
            in_double = not in_double
        elif char == "#" and not in_single and not in_double:
            return line[:index].rstrip()
        index += 1
    return line.rstrip()


def _flow_depth(text: str) -> int:
    depth = 0
    in_single = False
    in_double = False
    index = 0
    while index < len(text):
        char = text[index]
        if char == "\\" and in_double:
            index += 2
            continue
        if char == "'" and not in_double:
            in_single = not in_single
        elif char == '"' and not in_single:
            in_double = not in_double
        elif not in_single and not in_double:
            if char in "[{":
                depth += 1
            elif char in "]}":
                depth -= 1
        index += 1
    return depth


def _logical_lines(text: str) -> list[LogicalLine]:
    physical = text.splitlines()
    lines: list[LogicalLine] = []
    index = 0
    while index < len(physical):
        raw = physical[index]
        line_no = index + 1
        if not raw.strip() or raw.lstrip().startswith("#"):
            index += 1
            continue
        leading = raw[: len(raw) - len(raw.lstrip(" \t"))]
        if "\t" in leading:
            raise YamlError("tab indentation is not allowed", line_no)
        indent = len(leading)
        content = _strip_comment(raw)[indent:].rstrip()
        if not content:
            index += 1
            continue
        start_line = line_no
        depth = _flow_depth(content)
        while depth > 0:
            index += 1
            if index >= len(physical):
                raise YamlError("unclosed flow collection", start_line)
            content += "\n" + physical[index]
            depth = _flow_depth(content)
        if depth < 0:
            raise YamlError("unbalanced flow collection", start_line)
        lines.append(LogicalLine(indent, content, start_line))
        index += 1
    return lines


def _parse_quoted(text: str, start: int, line: int) -> tuple[str, int]:
    quote = text[start]
    index = start + 1
    chars: list[str] = []
    while index < len(text):
        char = text[index]
        if quote == '"' and char == "\\":
            if index + 1 >= len(text):
                raise YamlError("unterminated escape in string", line)
            escaped = text[index + 1]
            mapping = {"n": "\n", "t": "\t", "r": "\r", '"': '"', "\\": "\\"}
            chars.append(mapping.get(escaped, escaped))
            index += 2
            continue
        if quote == "'" and char == "'" and index + 1 < len(text) and text[index + 1] == "'":
            chars.append("'")
            index += 2
            continue
        if char == quote:
            return "".join(chars), index + 1
        chars.append(char)
        index += 1
    raise YamlError("unterminated quoted string", line)


def _parse_plain_scalar(text: str) -> Any:
    value = text.strip()
    if value in {"null", "Null", "NULL", "~"}:
        return None
    lowered = value.lower()
    if lowered in {"true", "yes", "on"}:
        return True
    if lowered in {"false", "no", "off"}:
        return False
    if value.startswith(("+", "-")) and value[1:].isdigit():
        return int(value)
    if value.isdigit():
        return int(value)
    if len(value) > 1 and value.count(".") == 1:
        left, right = value.split(".")
        signed = left.startswith(("+", "-"))
        digits = left[1:] if signed else left
        if digits.isdigit() and right.isdigit():
            return float(value)
    return value


def _skip_space(text: str, index: int) -> int:
    while index < len(text) and text[index] in " \t\n\r":
        index += 1
    return index


def _parse_flow(text: str, start: int, line: int) -> tuple[Any, int]:
    index = _skip_space(text, start)
    if index >= len(text):
        raise YamlError("expected a value", line)
    char = text[index]
    if char in {'"', "'"}:
        return _parse_quoted(text, index, line)
    if char == "[":
        items: list[Any] = []
        index += 1
        while True:
            index = _skip_space(text, index)
            if index >= len(text):
                raise YamlError("unclosed flow sequence", line)
            if text[index] == "]":
                return items, index + 1
            if text[index] == ",":
                index += 1
                continue
            item, index = _parse_flow(text, index, line)
            items.append(item)
            index = _skip_space(text, index)
            if index < len(text) and text[index] == ",":
                index += 1
        raise YamlError("unclosed flow sequence", line)
    if char == "{":
        mapping: dict[str, Any] = {}
        index += 1
        while True:
            index = _skip_space(text, index)
            if index >= len(text):
                raise YamlError("unclosed flow mapping", line)
            if text[index] == "}":
                return mapping, index + 1
            if text[index] == ",":
                index += 1
                continue
            key_obj, index = _parse_flow(text, index, line)
            if not isinstance(key_obj, str):
                raise YamlError("flow mapping keys must be strings", line)
            index = _skip_space(text, index)
            if index >= len(text) or text[index] != ":":
                raise YamlError("expected ':' in flow mapping", line)
            value, index = _parse_flow(text, index + 1, line)
            mapping[key_obj] = value
            index = _skip_space(text, index)
            if index < len(text) and text[index] == ",":
                index += 1
        raise YamlError("unclosed flow mapping", line)

    end = index
    while end < len(text) and text[end] not in ",]}:\n":
        end += 1
    if end == index:
        raise YamlError("expected a scalar value", line)
    return _parse_plain_scalar(text[index:end]), end


def _parse_scalar(text: str, line: int) -> Any:
    stripped = text.strip()
    if not stripped:
        return None
    if stripped[0] in {'"', "'", "[", "{"}:
        value, index = _parse_flow(stripped, 0, line)
        index = _skip_space(stripped, index)
        if index != len(stripped):
            raise YamlError("trailing content after value", line)
        return value
    return _parse_plain_scalar(stripped)


def _split_key(text: str, line: int) -> tuple[str, str | None]:
    in_single = False
    in_double = False
    index = 0
    while index < len(text):
        char = text[index]
        if char == "\\" and in_double:
            index += 2
            continue
        if char == "'" and not in_double:
            in_single = not in_single
        elif char == '"' and not in_single:
            in_double = not in_double
        elif char == ":" and not in_single and not in_double:
            rest = text[index + 1 :]
            if rest and rest[0] not in " \t\n#":
                index += 1
                continue
            key = text[:index].strip()
            if not key:
                raise YamlError("empty mapping key", line)
            if key[0] in {'"', "'"}:
                parsed, consumed = _parse_quoted(key, 0, line)
                if consumed != len(key):
                    raise YamlError("invalid quoted mapping key", line)
                key = parsed
            value = rest.strip()
            return key, (value if value else None)
        index += 1
    return text.strip(), None


def _is_sequence_item(text: str) -> bool:
    return text == "-" or text.startswith("- ") or text.startswith("-[") or text.startswith("-{")


def _sequence_item_text(text: str) -> str:
    if text == "-":
        return ""
    if text.startswith("- "):
        return text[2:].lstrip(" ")
    return text[1:]


def _parse_mapping(lines: list[LogicalLine], index: int, indent: int) -> tuple[dict[str, Any], int]:
    mapping: dict[str, Any] = {}
    while index < len(lines):
        current = lines[index]
        if current.indent != indent or _is_sequence_item(current.text):
            break
        key, inline = _split_key(current.text, current.line)
        if key in mapping:
            raise YamlError(f"duplicate mapping key {key!r}", current.line)
        if inline is not None:
            mapping[key] = _parse_scalar(inline, current.line)
            index += 1
            continue
        if index + 1 >= len(lines) or lines[index + 1].indent <= indent:
            mapping[key] = None
            index += 1
            continue
        mapping[key], index = _parse_node(lines, index + 1, indent + 1)
    return mapping, index


def _parse_sequence(lines: list[LogicalLine], index: int, indent: int) -> tuple[list[Any], int]:
    items: list[Any] = []
    while index < len(lines):
        current = lines[index]
        if current.indent != indent or not _is_sequence_item(current.text):
            break
        item_text = _sequence_item_text(current.text)
        if item_text == "":
            if index + 1 >= len(lines) or lines[index + 1].indent <= indent:
                items.append(None)
                index += 1
                continue
            value, index = _parse_node(lines, index + 1, indent + 1)
            items.append(value)
            continue
        key, inline = _split_key(item_text, current.line)
        if ":" in item_text and (inline is not None or item_text.endswith(":")):
            mapping: dict[str, Any] = {}
            if inline is not None:
                mapping[key] = _parse_scalar(inline, current.line)
                index += 1
            elif index + 1 < len(lines) and lines[index + 1].indent > indent:
                mapping[key], index = _parse_node(lines, index + 1, indent + 1)
            else:
                mapping[key] = None
                index += 1
            while index < len(lines) and lines[index].indent > indent:
                nested, index = _parse_mapping(lines, index, lines[index].indent)
                mapping.update(nested)
            items.append(mapping)
            continue
        items.append(_parse_scalar(item_text, current.line))
        index += 1
    return items, index


def _parse_node(lines: list[LogicalLine], index: int, min_indent: int) -> tuple[Any, int]:
    if index >= len(lines):
        raise YamlError("unexpected end of file")
    current = lines[index]
    if current.indent < min_indent:
        raise YamlError("unexpected dedent", current.line)
    if _is_sequence_item(current.text):
        return _parse_sequence(lines, index, current.indent)
    if _split_key(current.text, current.line)[1] is not None or current.text.endswith(":"):
        return _parse_mapping(lines, index, current.indent)
    return _parse_scalar(current.text, current.line), index + 1


def load_yaml(text: str) -> Any:
    """Load the YAML subset used by CodeQL packs and data extensions."""
    lines = _logical_lines(text)
    if not lines:
        return None
    value, index = _parse_node(lines, 0, 0)
    if index != len(lines):
        raise YamlError("unexpected trailing content", lines[index].line)
    return value


def _rel(root: Path, path: Path) -> str:
    return path.resolve().relative_to(root.resolve()).as_posix()


def _type_name(value: Any) -> str:
    if value is None:
        return "null"
    if isinstance(value, bool):
        return "boolean"
    if isinstance(value, int):
        return "integer"
    if isinstance(value, float):
        return "float"
    if isinstance(value, list):
        return "list"
    if isinstance(value, dict):
        return "mapping"
    return type(value).__name__


def _error(path: str, message: str) -> str:
    return f"check-codeql-extension-schema: {path}: {message}"


def _validate_repository_model_scope(
    path: str,
    predicate: str,
    fields: tuple[str, ...],
    row: list[str],
    location: str,
) -> list[str]:
    """Reject valid CodeQL syntax that is too broad for this first-party pack."""
    errors: list[str] = []
    values = dict(zip(fields, row))
    callable_path = values["path"]
    if not CANONICAL_CALLABLE.fullmatch(callable_path):
        errors.append(
            _error(
                path,
                f"{location} field 1 (path): expected an exact Orbit callable path; "
                "wildcards, trait-wide selectors, and external crates are not allowed",
            )
        )

    for field_name in ("input", "output"):
        access_path = values.get(field_name)
        if access_path is not None and not ACCESS_PATH.fullmatch(access_path):
            field_index = fields.index(field_name) + 1
            errors.append(
                _error(
                    path,
                    f"{location} field {field_index} ({field_name}): expected an exact access path",
                )
            )

    if values.get("kind") != "path-injection":
        errors.append(_error(path, f"{location}: only the 'path-injection' model kind is allowed"))
    if values.get("provenance") != "manual":
        errors.append(_error(path, f"{location}: only 'manual' provenance is allowed"))
    if predicate == "barrierGuardModel" and values.get("acceptingValue") not in {"true", "false"}:
        errors.append(
            _error(path, f"{location}: acceptingValue must be exactly 'true' or 'false'")
        )
    return errors


def validate_data_extension(path: str, document: Any) -> list[str]:
    """Return schema errors for one loaded data-extension document."""
    errors: list[str] = []
    if not isinstance(document, dict):
        return [_error(path, f"expected a mapping, found {_type_name(document)}")]
    if "extensions" not in document:
        return [_error(path, "missing 'extensions' list")]
    extensions = document["extensions"]
    if not isinstance(extensions, list):
        return [_error(path, f"'extensions' must be a list, found {_type_name(extensions)}")]

    for extension_index, extension in enumerate(extensions, start=1):
        prefix = f"extension {extension_index}"
        if not isinstance(extension, dict):
            errors.append(_error(path, f"{prefix}: expected a mapping, found {_type_name(extension)}"))
            continue
        adds_to = extension.get("addsTo")
        if not isinstance(adds_to, dict):
            errors.append(
                _error(path, f"{prefix}: missing 'addsTo' mapping (found {_type_name(adds_to)})")
            )
            continue
        pack = adds_to.get("pack")
        predicate = adds_to.get("extensible")
        if not isinstance(pack, str) or not pack.strip():
            errors.append(_error(path, f"{prefix}: 'addsTo.pack' must be a non-empty string"))
            continue
        if not isinstance(predicate, str) or not predicate.strip():
            errors.append(_error(path, f"{prefix}: 'addsTo.extensible' must be a non-empty string"))
            continue
        supported = SUPPORTED_PACKS.get(pack)
        if supported is None:
            errors.append(_error(path, f"{prefix}: unsupported extension pack {pack!r}"))
            continue
        fields = supported.get(predicate)
        if fields is None:
            errors.append(
                _error(
                    path,
                    f"{prefix}: unsupported extensible predicate {predicate!r} for pack {pack!r}",
                )
            )
            continue
        rows = extension.get("data")
        if not isinstance(rows, list):
            errors.append(
                _error(path, f"{prefix}: '{predicate}' data must be a list, found {_type_name(rows)}")
            )
            continue
        expected = len(fields)
        field_list = ", ".join(fields)
        for row_index, row in enumerate(rows, start=1):
            location = f"{predicate} row {row_index}"
            if not isinstance(row, list):
                errors.append(
                    _error(path, f"{location}: expected a list of {expected} fields, found {_type_name(row)}")
                )
                continue
            if len(row) != expected:
                errors.append(
                    _error(
                        path,
                        f"{location}: expected {expected} fields ({field_list}), found {len(row)}",
                    )
                )
                continue
            for field_index, (field_name, value) in enumerate(zip(fields, row), start=1):
                if not isinstance(value, str) or not value.strip():
                    errors.append(
                        _error(
                            path,
                            f"{location} field {field_index} ({field_name}): "
                            f"expected a non-empty string, found {_type_name(value)}",
                        )
                    )
            if all(isinstance(value, str) and value.strip() for value in row):
                errors.extend(
                    _validate_repository_model_scope(path, predicate, fields, row, location)
                )
    return errors


def _load_file(path: Path) -> Any:
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as error:
        raise YamlError(str(error)) from error
    return load_yaml(text)


def _pack_data_extensions(pack_dir: Path, document: Any, pack_path: str) -> tuple[list[Path], list[str]]:
    if not isinstance(document, dict):
        return [], [_error(pack_path, f"expected a mapping, found {_type_name(document)}")]
    declared = document.get("dataExtensions")
    if declared is None:
        return [], [_error(pack_path, "missing 'dataExtensions' list")]
    if not isinstance(declared, list):
        return [], [_error(pack_path, f"'dataExtensions' must be a list, found {_type_name(declared)}")]

    files: list[Path] = []
    errors: list[str] = []
    for index, entry in enumerate(declared, start=1):
        if not isinstance(entry, str) or not entry.strip():
            errors.append(
                _error(pack_path, f"dataExtensions[{index}] must be a non-empty string")
            )
            continue
        candidate = Path(entry)
        if candidate.is_absolute() or ".." in candidate.parts:
            errors.append(
                _error(pack_path, f"dataExtensions[{index}] must be a path inside the pack directory")
            )
            continue
        if candidate.suffix != ".yaml":
            errors.append(
                _error(
                    pack_path,
                    f"dataExtensions[{index}] must end in '.yaml' to match "
                    f"the generated CodeQL discovery glob {GENERATED_EXTENSION_GLOB!r}: {entry}",
                )
            )
            continue
        resolved = (pack_dir / candidate).resolve()
        if pack_dir.resolve() not in resolved.parents and resolved != pack_dir.resolve():
            errors.append(
                _error(pack_path, f"dataExtensions[{index}] must be a path inside the pack directory")
            )
            continue
        if not resolved.is_file():
            errors.append(_error(pack_path, f"dataExtensions[{index}] not found: {entry}"))
            continue
        files.append(resolved)
    return files, errors


def discover_packs(root: Path) -> list[Path]:
    base = root / EXTENSIONS_ROOT
    if not base.is_dir():
        return []
    packs = []
    for child in sorted(path for path in base.iterdir() if path.is_dir()):
        pack = child / PACK_FILENAME
        if pack.is_file():
            packs.append(pack)
    return packs


def validate_root(root: Path) -> list[str]:
    """Validate every CodeQL data extension declared under the repository root."""
    packs = discover_packs(root)
    if not packs:
        return [
            _error(
                EXTENSIONS_ROOT.as_posix(),
                "no CodeQL extension packs found",
            )
        ]

    errors: list[str] = []
    for pack in packs:
        pack_path = _rel(root, pack)
        try:
            document = _load_file(pack)
        except YamlError as error:
            errors.append(_error(pack_path, f"malformed YAML: {error}"))
            continue
        files, pack_errors = _pack_data_extensions(pack.parent, document, pack_path)
        errors.extend(pack_errors)
        for data_file in files:
            relative = _rel(root, data_file)
            try:
                extension = _load_file(data_file)
            except YamlError as error:
                errors.append(_error(relative, f"malformed YAML: {error}"))
                continue
            errors.extend(validate_data_extension(relative, extension))
    return errors


def main(argv: list[str] | None = None) -> int:
    parser = ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root",
        type=Path,
        help="repository root containing .github/codeql/extensions (default: this repo)",
    )
    arguments = parser.parse_args(argv)
    root = (arguments.root or Path(__file__).resolve().parent.parent).resolve()

    try:
        errors = validate_root(root)
    except OSError as error:
        print(f"check-codeql-extension-schema: {error}", file=sys.stderr)
        return 2

    if errors:
        print(
            "CodeQL data-extension rows must match the pinned rust-all schema; "
            "YAML parse success is not schema validation:",
            file=sys.stderr,
        )
        for error in errors:
            print(error, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
