#!/usr/bin/env python3
"""Enforce STD-02 §R43: unit map in ARCHITECTURE.md + crate pages, path existence, first-line //! docs, and one destination row per orbit-core application entry."""

import argparse
from dataclasses import dataclass
import json
from pathlib import Path
import re
import subprocess
import sys
from urllib.parse import unquote, urlsplit


ROOT = Path(__file__).resolve().parent.parent

REPO_ROOT_PREFIXES = (
    "crates/",
    "docs/",
    "scripts/",
    "desktop/",
    "plugin/",
    "npm/",
    "website/",
    ".github/",
)
FILE_EXTENSIONS = (
    ".rs",
    ".md",
    ".sh",
    ".py",
    ".toml",
    ".yaml",
    ".yml",
    ".json",
    ".sql",
    ".js",
    ".mjs",
    ".cjs",
    ".ts",
    ".tsx",
    ".css",
    ".html",
    ".txt",
)
CRATE_TARGET_KINDS = {"lib", "bin", "proc-macro", "cdylib", "rlib", "dylib", "staticlib"}

CODE_SPAN = re.compile(r"(?<!`)(`+)(?!`)(.+?)(?<!`)\1(?!`)", re.DOTALL)
MD_LINK = re.compile(
    r"!?\[(?:[^\[\]\\]|\\.|\[[^\[\]]*\])*\]"
    r"\(\s*(?:<(?P<angle>[^>\n]+)>|"
    r"(?P<plain>(?:[^\s()\\]|\\.|\([^()]*\))+))"
    r"(?:\s+(?:\"[^\"]*\"|'[^']*'|\([^()]*\)))?\s*\)"
)
CRATE_LINK_CELL = re.compile(r"^\[`?([A-Za-z0-9_-]+)`?\]\(([^)]+)\)$")
CRATE_PLAIN_CELL = re.compile(r"^`?([A-Za-z0-9_-]+)`?$")
BULLET_MODULE = re.compile(r"^\s*[-*]\s+`([A-Za-z_][A-Za-z0-9_]*)`\s*(?::|—|--|-)\s*(.*)$")
TABLE_MODULE = re.compile(r"^\s*\|\s*`([A-Za-z_][A-Za-z0-9_]*)`\s*\|\s*(.*?)\s*\|")
RELATIVE_PATH_TOKEN = re.compile(r"^[a-z0-9_.-]+(?:/[a-z0-9_.-]+)+$")
TRAILING_SLASH_TOKEN = re.compile(r"^[A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+)*/$")
PATH_SUFFIX = re.compile(r"(?:::\S+|:\d+(?::\d+)?|#L\d+(?:-L\d+)?)$")
APPLICATION_DIR = Path("crates/orbit-core/src/application")
APPLICATION_PAGE = Path("docs/architecture/orbit-core.md")
APPLICATION_MAP_HEADING = "Application module destinations"
APPLICATION_DESTINATION_PREFIXES = ("app::", "unit::", "migrate", "dissolved")
APPLICATION_ENTRY_CELL = re.compile(r"^`([A-Za-z0-9_.-]+)`$")


@dataclass(frozen=True)
class ModuleUnit:
    name: str
    doc_file: Path | None


@dataclass(frozen=True)
class CrateUnits:
    name: str
    crate_dir: Path
    root_files: tuple[Path, ...]
    modules: tuple[ModuleUnit, ...]


def blank(match):
    """Mask syntax while preserving line numbers and character offsets."""
    return re.sub(r"[^\n]", " ", match.group())


