#!/usr/bin/env python3
"""Fail when a dashboard browser fixture queries a hook that no longer exists.

Reads every ``crates/orbit-web/**/*_browser.mjs`` fixture, extracts the
literal element ids, CSS classes, ``data-*`` attributes and ``/api/...``
paths it queries, and fails when one has no occurrence in
``crates/orbit-web/assets/dashboard/`` or the web API routes
(``crates/orbit-web/src/api/routes.rs``). Each failure names the fixture,
the line and the missing hook.

Known limit: this catches removed or renamed hooks (the ORB-17392,
ORB-15324 and ORB-15281 shapes), not computed geometry such as
ORB-17831's hard-coded track width. A fixture that measures pixel
geometry against a changed layout still needs the browser run itself.

This is a lexical guard, not a JavaScript parser: only literal hooks in
string/template literals (plus ``getElementById`` and ``dataset.*``
lookups) are extracted. A ``getElementById`` probe is exempt only when the
fixture directly asserts the element absent (a null/undefined value or a
comparison asserted true, ``assert.ok(!...)``, or a Boolean asserted false).
A null comparison alone and ``Boolean(...)``/``!!`` alone remain queries.
Computed selectors and paths are skipped. An id or
class the assets build from a template (``id = `detail-${task.id}```,
``class: `chip chip-${tone}```) satisfies only a fixture hook of that kind
that the whole template can produce.
"""

import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SENTINEL = "\x00"  # marks a ${...} interpolation inside a template literal

ID_RE = re.compile(r"#([A-Za-z_][A-Za-z0-9_-]*)")
CLASS_RE = re.compile(r"\.([A-Za-z_][A-Za-z0-9_-]*)")
DATA_RE = re.compile(r"\b(data-[a-z0-9][a-z0-9_-]*)")
API_RE = re.compile(r"(?<![A-Za-z0-9_/.])/api/(?:[A-Za-z0-9_.\-/]|\x00)+")
GET_BY_ID_RE = re.compile(r"getElementById\(\s*['\"`]([^'\"`\s]+)['\"`]")
DATASET_RE = re.compile(r"\.dataset\.([A-Za-z_$][A-Za-z0-9_$]*)")
DATASET_BRACKET_RE = re.compile(r"\.dataset\[\s*['\"]([^'\"]+)['\"]\s*\]")
CLASSLIST_RE = re.compile(
    r"classList\.contains\(\s*['\"]([A-Za-z_][A-Za-z0-9_-]*)['\"]")
ROUTE_RE = re.compile(r"\.route\(\s*\"(/[^\"]*)\"")


def _parse_quoted(source, start):
    """Return (content, end_index) for a ' or \" literal at start."""
    quote = source[start]
    out = []
    index = start + 1
    while index < len(source):
        char = source[index]
        if char == "\\" and index + 1 < len(source):
            out.append(source[index + 1])
            index += 2
        elif char == quote:
            return "".join(out), index + 1
        elif char == "\n":
            break
        else:
            out.append(char)
            index += 1
    return "".join(out), index


def _parse_template(source, start, found, expressions=None):
    """Return (content, end_index) for a ` template at start.

    Interpolations become one SENTINEL; literals nested inside an
    interpolation are appended to ``found`` as (content, offset), and
    the source text of each interpolation to ``expressions``.
    """
    out = []
    index = start + 1
    while index < len(source):
        char = source[index]
        if char == "\\" and index + 1 < len(source):
            out.append(source[index + 1])
            index += 2
        elif char == "`":
            return "".join(out), index + 1
        elif char == "$" and source[index + 1:index + 2] == "{":
            out.append(SENTINEL)
            index += 2
            expression_start = index
            depth = 1
            while index < len(source) and depth:
                inner = source[index]
                if inner in "'\"":
                    content, index = _parse_quoted(source, index)
                    found.append((content, index))
                elif inner == "`":
                    content, index = _parse_template(source, index, found)
                    found.append((content, index))
                elif inner == "{":
                    depth += 1
                    index += 1
                elif inner == "}":
                    depth -= 1
                    index += 1
                    if not depth and expressions is not None:
                        expressions.append(source[expression_start:index - 1].strip())
                else:
                    index += 1
        else:
            out.append(char)
            index += 1
    return "".join(out), index


