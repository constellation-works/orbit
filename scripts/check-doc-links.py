#!/usr/bin/env python3
"""Check live Markdown links and literal repository source paths without a build."""

from fnmatch import fnmatchcase
import html
from pathlib import Path
import re
import subprocess
import sys
import unicodedata
from urllib.parse import unquote, urlsplit


ROOT = Path(__file__).resolve().parent.parent
# Historical decisions/RCAs, templates, example links, and test fixture prose
# are not live documentation. Keep this list visible rather than a hidden allowlist.
EXCLUSIONS = (
    "docs/design/*/4_decisions.md",
    "docs/design/_templates/*",
    "docs/design/CONVENTIONS.md",
    "docs/rca/*",
    "CHANGELOG.md",
    "crates/*/tests/fixtures/*",
)
SOURCE_PATH = re.compile(
    r"^(?:crates|scripts|docs|plugin|npm|website|\.github)/"
    r"[^\s<>*{}|]+\.(?:rs|md|sh|py|js|mjs|cjs|ts|tsx|jsx|html|css|json|"
    r"yaml|yml|toml|txt|svg|png|jpg|jpeg|gif|webp|sql|lock)"
    r"(?:::\w+|:\d+(?::\d+)?|#L\d+(?:-L\d+)?)?$"
)
CODE_SPAN = re.compile(r"(?<!`)(`+)(?!`)(.+?)(?<!`)\1(?!`)", re.DOTALL)
LINK = re.compile(
    r"!?\[(?:[^\[\]\\]|\\.|\[[^\[\]]*\])*\]"
    r"\(\s*(?:<(?P<angle>[^>\n]+)>|"
    r"(?P<plain>(?:[^\s()\\]|\\.|\([^()]*\))+))"
    r"(?:\s+(?:\"[^\"]*\"|'[^']*'|\([^()]*\)))?\s*\)"
)
REFERENCE = re.compile(r"^ {0,3}\[([^]\n]+)\]:\s*(?:<([^>\n]+)>|(\S+))", re.MULTILINE)


def blank(match):
    """Mask syntax while preserving line numbers and character offsets."""
    return re.sub(r"[^\n]", " ", match.group())


def prose(text):
    """Remove examples in fenced/indented code, front matter and HTML comments."""
    text = re.sub(r"<!--.*?-->", blank, text, flags=re.DOTALL)
    if text.startswith("---\n"):
        text = re.sub(r"\A---\n.*?\n(?:---|\.\.\.)\s*(?:\n|$)", blank, text,
                      count=1, flags=re.DOTALL)
    lines = text.splitlines(keepends=True)
    fence = None
    list_indent = None
    indented_code = False
    previous_blank = True
    for index, line in enumerate(lines):
        indent = len(line) - len(line.lstrip(" \t"))
        item = re.match(r"^( *)(?:[-+*]|\d+[.)])\s+", line)
        if item:
            list_indent = len(item[1])
        elif line.strip() and list_indent is not None and indent <= list_indent:
            list_indent = None
        marker = re.match(r"^ {0,3}(`{3,}|~{3,})", line)
        if fence:
            lines[index] = re.sub(r"[^\n]", " ", line)
            if re.match(r"^ {0,3}" + re.escape(fence[0]) +
                        "{" + str(fence[1]) + r",}\s*$", line):
                fence = None
        elif marker:
            fence = (marker[1][0], len(marker[1]))
            lines[index] = re.sub(r"[^\n]", " ", line)
        elif line.startswith(("    ", "\t")) and list_indent is None:
            # Indentation in list items and lazy paragraph continuations is
            # prose; an indented code block starts only after a blank line.
            if previous_blank or indented_code:
                lines[index] = re.sub(r"[^\n]", " ", line)
                indented_code = True
        elif line.strip():
            indented_code = False
        previous_blank = not line.strip()
    return "".join(lines)


def heading_text(text):
    """Extract rendered inline heading text, preserving literal underscores."""
    codes = []

    def save_code(match):
        codes.append(match[2].strip().replace("\n", " "))
        return f"\x00{len(codes) - 1}\x00"

    text = CODE_SPAN.sub(save_code, text)
    text = re.sub(r"!?\[([^]]+)\]\([^)]*\)", r"\1", text)
    text = re.sub(r"<[^>]*>", "", text)
    text = re.sub(r"(?<!\w)(_+)(.+?)\1(?!\w)", r"\2", text)
    text = text.replace("*", "").replace("~", "")
    text = re.sub(r"\x00(\d+)\x00", lambda match: codes[int(match[1])], text)
    return html.unescape(re.sub(r"\\([!\"#$%&'()*+,\-./:;<=>?@\[\]^_`{|}~])", r"\1", text))


