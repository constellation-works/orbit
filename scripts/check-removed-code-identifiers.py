#!/usr/bin/env python3
"""Fail when a code identifier removed in the diff is still named in live docs.

From the delivery diff against the guardrails base (the merge-base of HEAD and
`CI_TEST_BASE`, so a base that moved on after the branch point adds nothing to
the diff, with the agent-main merge-base fallback `scripts/ci-guardrails.sh`
uses), take the identifiers whose definition the diff deletes:

- Rust `const`, `static`, `fn`, `struct`, `enum`, `trait` and `type` names, outside
  comments and quoted literals;
- Rust `pub use` re-exports, named by their crate-qualified public path
  (`orbit_policy::resolve_symlinks` for `pub use ...::resolve_symlinks;` in
  `crates/orbit-policy/src/lib.rs`). A `pub use` inside a comment is not a
  re-export. A re-export in a file outside `crates/<crate>/src` has no public
  path here and is not judged;
- CSS class selectors, named with their dot (`.ws-badge`), including a class in
  a compound or element-qualified selector (`.active.retired`, `a.retired`).
  Text in a CSS comment, a quoted string, a `url(...)` or a declaration value
  names no class.

Drop any identifier the same diff adds back, or that the working tree still
defines, re-exports or styles. Fail when a removed identifier is named as a
token inside an inline code span of a live document. Live documents are `docs/`
(except `docs/design/*/4_decisions.md`, `docs/agents/records/rca/` and `docs/standards/`),
`website/src/content`, the shipped skills, `ARCHITECTURE.md` and the per-crate
`AGENTS.md` and `CLAUDE.md` files. Legitimate keeps are recorded in
`scripts/removed-code-identifiers-allowlist.json` with a reason. An entry is
judged only when this diff removes that identifier: it is stale when the named
document no longer names it.

A checkout that holds no merge-base with `CI_TEST_BASE` (a shallow clone without
the common ancestor) fails with an insufficient-history report before any removal
is scanned, rather than comparing from the base tip.
"""

import json
import os
from pathlib import Path
import re
import subprocess
import sys


ROOT = Path(__file__).resolve().parent.parent
ALLOWLIST = ROOT / "scripts/removed-code-identifiers-allowlist.json"
SOURCE_SUFFIXES = (".rs", ".css")
DOC_SUFFIXES = (".md", ".mdx")
DOC_PREFIXES = ("docs/", "website/src/content/", "crates/orbit-core/assets/skills/",
                "plugin/skills/")
DOC_FILES = ("ARCHITECTURE.md",)
CRATE_GUIDE = re.compile(r"crates/[^/]+/(?:AGENTS|CLAUDE)\.md")
# Decision records, RCAs and vendored standards keep history on purpose.
HISTORICAL_DOCS = re.compile(r"docs/design/[^/]+/4_decisions\.md|docs/agents/records/rca/|docs/standards/")
CRATE_SOURCE = re.compile(r"crates/([^/]+)/src/(.+)\.rs")
# A definition starts its line, after visibility and qualifier keywords, so
# `*const T`, `&'static str` and `fn(u8)` pointer types do not count.
RUST_ITEM = re.compile(
    r"^[ \t]*(?:pub(?:\([^)\n]*\))?[ \t]+)?"
    r"(?:(?:const|async|unsafe|extern)(?:[ \t]+\"[^\"\n]*\")?[ \t]+)*"
    r"(?:const|static|fn|struct|enum|trait|type)[ \t]+(?:mut[ \t]+)?([A-Za-z_]\w*)",
    re.MULTILINE)
PUB_USE = re.compile(r"\bpub(?:\([^)\n]*\))?[ \t]+use[ \t]+([^;]+);")
USE_PATH_SEPARATOR = re.compile(r"\s*::\s*")
RUST_RAW_STRING = re.compile(r'(?:br|r)(#*)"')
# A character literal is one character, a backslash escape, or a `\x7F` or
# `\u{2f}` escape; a quoted string runs to its closing quote.
RUST_QUOTED = re.compile(
    r'''"(?:\\.|[^"\\])*"|'(?:\\u\{[0-9A-Fa-f_]+\}|\\x[0-9A-Fa-f]{2}|\\.|[^'\\\n])' ''',
    re.DOTALL | re.VERBOSE)
# CSS comments, quoted strings and `url(...)` tokens, which name no class even
# in a selector (`a[href$=".pdf"]`), and selector preludes: the text before
# each `{` since the last `;`, `{` or `}`. Declarations are never preludes.
CSS_OPAQUE = re.compile(
    r"""/\*.*?\*/|"(?:\\.|[^"\\\n])*"|'(?:\\.|[^'\\\n])*'|\burl\([^)\n]*\)""", re.DOTALL)