def strip_non_prose(text):
    """Mask YAML frontmatter, HTML comments, and fenced code blocks while keeping line offsets."""
    text = re.sub(r"<!--.*?-->", blank, text, flags=re.DOTALL)
    if text.startswith("---\n"):
        text = re.sub(
            r"\A---\n.*?\n(?:---|\.\.\.)\s*(?:\n|$)",
            blank,
            text,
            count=1,
            flags=re.DOTALL,
        )
    lines = text.splitlines(keepends=True)
    fence = None
    for index, line in enumerate(lines):
        marker = re.match(r"^ {0,3}(`{3,}|~{3,})", line)
        if fence:
            lines[index] = re.sub(r"[^\n]", " ", line)
            if re.match(
                r"^ {0,3}" + re.escape(fence[0]) + "{" + str(fence[1]) + r",}\s*$",
                line,
            ):
                fence = None
        elif marker:
            fence = (marker.group(1)[0], len(marker.group(1)))
            lines[index] = re.sub(r"[^\n]", " ", line)
    return "".join(lines)


def extract_section(masked_text, heading_title, level=2):
    """Return (start_line_index, lines) for a Markdown heading section, or None."""
    prefix = "#" * level + " "
    lines = masked_text.splitlines()
    start = None
    for index, line in enumerate(lines):
        stripped = line.strip()
        if stripped == f"{prefix}{heading_title}":
            start = index + 1
            continue
        if start is not None and re.match(rf"^#{{1,{level}}}\s+", stripped):
            return start, lines[start:index]
    if start is not None:
        return start, lines[start:]
    return None


def discover_workspace_crates(root):
    """Discover workspace crates, their root files, and top-level modules via cargo metadata."""
    manifest = root / "Cargo.toml"
    if not manifest.is_file():
        raise ValueError(f"missing workspace manifest: {manifest}")

    proc = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps", "--manifest-path", str(manifest)],
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        detail = (proc.stderr or proc.stdout).strip()
        raise ValueError(f"cargo metadata failed (exit {proc.returncode}): {detail}")

    try:
        metadata = json.loads(proc.stdout)
    except json.JSONDecodeError as err:
        raise ValueError(f"cargo metadata returned invalid JSON: {err}") from err

    workspace_members = set(metadata.get("workspace_members", []))
    packages = [
        pkg for pkg in metadata.get("packages", []) if pkg.get("id") in workspace_members
    ]
    if not packages:
        raise ValueError("no workspace crates discovered from cargo metadata")

    crates = []
    errors = []
    for pkg in sorted(packages, key=lambda item: item["name"]):
        name = pkg["name"]
        crate_dir = Path(pkg["manifest_path"]).resolve().parent
        src_dir = crate_dir / "src"
        if not src_dir.is_dir():
            errors.append(f"{name}: missing src/ directory at {rel(root, src_dir)}")
            continue

        root_files = []
        seen_roots = set()
        for target in pkg.get("targets", []):
            kinds = set(target.get("kind", []))
            if not (kinds & CRATE_TARGET_KINDS):
                continue
            target_path = Path(target["src_path"]).resolve()
            if target_path.parent == src_dir and target_path not in seen_roots:
                seen_roots.add(target_path)
                root_files.append(target_path)
        if not root_files:
            for fallback in ("lib.rs", "main.rs"):
                candidate = src_dir / fallback
                if candidate.is_file():
                    root_files.append(candidate)
        root_files.sort()
        if not root_files:
            errors.append(f"{name}: no crate root file found under {rel(root, src_dir)}")

        root_names = {path.name for path in root_files} | {"lib.rs", "main.rs"}
        modules = []
        for entry in sorted(src_dir.iterdir(), key=lambda p: p.name):
            if entry.name.startswith(".") or entry.name in root_names:
                continue
            if entry.name in ("tests", "tests.rs"):
                continue
            if entry.is_file() and entry.suffix == ".rs":
                modules.append(ModuleUnit(name=entry.stem, doc_file=entry))
            elif entry.is_dir():
                if entry.name == "bin" and not (entry / "mod.rs").is_file():
                    continue
                if (src_dir / f"{entry.name}.rs").is_file():
                    continue
                mod_rs = entry / "mod.rs"
                modules.append(ModuleUnit(name=entry.name, doc_file=mod_rs if mod_rs.is_file() else None))

        crates.append(
            CrateUnits(
                name=name,
                crate_dir=crate_dir,
                root_files=tuple(root_files),
                modules=tuple(modules),
            )
        )
    return crates, errors