def literals(source):
    """Yield (content, offset) for every string/template literal."""
    found = []
    index = 0
    while index < len(source):
        if source.startswith("//", index):
            end = source.find("\n", index)
            index = len(source) if end < 0 else end + 1
        elif source.startswith("/*", index):
            end = source.find("*/", index + 2)
            index = len(source) if end < 0 else end + 2
        elif source[index] in "'\"":
            content, end = _parse_quoted(source, index)
            found.append((content, index))
            index = end
        elif source[index] == "`":
            nested = []
            content, end = _parse_template(source, index, nested)
            found.append((content, index))
            # Nested literals end before the outer one's offset; the
            # line is computed from their own start where known.
            found.extend(nested)
            index = end
        else:
            index += 1
    return found


TAGS = {
    "a", "article", "aside", "button", "code", "dd", "details", "div", "dl",
    "dt", "em", "fieldset", "figcaption", "figure", "footer", "form", "h1",
    "h2", "h3", "h4", "header", "hr", "img", "input", "label", "legend",
    "li", "main", "nav", "ol", "option", "p", "path", "pre", "section",
    "select", "small", "span", "strong", "summary", "svg", "table", "tbody",
    "td", "textarea", "th", "thead", "tr", "ul",
}


def is_selector_like(content):
    # A selector literal is one line of selector characters. Multi-line
    # literals are injected harness code or prose, and a '/' or ';'
    # never appears in the selectors the fixtures query with.
    text = content.strip()
    if not text or "\n" in text or "\r" in text:
        return False
    if any(char in text for char in "/;<>{}"):
        return False
    if text[0] in ".#[:*":
        return True
    if re.search(r"[\s,>+~]\s*[.#\[]", text):
        return True
    match = re.match(r"([A-Za-z][A-Za-z0-9-]*)(?=[.#\[:>+~ ]|$)", text)
    return bool(match and match.group(1).lower() in TAGS)


def split_selector_list(content):
    """Split a selector list on top-level commas."""
    parts, depth, current = [], 0, []
    for char in content:
        if char in "[(":
            depth += 1
        elif char in "])":
            depth -= 1
        if char == "," and depth == 0:
            parts.append("".join(current))
            current = []
        else:
            current.append(char)
    parts.append("".join(current))
    return parts


def defined_hooks(source):
    """Hooks a fixture creates itself, so querying them is not a
    dashboard query: ids/classes/data attributes in its injected
    markup, dataset writes, classList additions and setAttribute."""
    defined = {"id": set(), "class": set(), "data attribute": set()}
    for content, _offset in literals(source):
        if "<" not in content:
            continue  # only injected markup defines hooks this way
        for match in re.finditer(r"\bid\s*=\s*[\"']([^\"']+)[\"']", content):
            defined["id"].add(match.group(1))
        for match in re.finditer(r"\bclass\s*=\s*[\"']([^\"']+)[\"']", content):
            defined["class"].update(match.group(1).split())
        for match in re.finditer(r"\b(data-[a-z0-9_-]+)\s*=", content):
            defined["data attribute"].add(match.group(1))
    for match in re.finditer(r"\.dataset\.([A-Za-z_$][A-Za-z0-9_$]*)\s*=(?!=)", source):
        defined["data attribute"].add("data-" + camel_to_kebab(match.group(1)))
    for match in re.finditer(
            r"setAttribute\(\s*['\"](data-[a-z0-9_-]+|id|class)['\"]\s*,\s*['\"]([^'\"]+)", source):
        kind = "data attribute" if match.group(1).startswith("data-") else match.group(1)
        if kind == "class":
            defined["class"].update(match.group(2).split())
        elif kind == "id":
            defined["id"].add(match.group(2))
        else:
            defined["data attribute"].add(match.group(1))
    for match in re.finditer(
            r"classList\.(?:add|toggle)\(\s*['\"]([A-Za-z_][A-Za-z0-9_-]*)['\"]", source):
        defined["class"].add(match.group(1))
    return defined


def camel_to_kebab(name):
    return re.sub(r"(?<!^)(?=[A-Z])", "-", name).lower()


NEGATIVE_COUNT_RE = re.compile(r"\.count\(\)[^\n]*(?:,\s*0\b|===\s*0|==\s*0)")


ASSERT_CALL = r"\bassert(?:\.\w+)?\(\s*"
ASSERT_TRUTHY_CALL = r"\bassert(?:\.ok)?\(\s*"
ASSERT_EQUAL_CALL = r"\bassert\.(?:equal|strictEqual|deepEqual|deepStrictEqual)\(\s*"
RECEIVER = r"(?:[A-Za-z_$][\w$]*\.)?"