CSS_PRELUDE = re.compile(r"[^{};]*\{")
# A class in a prelude, including one after another selector part (`a.retired`,
# `.active.retired`). A letter must follow the dot, so `1.5` is not a class.
CSS_CLASS = re.compile(r"\.(-?[A-Za-z_][\w-]*)")
CODE_SPAN = re.compile(r"(?<!`)(?P<ticks>`+)(?!`)(?P<code>.*?)(?<!`)(?P=ticks)(?!`)",
                       re.DOTALL)
FENCE = re.compile(r"[ ]{0,3}(?P<marker>`{3,}|~{3,})(?P<info>.*)")
QUOTE = re.compile(r"[ ]{0,3}>[ ]?")
LIST_ITEM = re.compile(r"([ ]{0,3})(?:[-+*]|[0-9]{1,9}[.)])([ ]{1,4})")


def git(*arguments):
    return subprocess.run(
        ["git", "-C", str(ROOT), "-c", "core.quotepath=off", *arguments],
        env=dict(os.environ, GIT_OPTIONAL_LOCKS="0"),
        capture_output=True, text=True)


def comparison_base():
    explicit = os.environ.get("CI_TEST_BASE")
    if explicit:
        result = git("rev-parse", "--verify", f"{explicit}^{{commit}}")
        if result.returncode != 0:
            raise ValueError(f"cannot resolve CI_TEST_BASE {explicit!r} to a commit")
        # A pull request passes its base branch tip, which may have moved past
        # the head's branch point. Diffing from that tip would read every
        # identifier the base added since as one this diff removes, so compare
        # from the merge-base. A push passes the previous tip, an ancestor of
        # HEAD, which is its own merge-base, so push runs compare as before.
        tip = result.stdout.strip()
        fork = git("merge-base", tip, "HEAD")
        if fork.returncode == 0 and fork.stdout.strip():
            return fork.stdout.strip()
        # A shallow checkout can hold both tips without their common ancestor.
        # The tip would then count every base addition since the branch point as
        # a removal, so stop before scanning instead of comparing from it.
        raise ValueError(
            f"insufficient history: no common ancestor of CI_TEST_BASE {explicit!r} and HEAD "
            "in this checkout; fetch enough history to reach it (git fetch --unshallow "
            "in a shallow clone)")
    for branch in ("refs/remotes/origin/agent-main", "refs/heads/agent-main"):
        if git("rev-parse", "--verify", "--quiet", branch).returncode == 0:
            result = git("merge-base", "HEAD", branch)
            if result.returncode == 0 and result.stdout.strip():
                return result.stdout.strip()
            break
    raise ValueError("cannot resolve agent-main; set CI_TEST_BASE to the delivery base commit")


def changed_paths(base):
    """Return the paths the diff touches against the base, including deleted ones."""
    result = git("diff", "--name-only", "--no-renames", "-z", base, "--")
    if result.returncode != 0:
        raise ValueError(f"git diff against {base} failed: {result.stderr.strip()}")
    return [name for name in result.stdout.split("\0") if name]


def module_of(path):
    """Crate-qualified module path of a crate source file, or None elsewhere."""
    match = CRATE_SOURCE.fullmatch(path)
    if match is None:
        return None
    segments = match.group(2).split("/")
    if segments[-1] == "mod" or (len(segments) == 1 and segments[0] in ("lib", "main")):
        segments.pop()
    return "::".join([match.group(1).replace("-", "_"), *segments])


def top_level_commas(text):
    parts, depth, start = [], 0, 0
    for index, char in enumerate(text):
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
        elif char == "," and depth == 0:
            parts.append(text[start:index])
            start = index + 1
    parts.append(text[start:])
    return [part for part in parts if part.strip()]


def use_items(tree, prefix=""):
    """Return (path, public name) for each item of a `use` tree; globs are skipped."""
    tree = USE_PATH_SEPARATOR.sub("::", " ".join(tree.split()))
    brace = tree.find("{")
    if brace >= 0:
        head = prefix + tree[:brace]
        items = []
        for part in top_level_commas(tree[brace + 1:tree.rfind("}")]):
            items.extend(use_items(part, head))
        return items
    target, _, alias = tree.partition(" as ")
    target = target.strip()
    if not target or target.endswith("*"):
        return []
    if target == "self":
        full = prefix[:-2] if prefix.endswith("::") else prefix
    else:
        full = prefix + target
    name = alias.strip() or full.rsplit("::", 1)[-1]
    if not full or not name or name == "_":
        return []
    return [(full, name)]