def rel(root, path):
    try:
        return str(path.resolve().relative_to(root.resolve()))
    except ValueError:
        return str(path)


def parse_crates_table(arch_masked):
    """Parse the `## Crates` table in ARCHITECTURE.md."""
    section = extract_section(arch_masked, "Crates", level=2)
    if section is None:
        return None, None, ["ARCHITECTURE.md: missing `## Crates` section"]

    start_line, lines = section
    errors = []
    entries = {}
    header_cols = None
    owns_index = None
    in_table = False

    for offset, line in enumerate(lines):
        lineno = start_line + offset + 1
        stripped = line.strip()
        if not stripped.startswith("|"):
            if in_table:
                break
            continue
        cells = [cell.strip() for cell in stripped.strip("|").split("|")]
        if header_cols is None:
            header_cols = [col.lower() for col in cells]
            if "owns" in header_cols:
                owns_index = header_cols.index("owns")
            in_table = True
            continue
        if all(re.fullmatch(r":?-{2,}:?", cell) for cell in cells if cell):
            continue
        if not cells:
            continue
        first = cells[0]
        link_match = CRATE_LINK_CELL.fullmatch(first)
        if link_match:
            crate_name = link_match.group(1)
            link_target = link_match.group(2).strip()
        else:
            plain_match = CRATE_PLAIN_CELL.fullmatch(first)
            if not plain_match:
                errors.append(f"ARCHITECTURE.md:{lineno}: unrecognized crate cell {first!r}")
                continue
            crate_name = plain_match.group(1)
            link_target = None

        if crate_name in entries:
            errors.append(f"ARCHITECTURE.md:{lineno}: duplicate crate row '{crate_name}'")
            continue

        if owns_index is not None:
            owns_text = cells[owns_index] if owns_index < len(cells) else ""
            if not owns_text or owns_text in ("—", "-", "TBD"):
                errors.append(f"ARCHITECTURE.md:{lineno}: crate '{crate_name}' has an empty Owns cell")

        entries[crate_name] = (lineno, link_target)

    return entries, owns_index is not None, errors


def parse_module_entries(section_lines, start_line, doc_label):
    """Parse `- `name`: sentence` or table rows under `## Ownership by module`."""
    entries = {}
    errors = []
    for offset, line in enumerate(section_lines):
        lineno = start_line + offset + 1
        bullet = BULLET_MODULE.match(line)
        if bullet:
            name, desc = bullet.group(1), bullet.group(2).strip()
        else:
            table = TABLE_MODULE.match(line)
            if not table:
                continue
            name, desc = table.group(1), table.group(2).strip()
        if name in entries:
            errors.append(f"{doc_label}:{lineno}: duplicate module entry '{name}'")
            continue
        if not desc or desc in ("—", "-", "TBD"):
            errors.append(f"{doc_label}:{lineno}: module '{name}' has an empty ownership description")
        entries[name] = lineno
    return entries, errors


def check_first_line_doc(root, path):
    """Verify a Rust unit file starts on line 1 with a non-empty `//!` doc comment."""
    rel_path = rel(root, path)
    if not path.is_file():
        return f"{rel_path}:1: missing unit source file (STD-02 R43)"
    try:
        with path.open("r", encoding="utf-8") as handle:
            first_line = handle.readline().rstrip("\r\n")
    except OSError as err:
        return f"{rel_path}:1: cannot read unit source file: {err}"
    if not first_line.startswith("//!") or not first_line[3:].strip():
        return f"{rel_path}:1: missing first-line //! doc comment (STD-02 R43)"
    return None