def is_asserted_absent(source, match):
    """True when a getElementById probe is a proven absence check.

    Only a call whose result is asserted missing qualifies: asserted
    null/undefined or a null comparison asserted true, negated once
    directly under an assert, or
    wrapped in Boolean(...)/!! and asserted false. A bare Boolean(...),
    ``!!`` or ``!`` is a positive or property probe
    (``!document.getElementById('x').disabled`` needs the element), so
    it stays a query.
    """
    before = source[max(0, match.start() - 160):match.start()]
    closing = re.match(r"\s*\)", source[match.end():])
    if not closing:
        return False
    rest = source[match.end() + closing.end():][:160]
    comparison = re.match(r"\s*===?\s*(?:null|undefined)\b", rest)
    if comparison:
        # The comparison's consumer determines whether absence is intended.
        # A guard that throws when it is true still requires the element.
        tail = rest[comparison.end():]
        if re.search(ASSERT_TRUTHY_CALL + rf"{RECEIVER}$", before):
            return bool(re.match(r"\s*[,)]", tail))
        if re.search(ASSERT_EQUAL_CALL + rf"{RECEIVER}$", before):
            return bool(re.match(r"\s*,\s*true\s*[,)]", tail))
        return False
    if re.search(ASSERT_CALL + rf"!\s*{RECEIVER}$", before):
        return bool(re.match(r"\s*[,)]", rest))
    if re.search(ASSERT_EQUAL_CALL + rf"{RECEIVER}$", before):
        return bool(re.match(r"\s*,\s*(?:null|undefined)\b", rest))
    if re.search(ASSERT_EQUAL_CALL + rf"!!\s*{RECEIVER}$", before):
        return bool(re.match(r"\s*,\s*false\b", rest))
    if re.search(ASSERT_EQUAL_CALL + rf"Boolean\(\s*{RECEIVER}$", before):
        return bool(re.match(r"\s*\)\s*,\s*false\b", rest))
    return False


def extract_hooks(source):
    """Return (singles, selector_groups) queried in one fixture.

    Singles are (line, kind, token). A selector group is
    (line, alternatives), each alternative a list of (kind, token):
    a comma-separated selector query succeeds when any alternative
    matches, so a group fails only when every alternative is broken.
    """
    singles = []
    groups = []
    lines = source.splitlines()

    def line_of(pos):
        return source.count("\n", 0, pos) + 1

    for match in GET_BY_ID_RE.finditer(source):
        token = match.group(1)
        if "$" in token:
            continue  # templated id, not a literal hook
        if is_asserted_absent(source, match):
            continue  # supported absence assertion; a comparison alone queries
        singles.append((line_of(match.start()), "id", token))
    for match in DATASET_RE.finditer(source):
        token = "data-" + camel_to_kebab(match.group(1))
        singles.append((line_of(match.start()), "data attribute", token))
    for match in DATASET_BRACKET_RE.finditer(source):
        name = match.group(1)
        token = name if name.startswith("data-") else "data-" + camel_to_kebab(name)
        singles.append((line_of(match.start()), "data attribute", token))
    for match in CLASSLIST_RE.finditer(source):
        singles.append((line_of(match.start()), "class", match.group(1)))

    for content, offset in literals(source):
        # Line is approximate for literals nested in a template
        # interpolation; their own offset was recorded at parse end.
        line = line_of(offset)
        for match in API_RE.finditer(content):
            raw = match.group(0)
            if match.end() < len(content) and content[match.end()] == SENTINEL:
                raw += SENTINEL
            if "*" in raw:
                continue  # route glob such as '**/api/**', not a query
            path = raw.split("?")[0].split("#")[0].rstrip(".,;:")
            if path in ("/api", "/api/"):
                continue
            singles.append((line, "api path", path))
        if DATA_RE.fullmatch(content.strip()):
            # getAttribute(...) whose result is asserted null in the
            # next lines probes an attribute that must stay absent.
            window = "\n".join(lines[line - 1:line + 3]) if 1 <= line <= len(lines) else ""
            if "getAttribute" not in window or "null" not in window:
                singles.append((line, "data attribute", content.strip()))
            continue
        if not is_selector_like(content):
            continue
        if 1 <= line <= len(lines) and NEGATIVE_COUNT_RE.search(lines[line - 1]):
            continue  # asserted-absent selector, not a query for it
        alternatives = []
        for alternative in split_selector_list(content):
            hooks = []
            # Attribute values are data, not selectors: a dot inside
            # [data-key="workflow.max_rounds"] is not a class query.
            bare = re.sub(r"\[[^\]]*\]", "[]", alternative)
            for regex, kind in ((ID_RE, "id"), (CLASS_RE, "class")):
                for match in regex.finditer(bare):
                    if match.end() < len(bare) and bare[match.end()] == SENTINEL:
                        continue
                    if match.start() > 0 and bare[match.start() - 1] == SENTINEL:
                        continue
                    hooks.append((kind, match.group(1)))
            for regex, kind in ((DATA_RE, "data attribute"),):
                for match in regex.finditer(alternative):
                    # A token abutting an interpolation is dynamic
                    # (`#audit-detail-${id}`); its static prefix is not
                    # the queried hook.
                    if match.end() < len(alternative) and alternative[match.end()] == SENTINEL:
                        continue
                    if match.start() > 0 and alternative[match.start() - 1] == SENTINEL:
                        continue
                    hooks.append((kind, match.group(1)))
            alternatives.append(hooks)
        if any(alternatives):
            groups.append((line, alternatives))
    return singles, groups