def rust_code_without_comments_or_literals(text):
    """Replace Rust comments and literals with whitespace, preserving line breaks."""
    result = []
    index = 0
    while index < len(text):
        raw = RUST_RAW_STRING.match(text, index) if text[index] in 'br' else None
        if raw:
            end_marker = '"' + raw.group(1)
            end = text.find(end_marker, raw.end())
            end = len(text) if end < 0 else end + len(end_marker)
            result.append(''.join('\n' if char == '\n' else ' ' for char in text[index:end]))
            index = end
            continue
        elif text[index] in "\"'" and (quoted := RUST_QUOTED.match(text, index)):
            end = quoted.end()
            result.append(''.join('\n' if char == '\n' else ' ' for char in text[index:end]))
            index = end
            continue
        elif text.startswith('//', index):
            end = text.find('\n', index)
            end = len(text) if end < 0 else end
            result.append(' ')
            index = end
            continue
        elif text.startswith('/*', index):
            end, depth = index + 2, 1
            while end < len(text) and depth:
                if text.startswith('/*', end):
                    depth += 1
                    end += 2
                elif text.startswith('*/', end):
                    depth -= 1
                    end += 2
                else:
                    end += 1
            result.append(' ')
            index = end
            continue
        else:
            end = index + 1
        result.append(text[index:end])
        index = end
    return ''.join(result)


def public_paths(path, code):
    """Crate-qualified public paths a Rust source re-exports, outside comments and literals."""
    module = module_of(path)
    if module is None:
        return set()
    return {f"{module}::{name}"
            for match in PUB_USE.finditer(code)
            for _, name in use_items(match.group(1))}


def css_classes(text):
    """CSS class selectors a stylesheet names, with their dot."""
    text = CSS_OPAQUE.sub(" ", text)
    return {"." + match.group(1)
            for prelude in CSS_PRELUDE.finditer(text)
            for match in CSS_CLASS.finditer(prelude.group())}


def rust_items(code):
    """Names of the Rust items a source defines, from its code without comments or literals."""
    names = {match.group(1) for match in RUST_ITEM.finditer(code)}
    names.discard("_")
    return names


def names_in(path, text):
    """Identifiers a source text defines or re-exports, or the CSS classes it styles.

    A Rust item or re-export spelled in a comment or a quoted literal names nothing."""
    if path.endswith(".css"):
        return css_classes(text)
    code = rust_code_without_comments_or_literals(text)
    return rust_items(code) | public_paths(path, code)


def read_text(relative):
    try:
        return (ROOT / relative).read_text(errors="replace")
    except OSError:
        return None


def worktree_files(*options):
    result = git("ls-files", *options, "--exclude-standard", "-z")
    if result.returncode != 0:
        raise ValueError(f"git ls-files failed: {result.stderr.strip()}")
    return [name for name in result.stdout.split("\0") if name]


def changed_identifiers(base):
    """Return {identifier: source paths} for identifiers the diff removes from the base.

    A hunk alone cannot tell live code from a comment: it can start or end inside a
    block comment, and a zero-context hunk can leave a `pub use` opening outside its
    text. So each changed Rust or CSS file is compared whole, before and after the diff."""
    removed = {}
    for path in changed_paths(base):
        if not path.endswith(SOURCE_SUFFIXES):
            continue
        old = git("show", f"{base}:{path}")
        old_names = names_in(path, old.stdout) if old.returncode == 0 else set()
        for name in old_names - names_in(path, read_text(path) or ""):
            removed.setdefault(name, set()).add(path)
    return removed


def tree_names():
    """Identifiers the working tree still defines, re-exports or styles."""
    names = set()
    for path in worktree_files("--cached", "--others"):
        text = read_text(path) if path.endswith(SOURCE_SUFFIXES) else None
        if text is not None:
            names |= names_in(path, text)
    return names


def live_docs():
    docs = []
    for path in worktree_files("--cached", "--others"):
        if not path.endswith(DOC_SUFFIXES) or HISTORICAL_DOCS.match(path):
            continue
        if path in DOC_FILES or CRATE_GUIDE.fullmatch(path) or path.startswith(DOC_PREFIXES):
            docs.append(path)
    return docs


def name_pattern(name):
    """Whole-token match for a Rust name; a CSS class also matches as a compound's tail."""
    before = "" if name.startswith(".") else r"(?<![\w-])"
    return re.compile(before + re.escape(name) + r"(?![\w-])")