def resolve_code_path(token, root, crate_dir=None):
    """Return (is_path, exists) for an inline code-formatted token."""
    raw = token.strip()
    if not raw or any(ch.isspace() for ch in raw):
        return False, True
    if any(ch in raw for ch in ("*", "<", ">", "{", "}", "?", "$", "|")):
        return False, True
    if raw.startswith(("/", "~", ".", "http://", "https://")):
        return False, True

    candidate = PATH_SUFFIX.sub("", raw)
    if "::" in candidate:
        return False, True

    # 1. Explicit repo-root path (file or directory).
    if candidate.startswith(REPO_ROOT_PREFIXES):
        stripped = candidate.rstrip("/")
        return True, (root / stripped).exists()

    # 2. Trailing-slash directory path.
    if TRAILING_SLASH_TOKEN.fullmatch(candidate):
        stripped = candidate.rstrip("/")
        if stripped == "tests" and crate_dir is not None:
            if (crate_dir / "tests").is_dir() or any(
                p.is_dir() and p.name == "tests" for p in (crate_dir / "src").rglob("tests")
            ):
                return True, True
        candidates = [root / stripped]
        if crate_dir is not None:
            candidates.extend([crate_dir / "src" / stripped, crate_dir / stripped])
        return True, any(path.is_dir() for path in candidates)

    # 3. Slash path ending with a known source/doc extension.
    if "/" in candidate and candidate.endswith(FILE_EXTENSIONS):
        candidates = [root / candidate]
        if crate_dir is not None:
            candidates.extend([crate_dir / "src" / candidate, crate_dir / candidate])
        return True, any(path.is_file() for path in candidates)

    # 4. Lowercase relative multi-segment path without extension (e.g. `driver/file`, `src/foo`).
    if RELATIVE_PATH_TOKEN.fullmatch(candidate):
        candidates = [root / candidate, root / f"{candidate}.rs"]
        if crate_dir is not None:
            candidates.extend(
                [
                    crate_dir / "src" / candidate,
                    crate_dir / "src" / f"{candidate}.rs",
                    crate_dir / candidate,
                    crate_dir / f"{candidate}.rs",
                ]
            )
        return True, any(path.exists() for path in candidates)

    return False, True


def check_doc_paths_and_links(doc_path, masked_text, root, crate_dir=None):
    """Validate every code-formatted repository path and relative Markdown link in a doc."""
    errors = []
    doc_label = rel(root, doc_path)

    for match in CODE_SPAN.finditer(masked_text):
        token = match.group(2)
        is_path, exists = resolve_code_path(token, root, crate_dir=crate_dir)
        if is_path and not exists:
            lineno = masked_text.count("\n", 0, match.start()) + 1
            errors.append(f"{doc_label}:{lineno}: documented path does not exist: `{token.strip()}`")

    link_text = CODE_SPAN.sub(blank, masked_text)
    for match in MD_LINK.finditer(link_text):
        destination = (match.group("angle") or match.group("plain") or "").strip()
        if not destination:
            continue
        parsed = urlsplit(destination)
        if parsed.scheme or parsed.netloc or parsed.path.startswith("/"):
            continue
        if not parsed.path:
            continue
        target = (doc_path.parent / unquote(parsed.path)).resolve()
        if not target.exists():
            lineno = masked_text.count("\n", 0, match.start()) + 1
            errors.append(f"{doc_label}:{lineno}: broken relative link target: {destination}")

    return errors


def application_entries(root):
    """Top-level entries of orbit-core's application/: directories and .rs files, minus the module root."""
    app_dir = root / APPLICATION_DIR
    if not app_dir.is_dir():
        return {}
    entries = {}
    for child in sorted(app_dir.iterdir(), key=lambda p: p.name):
        if child.name.startswith(".") or child.name == "mod.rs":
            continue
        if child.is_dir() or (child.is_file() and child.suffix == ".rs"):
            entries[child.name] = child
    return entries