def route_patterns(root):
    """Full /api route patterns from the web router, as segment lists."""
    routes_file = root / "crates/orbit-web/src/api/routes.rs"
    patterns = []
    if routes_file.is_file():
        for match in ROUTE_RE.finditer(routes_file.read_text(encoding="utf-8", errors="ignore")):
            patterns.append(("/api" + match.group(1)).strip("/").split("/"))
    return patterns


def route_matches(path, patterns, assets_text):
    if SENTINEL not in path and path in assets_text:
        return True
    segments = path.strip("/").split("/") if path.strip("/") else []
    is_prefix = path.endswith("/") or (segments and SENTINEL in segments[-1])
    for pattern in patterns:
        if _segments_match(segments, pattern, is_prefix):
            return True
    return False


def _segments_match(segments, pattern, is_prefix):
    index = 0
    for part in pattern:
        if part.startswith("*"):  # *rest swallows the remainder, even empty
            return True
        if index >= len(segments):
            # The fixture path is a strict prefix of this route (it
            # ends in '/' or an interpolation); a prefix query such
            # as startsWith('/api/tasks/') is served by the route.
            return is_prefix
        segment = segments[index]
        if part.startswith(":") or part.startswith("{"):
            pass  # route parameter accepts any one segment
        elif SENTINEL in segment:
            pass  # templated fixture segment accepts any one segment
        elif segment != part:
            return False
        index += 1
    return index == len(segments)


IDENT = r"[A-Za-z_$][\w$]*"
NAMED_TEMPLATE_RE = re.compile(rf"\b(?:const|let|var)\s+({IDENT})\s*=\s*`")
RETURNED_TEMPLATE_RE = re.compile(
    rf"\bfunction\s+({IDENT})\s*\([^)]*\)\s*\{{\s*return\s*`")
# Where a dashboard asset defines a hook: an id assigned or passed as an
# object property, a class passed as a property or added to classList.
ID_DEFINITION_RE = re.compile(rf"\bid\s*[:=](?!=)\s*(?=`|{IDENT})")
CLASS_DEFINITION_RE = re.compile(
    r"\b(?:class|className)\s*[:=](?!=)\s*(?=`)|classList\.(?:add|toggle)\(\s*(?=`)")
MAX_TEMPLATE_DEPTH = 3


def _template_at(text, start):
    """Return ([items], end) for the template at start: characters, with
    each interpolation (SENTINEL) followed by its source text."""
    expressions = []
    content, end = _parse_template(text, start, [], expressions)
    items, slot = [], 0
    for char in content:
        if char == SENTINEL:
            items.append((SENTINEL, expressions[slot] if slot < len(expressions) else ""))
            slot += 1
        else:
            items.append(char)
    return items, end


def _item_regex(items, named, depth=0):
    """Regex source for template items, or None when no literal text
    constrains it (`${x}` alone would match every hook)."""
    parts, constrained = [], False
    for item in items:
        if isinstance(item, str):
            parts.append(re.escape(item))
            constrained = True
            continue
        resolved = _name_regex(item[1], named, depth + 1)
        if resolved:
            parts.append(resolved)
            constrained = True
        else:
            parts.append(".*")
    return "".join(parts) if constrained else None


def _name_regex(name, named, depth=0):
    """Alternation of every constrained template bound to ``name``."""
    if depth > MAX_TEMPLATE_DEPTH:
        return None
    sources = [_item_regex(items, named, depth) for items in named.get(name, ())]
    sources = [source for source in sources if source]
    return "(?:" + "|".join(sources) + ")" if sources else None