def anchors(text):
    """GitHub slugs retain hyphens/underscores and disambiguate duplicates."""
    result = set()
    lines = prose(text).splitlines()
    for index, line in enumerate(lines):
        match = re.match(r"^ {0,3}#{1,6}\s+(.+?)\s*#*\s*$", line)
        if match:
            heading = match[1]
        elif index and re.fullmatch(r" {0,3}(?:=+|-+)\s*", line) and lines[index - 1].strip():
            heading = lines[index - 1].strip()
        else:
            continue
        heading = heading_text(heading).strip().lower()
        slug = "".join(char for char in heading
                       if char in " -_" or unicodedata.category(char)[0] in "LNM")
        slug = slug.replace(" ", "-")
        candidate, suffix = slug, 0
        while candidate in result:
            suffix += 1
            candidate = f"{slug}-{suffix}"
        result.add(candidate)
    # Explicit HTML anchors are also valid link destinations on GitHub.
    result.update(re.findall(r"<(?:a|[a-z][\w-]*)\b[^>]*\b(?:id|name)=[\"']([^\"']+)[\"']",
                             prose(text), flags=re.IGNORECASE))
    return result


def check(root):
    tracked = set(subprocess.check_output(
        ["git", "ls-files", "-z"], cwd=root).decode().split("\0")) - {""}
    documents = sorted(path for path in tracked if path.endswith(".md")
                       and not any(fnmatchcase(path, pattern) for pattern in EXCLUSIONS))
    heading_cache = {}
    errors = []
    for document in documents:
        path = root / document
        if not path.is_file():
            # A tracked deletion is not part of the candidate documentation.
            continue
        text = prose(path.read_text(encoding="utf-8"))

        def report(offset, message):
            errors.append(f"{document}:{text.count(chr(10), 0, offset) + 1}: {message}")

        for match in CODE_SPAN.finditer(text):
            value = match[2].strip()
            if SOURCE_PATH.fullmatch(value):
                target = re.split(r"::|:\d|#L", value, maxsplit=1)[0]
                if target not in tracked or not (root / target).is_file():
                    report(match.start(), f"missing tracked source path: {target}")
        # A displayed code example is not a link, but backticks in a link's
        # label must not hide the destination that follows it.
        links = CODE_SPAN.sub(blank, text)
        # Escaped opening brackets cannot start a Markdown link. Preserve
        # offsets so diagnostics still identify the original source line.
        links = re.sub(r"\\\\|\\\[", blank, links)
        destinations = [(match.start(), match["angle"] or match["plain"])
                        for match in LINK.finditer(links)]
        # Validate definitions only when used: provenance such as [issue]: prose
        # can look like an unused reference definition.
        definitions = {match[1].strip().casefold(): match[2] or match[3]
                       for match in REFERENCE.finditer(links)}
        reference_uses = REFERENCE.sub(blank, links)
        reference_uses = LINK.sub(blank, reference_uses)
        for match in re.finditer(r"!?\[([^]\n]+)\](?:\[([^]\n]*)\])?", reference_uses):
            label = (match[2] or match[1]).strip().casefold()
            if label in definitions:
                destinations.append((match.start(), definitions[label]))
        for offset, destination in destinations:
            destination = html.unescape(re.sub(r"\\(.)", r"\1", destination))
            url = urlsplit(destination)
            if url.scheme or url.netloc or url.path.startswith("/"):
                continue
            target = (path.parent / unquote(url.path)).resolve() if url.path else path
            if (document.startswith("website/src/content/docs/") and url.path
                    and not Path(url.path).suffix):
                # Starlight pages have directory URLs: ../ from a page's slug
                # refers to a sibling page, not its parent's sibling file.
                page = path.parent if path.stem == "index" else path.with_suffix("")
                route = (page / unquote(url.path)).resolve()
                target = route.with_suffix(".md")
                if not target.is_file():
                    target = route / "index.md"
            if not target.exists():
                report(offset, f"missing link target: {destination}")
                continue
            if target.suffix.lower() == ".md" and url.fragment:
                if target not in heading_cache:
                    heading_cache[target] = anchors(target.read_text(encoding="utf-8"))
                if unquote(url.fragment) not in heading_cache[target]:
                    report(offset, f"missing heading anchor: {destination}")
    for error in errors:
        print(error, file=sys.stderr)
    if errors:
        print(f"doc-links: {len(errors)} broken references in {len(documents)} Markdown files",
              file=sys.stderr)
        return 1
    print(f"doc-links: checked {len(documents)} Markdown files")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(check(ROOT))
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"doc-links: {error}", file=sys.stderr)
        sys.exit(2)