def doc_hits(identifiers, docs):
    """Return {identifier: {(document, line)}} for inline code spans that name it."""
    patterns = {name: name_pattern(name) for name in identifiers}
    hits = {name: set() for name in identifiers}
    for doc in docs:
        text = read_text(doc)
        if text is None:
            continue
        fence = None
        list_indents = []
        paragraph = []

        def scan_paragraph():
            if not paragraph:
                return
            content = "\n".join(line for _, line in paragraph)
            for span in CODE_SPAN.finditer(content):
                for name, pattern in patterns.items():
                    for match in pattern.finditer(span["code"]):
                        offset = span.start("code") + match.start()
                        number = paragraph[content.count("\n", 0, offset)][0]
                        hits[name].add((doc, number))
            paragraph.clear()

        for number, line in enumerate(text.splitlines(), 1):
            # Strip blockquote containers, then list indentation. A fence may
            # start on the list marker's line or a later continuation line.
            rest = line
            quote_depth = 0
            while (quote := QUOTE.match(rest)) is not None:
                rest = rest[quote.end():]
                quote_depth += 1
            indent = len(rest) - len(rest.lstrip(" "))
            while list_indents and (quote_depth != list_indents[-1][0]
                                    or (rest.strip() and indent < list_indents[-1][1])):
                list_indents.pop()
            if list_indents:
                rest = rest[list_indents[-1][1]:]
            while fence is None and (item := LIST_ITEM.match(rest)) is not None:
                consumed = item.end()
                indent = (list_indents[-1][1] if list_indents else 0) + consumed
                list_indents.append((quote_depth, indent))
                rest = rest[consumed:]
            opening = FENCE.match(rest)
            if fence is not None:
                if quote_depth < fence[2] or (fence[3] and
                        (not list_indents or list_indents[-1][1] < fence[3])):
                    fence = None
                else:
                    if (opening is not None and opening["marker"][0] == fence[0]
                            and len(opening["marker"]) >= fence[1] and not opening["info"].strip()):
                        fence = None
                    continue
            if opening is not None:
                scan_paragraph()
                fence = (opening["marker"][0], len(opening["marker"]), quote_depth,
                         list_indents[-1][1] if list_indents else 0)
                continue
            if not rest.strip():
                scan_paragraph()
            else:
                paragraph.append((number, rest))
        scan_paragraph()
    return hits


def normalize_identifier(value):
    return str(value).strip().strip("`").strip()


def load_allowlist():
    entries = json.loads(ALLOWLIST.read_text())
    if not isinstance(entries, list):
        raise ValueError("allowlist must be a JSON list")
    allowlist = {}
    for entry in entries:
        if not isinstance(entry, dict):
            raise ValueError("each allowlist entry must be an object")
        name = normalize_identifier(entry["identifier"])
        doc = str(entry["doc"])
        if not str(entry["reason"]).strip():
            raise ValueError(f"allowlist entry for {name!r} in {doc} needs a reason")
        if (name, doc) in allowlist:
            raise ValueError(f"duplicate allowlist entry for {name!r} in {doc}")
        allowlist[(name, doc)] = entry["reason"]
    return allowlist


def main():
    try:
        allowlist = load_allowlist()
        base = comparison_base()
        removed = changed_identifiers(base)
        if removed:
            defined = tree_names()
            removed = {name: files for name, files in removed.items() if name not in defined}
        if not removed:
            print("removed code identifiers: no removed code identifiers in the diff")
            return 0
        hits = doc_hits(sorted(removed), live_docs())
        errors = []
        for name in sorted(removed):
            files = ", ".join(sorted(removed[name]))
            for doc, number in sorted(hits[name]):
                if (name, doc) not in allowlist:
                    errors.append(
                        f'removed code identifier "{name}" (removed in {files}) '
                        f"still named in {doc}:{number}")
        # An entry is judged only when this diff removes its identifier. It is
        # stale when the named document no longer names the identifier.
        hit_pairs = {(name, doc) for name, found in hits.items() for doc, _ in found}
        for name, doc in sorted(allowlist):
            if name in removed and (name, doc) not in hit_pairs:
                errors.append(
                    f'stale allowlist entry: "{name}" in {doc} no longer matches '
                    "a removed code identifier and a doc line")
        if errors:
            print("\n".join(errors), file=sys.stderr)
            return 1
        print(f"removed code identifiers: no stale references to {len(removed)} removed identifier(s)")
        return 0
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"removed code identifiers: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