def parse_application_map(masked_text, doc_label):
    """Parse the `## Application module destinations` table into {entry: lineno}."""
    section = extract_section(masked_text, APPLICATION_MAP_HEADING, level=2)
    if section is None:
        return None, []
    start_line, lines = section
    rows = {}
    errors = []
    header_seen = False
    for offset, line in enumerate(lines):
        stripped = line.strip()
        if not stripped.startswith("|"):
            continue
        cells = [cell.strip() for cell in stripped.strip("|").split("|")]
        if not header_seen:
            header_seen = True
            continue
        if all(re.fullmatch(r":?-{2,}:?", cell) for cell in cells if cell):
            continue
        lineno = start_line + offset + 1
        entry = APPLICATION_ENTRY_CELL.fullmatch(cells[0]) if cells else None
        if not entry:
            errors.append(
                f"{doc_label}:{lineno}: application map entry must be a backticked name: {cells[0]!r}"
            )
            continue
        name = entry.group(1)
        if name in rows:
            errors.append(f"{doc_label}:{lineno}: duplicate application map row '{name}'")
            continue
        destination = cells[1].replace("`", "") if len(cells) > 1 else ""
        if not destination.startswith(APPLICATION_DESTINATION_PREFIXES):
            errors.append(
                f"{doc_label}:{lineno}: '{name}' destination must start with app::, unit::, migrate or dissolved"
            )
        rows[name] = lineno
    return rows, errors


def check_application_map(root):
    """Require one destination row per top-level entry of orbit-core's application/ (STD-02 §R43)."""
    entries = application_entries(root)
    app_dir_present = (root / APPLICATION_DIR).is_dir()
    label = APPLICATION_PAGE.as_posix()
    page = root / APPLICATION_PAGE
    if not page.is_file():
        return [f"{label}: missing application destination page"] if app_dir_present else []
    masked = strip_non_prose(page.read_text(encoding="utf-8"))
    rows, errors = parse_application_map(masked, label)
    if rows is None:
        if app_dir_present:
            errors.append(
                f"{label}: missing `## {APPLICATION_MAP_HEADING}` section for {APPLICATION_DIR.as_posix()}"
            )
        return errors
    for name in sorted(entries):
        if name not in rows:
            errors.append(f"{label}: no destination row for application/{name}")
    for name, lineno in sorted(rows.items(), key=lambda item: item[1]):
        if name in entries:
            continue
        errors.append(
            f"{label}:{lineno}: row '{name}' does not match an entry of {APPLICATION_DIR.as_posix()}/"
        )
    return errors