def dynamic_hook_patterns(assets):
    """Patterns for the ids and classes the assets build from templates.

    ``assets`` is a list of file texts. A dynamically built hook is
    exempt only for the kind its generating expression defines (a class
    template cannot satisfy a missing id, nor can a data-key value), and
    only for tokens the whole template can produce.
    """
    patterns = {"id": [], "class": []}
    for text in assets:
        named = {}
        for regex in (NAMED_TEMPLATE_RE, RETURNED_TEMPLATE_RE):
            for match in regex.finditer(text):
                named.setdefault(match.group(1), []).append(
                    _template_at(text, match.end() - 1)[0])
        for match in ID_DEFINITION_RE.finditer(text):
            if text[match.end()] == "`":
                source = _item_regex(_template_at(text, match.end())[0], named)
            else:
                name = re.match(IDENT, text[match.end():]).group(0)
                if text.startswith(".", match.end() + len(name)):
                    continue  # property access, not a bare template name
                source = _name_regex(name, named)
            if source:
                patterns["id"].append(re.compile(source))
        for match in CLASS_DEFINITION_RE.finditer(text):
            words, word = [], []
            for item in _template_at(text, match.end())[0]:
                if isinstance(item, str) and item.isspace():
                    words.append(word)
                    word = []
                else:
                    word.append(item)
            words.append(word)
            for word in words:
                if any(not isinstance(item, str) for item in word):
                    source = _item_regex(word, named)
                    if source:
                        patterns["class"].append(re.compile(source))
    return patterns


def check(root):
    web = root / "crates/orbit-web"
    fixtures = sorted(web.rglob("*_browser.mjs"))
    assets_dir = web / "assets/dashboard"
    asset_files = []
    if assets_dir.is_dir():
        for path in sorted(assets_dir.rglob("*")):
            if path.is_file():
                asset_files.append(path.read_text(encoding="utf-8", errors="ignore"))
    assets_text = "".join(text + "\n" for text in asset_files)
    patterns = route_patterns(root)
    dynamic = dynamic_hook_patterns(asset_files)

    def present(kind, token, defined):
        if token in defined.get(kind, ()):
            return True
        if kind == "api path":
            return route_matches(token, patterns, assets_text)
        if kind == "data attribute" and token.startswith("data-"):
            # The dashboard sets most data attributes through the
            # dataset API, where data-agent-stdout is dataset.agentStdout.
            camel = re.sub(r"-([a-z0-9])", lambda m: m.group(1).upper(), token[5:])
            if f"dataset.{camel}" in assets_text:
                return True
        return token in assets_text or any(
            regex.fullmatch(token) for regex in dynamic.get(kind, ()))

    failures = []
    for fixture in fixtures:
        seen = set()
        source = fixture.read_text(encoding="utf-8", errors="ignore")
        defined = defined_hooks(source)
        relative = fixture.relative_to(root)
        singles, groups = extract_hooks(source)
        for line, kind, token in singles:
            key = (line, kind, token)
            if key in seen:
                continue
            seen.add(key)
            if not present(kind, token, defined):
                shown = token.replace(SENTINEL, "${...}")
                failures.append(
                    f"{relative}:{line}: missing {kind} hook '{shown}' "
                    "(no occurrence in crates/orbit-web/assets/dashboard/ "
                    "or the web API routes)")
        for line, alternatives in groups:
            missing = [[(kind, token) for kind, token in alternative
                        if not present(kind, token, defined)]
                       for alternative in alternatives]
            if any(not absent for absent in missing):
                continue  # one alternative still matches end to end
            # Every alternative is broken; name the hooks of the
            # alternative closest to matching.
            for kind, token in min(missing, key=len):
                key = (line, kind, token)
                if key in seen:
                    continue
                seen.add(key)
                failures.append(
                    f"{relative}:{line}: missing {kind} hook '{token}' "
                    "(no occurrence in crates/orbit-web/assets/dashboard/ "
                    "or the web API routes)")
    return failures


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    args = parser.parse_args()
    try:
        failures = check(args.root.resolve())
    except OSError as error:
        print(f"dashboard browser fixtures: {error}", file=sys.stderr)
        return 1
    if failures:
        print("\n".join(failures), file=sys.stderr)
        return 1
    print("dashboard browser fixtures: every queried hook exists in the dashboard assets or API routes")
    return 0


if __name__ == "__main__":
    sys.exit(main())