def check(root):
    root = root.resolve()
    crates, discovery_errors = discover_workspace_crates(root)
    errors = list(discovery_errors)
    crates_by_name = {crate.name: crate for crate in crates}

    arch_path = root / "ARCHITECTURE.md"
    if not arch_path.is_file():
        errors.append("ARCHITECTURE.md: missing architecture document at repository root")
        return crates, errors

    arch_raw = arch_path.read_text(encoding="utf-8")
    arch_masked = strip_non_prose(arch_raw)
    errors.extend(check_doc_paths_and_links(arch_path, arch_masked, root))

    crate_rows, _has_owns_col, table_errors = parse_crates_table(arch_masked)
    errors.extend(table_errors)
    crate_rows = crate_rows or {}

    for crate in crates:
        if crate.name not in crate_rows:
            errors.append(f"ARCHITECTURE.md: missing workspace crate '{crate.name}' in ## Crates")

    for mapped_crate, (lineno, _target) in crate_rows.items():
        if mapped_crate not in crates_by_name:
            errors.append(
                f"ARCHITECTURE.md:{lineno}: mapped crate '{mapped_crate}' does not exist in cargo metadata"
            )

    checked_docs = set()
    for crate in crates:
        # 1. Check first-line //! doc comments on crate roots and top-level modules.
        for root_file in crate.root_files:
            err = check_first_line_doc(root, root_file)
            if err:
                errors.append(err)
        for mod in crate.modules:
            if mod.doc_file is None:
                errors.append(
                    f"{rel(root, crate.crate_dir / 'src' / mod.name)}: "
                    "top-level module directory lacks mod.rs and first-line //! doc comment (STD-02 R43)"
                )
            else:
                err = check_first_line_doc(root, mod.doc_file)
                if err:
                    errors.append(err)

        # 2. Check crate's module map in its linked crate page (or in-file section).
        if crate.name not in crate_rows:
            continue
        lineno, link_target = crate_rows[crate.name]
        if link_target:
            target_path = (arch_path.parent / unquote(urlsplit(link_target).path)).resolve()
            if not target_path.is_file():
                errors.append(
                    f"ARCHITECTURE.md:{lineno}: crate '{crate.name}' links to missing page {link_target}"
                )
                continue
            crate_doc_path = target_path
            crate_doc_masked = strip_non_prose(crate_doc_path.read_text(encoding="utf-8"))
            if crate_doc_path not in checked_docs:
                checked_docs.add(crate_doc_path)
                errors.extend(
                    check_doc_paths_and_links(
                        crate_doc_path, crate_doc_masked, root, crate_dir=crate.crate_dir
                    )
                )

            owns_section = extract_section(crate_doc_masked, "What it owns", level=2)
            if owns_section is None or not any(line.strip() for line in owns_section[1]):
                errors.append(
                    f"{rel(root, crate_doc_path)}: missing or empty `## What it owns` section"
                )

            mod_section = extract_section(crate_doc_masked, "Ownership by module", level=2)
            doc_label = rel(root, crate_doc_path)
        else:
            mod_section = extract_section(arch_masked, crate.name, level=3)
            if mod_section is None:
                mod_section = extract_section(arch_masked, crate.name, level=2)
            doc_label = "ARCHITECTURE.md"

        disk_modules = {mod.name for mod in crate.modules}
        if mod_section is None:
            if disk_modules:
                errors.append(
                    f"{doc_label}: missing `## Ownership by module` section for crate '{crate.name}'"
                )
            continue

        mapped_modules, mod_errors = parse_module_entries(
            mod_section[1], mod_section[0], doc_label
        )
        errors.extend(mod_errors)

        for mod_name in sorted(disk_modules - set(mapped_modules)):
            errors.append(
                f"{doc_label}: missing top-level module '{mod_name}' for crate '{crate.name}'"
            )
        for mod_name in sorted(set(mapped_modules) - disk_modules):
            mod_line = mapped_modules[mod_name]
            errors.append(
                f"{doc_label}:{mod_line}: mapped module '{mod_name}' does not exist under "
                f"{rel(root, crate.crate_dir / 'src')}"
            )

    readme_path = root / "docs/architecture/README.md"
    if readme_path.is_file() and readme_path.resolve() not in checked_docs:
        readme_masked = strip_non_prose(readme_path.read_text(encoding="utf-8"))
        errors.extend(check_doc_paths_and_links(readme_path, readme_masked, root))

    errors.extend(check_application_map(root))
    return crates, errors


def main(argv=None):
    parser = argparse.ArgumentParser(
        description="Verify STD-02 §R43 architecture unit map, paths, and first-line //! docs."
    )
    parser.add_argument("--root", type=Path, default=ROOT, help="Repository root to inspect")
    args = parser.parse_args(argv)

    try:
        crates, errors = check(args.root)
    except (OSError, ValueError) as err:
        print(f"architecture-map: {err}", file=sys.stderr)
        return 2

    if errors:
        for error in errors:
            print(error, file=sys.stderr)
        print(
            f"architecture-map: {len(errors)} violation(s) across {len(crates)} crate(s)",
            file=sys.stderr,
        )
        return 1

    total_modules = sum(len(crate.modules) for crate in crates)
    print(f"architecture-map: checked {len(crates)} crates and {total_modules} top-level modules")
    return 0


if __name__ == "__main__":
    sys.exit(main())
