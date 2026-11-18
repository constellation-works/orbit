#!/usr/bin/env python3
"""Fail when a dashboard label removed in the diff is still named in docs.

From the delivery diff against the guardrails base (`CI_TEST_BASE`, with the
merge-base fallback `scripts/ci-guardrails.sh` uses), take quoted multi-word
labels on lines removed from `crates/orbit-web/assets/dashboard/**`, drop any
label the same diff re-adds, and fail when the label is still present in
current-state docs, the website or shipped skills. Legitimate keeps are
recorded in `scripts/removed-dashboard-labels-allowlist.json`. An entry is
judged only when this diff removes that same label: it is stale when the
named document no longer contains it. An earlier delivery's entry is left
alone, and a diff that removes no dashboard label does not judge the
allowlist.
"""

import json
import os
from pathlib import Path
import re
import subprocess
import sys


ROOT = Path(__file__).resolve().parent.parent
ALLOWLIST = ROOT / "scripts/removed-dashboard-labels-allowlist.json"
DASHBOARD = "crates/orbit-web/assets/dashboard"
DOC_ROOTS = (
    "docs/design/user-interface",
    "website/src/content",
    "crates/orbit-core/assets/skills",
    "plugin/skills",
)
DOC_FILES = ("docs/agents/dashboard-map.md",)
QUOTED = re.compile(r'"([^"\n]+)"|\'([^\'\n]+)\'|`([^`\n]+)`')
# Characters that mark a quoted string as code, markup or a path rather than
# user-visible label text. `/`, `-`, `&`, `:`, `.` and `,` stay allowed so
# labels such as "Open / Recently closed" still match.
CODE_CHARS = set("{}<>()[]=;\\|@#$^*_+~")
# Declaration clause that holds a quoted run in JS or HTML: a `class`, `className`
# or `*Class` property (its value, or a ternary branch of it), or a `class=`
# attribute or `className =` assignment. A class list is never copy.
CLASS_SINK = re.compile(
    r"\b\w*?(?:[Cc]lass(?:es|Name|List)?|cls)\b[\"']?\s*(?::(?!:)|[+]?=(?!=))")
CLASS_CALL = re.compile(r"\bclassList\.\w+\(\s*$")
# An HTML `class="…"` attribute: the run follows `class=` with no closing quote
# or tag boundary in between, so `class="a b" aria-label="c d"` leaves `c d` as copy.
HTML_CLASS_ATTR = re.compile(r"\bclass\s*=\s*[\"']?[^\"'<>]*$")
# CSS strings are visible copy only as `content:` values; grid-template-areas
# rows and font or attribute selectors name layout, not labels.
CSS_CONTENT = re.compile(r"\bcontent\s*:")
PROPERTY_BEFORE = re.compile(r"[{,]\s*$")
PROPERTY_AFTER = re.compile(r"\s*:(?!:)")
IDENTIFIER_WORD = re.compile(r"[a-z0-9_-]+")


def git(*arguments):
    return subprocess.run(
        ["git", "-C", str(ROOT), *arguments],
        env=dict(os.environ, GIT_OPTIONAL_LOCKS="0"),
        capture_output=True, text=True)


def comparison_base():
    explicit = os.environ.get("CI_TEST_BASE")
    if explicit:
        result = git("rev-parse", "--verify", f"{explicit}^{{commit}}")
        if result.returncode == 0:
            return result.stdout.strip()
        raise ValueError(f"cannot resolve CI_TEST_BASE {explicit!r} to a commit")
    for branch in ("refs/remotes/origin/agent-main", "refs/heads/agent-main"):
        if git("rev-parse", "--verify", "--quiet", branch).returncode == 0:
            result = git("merge-base", "HEAD", branch)
            if result.returncode == 0 and result.stdout.strip():
                return result.stdout.strip()
            break
    raise ValueError("cannot resolve agent-main; set CI_TEST_BASE to the delivery base commit")


def top_level_clause(text):
    # Text since the nearest `,` `;` or opening bracket outside any nested brackets.
    depth = 0
    for index in range(len(text) - 1, -1, -1):
        char = text[index]
        if char in ")]}":
            depth += 1
        elif char in "([{":
            if depth == 0:
                return text[index + 1:]
            depth -= 1
        elif char in ",;" and depth == 0:
            return text[index + 1:]
    return text


def is_class_or_property(line, match, words, suffix):
    # Decided by where the run sits, not by its capitalization: lowercase copy
    # such as `text: "not probed"` is a visible label.
    before = line[:match.start()]
    if suffix == ".css":
        return CSS_CONTENT.search(top_level_clause(before)) is None
    if HTML_CLASS_ATTR.search(before):
        return True
    if suffix == ".html":
        return False
    if CLASS_SINK.search(top_level_clause(before)) or CLASS_CALL.search(before):
        return True
    # A lowercase identifier run used as an object key is a property name.
    return (PROPERTY_BEFORE.search(before) is not None
            and PROPERTY_AFTER.match(line, match.end()) is not None
            and all(IDENTIFIER_WORD.fullmatch(word) for word in words))


def labels_in(line, path=""):
    suffix = os.path.splitext(path)[1]
    labels = []
    for match in QUOTED.finditer(line):
        value = next(group for group in match.groups() if group is not None)
        label = " ".join(value.split())
        words = label.split()
        if len(words) < 2 or not any(char.isalpha() for char in label):
            continue
        if CODE_CHARS.intersection(label) or "${" in label or "://" in label:
            continue
        if is_class_or_property(line, match, words, suffix):
            continue
        labels.append(label)
    return labels


def removed_and_added_labels(base):
    result = git("diff", "--no-renames", "--unified=0", base, "--", DASHBOARD)
    if result.returncode != 0:
        raise ValueError(f"git diff against {base} failed: {result.stderr.strip()}")
    removed = {}
    added = set()
    current_file = ""
    for line in result.stdout.splitlines():
        if line.startswith("--- a/"):
            current_file = line[6:]
        elif line.startswith("+++ b/"):
            current_file = line[6:]
        elif line.startswith("-") and not line.startswith("---"):
            for label in labels_in(line[1:], current_file):
                removed.setdefault(label, set()).add(current_file)
        elif line.startswith("+") and not line.startswith("+++"):
            added.update(labels_in(line[1:], current_file))
    return {label: files for label, files in removed.items() if label not in added}


def filter_still_present(removed):
    # A label removed from one dashboard file but still present elsewhere in
    # the worktree dashboard is not a removal: docs may name the remaining
    # instance. Check raw substrings, mirroring doc_hits.
    if not removed:
        return removed
    root = ROOT / DASHBOARD
    if not root.is_dir():
        return removed
    texts = []
    for path in sorted(root.rglob("*")):
        if not path.is_file():
            continue
        try:
            texts.append(path.read_text(errors="replace"))
        except OSError:
            continue
    return {label: files for label, files in removed.items()
            if not any(label in text for text in texts)}


def doc_paths():
    paths = []
    for relative in DOC_ROOTS:
        root = ROOT / relative
        if root.is_dir():
            paths.extend(path for path in sorted(root.rglob("*")) if path.is_file())
    for relative in DOC_FILES:
        path = ROOT / relative
        if path.is_file():
            paths.append(path)
    return paths


def doc_hits(labels):
    hits = {label: [] for label in labels}
    for path in doc_paths():
        relative = path.relative_to(ROOT).as_posix()
        try:
            lines = path.read_text(errors="replace").splitlines()
        except OSError:
            continue
        for number, line in enumerate(lines, 1):
            for label in labels:
                if label in line:
                    hits[label].append((relative, number))
    return hits


def normalize_label(value):
    label = " ".join(str(value).split())
    if len(label) >= 2 and label[0] == label[-1] and label[0] in "\"'`":
        label = " ".join(label[1:-1].split())
    return label


def load_allowlist():
    entries = json.loads(ALLOWLIST.read_text())
    if not isinstance(entries, list):
        raise ValueError("allowlist must be a JSON list")
    allowlist = {}
    for entry in entries:
        if not isinstance(entry, dict):
            raise ValueError("each allowlist entry must be an object")
        label = normalize_label(entry["label"])
        doc = str(entry["doc"])
        if not str(entry["reason"]).strip():
            raise ValueError(f"allowlist entry for {label!r} in {doc} needs a reason")
        if (label, doc) in allowlist:
            raise ValueError(f"duplicate allowlist entry for {label!r} in {doc}")
        allowlist[(label, doc)] = entry["reason"]
    return allowlist


def main():
    try:
        allowlist = load_allowlist()
        base = comparison_base()
        removed = removed_and_added_labels(base)
        removed = filter_still_present(removed)
        if not removed:
            print("removed dashboard labels: no removed dashboard labels in the diff")
            return 0
        hits = doc_hits(removed)
        errors = []
        for label in sorted(removed):
            files = ", ".join(sorted(removed[label]))
            for doc, number in hits[label]:
                if (label, doc) not in allowlist:
                    errors.append(
                        f'removed dashboard label "{label}" (removed in {files}) '
                        f"still present in {doc}:{number}")
        # An earlier delivery's entry stays until this diff removes that same
        # label. It is stale only when this diff removes the label and the
        # named document no longer contains it. A diff that removes no
        # dashboard label returns above and does not judge the allowlist.
        hit_pairs = {(label, doc) for label, found in hits.items() for doc, _ in found}
        for label, doc in sorted(allowlist):
            if label not in removed:
                continue
            if (label, doc) not in hit_pairs:
                errors.append(
                    f'stale allowlist entry: "{label}" in {doc} no longer matches '
                    "a removed label and a doc line")
        if errors:
            print("\n".join(errors), file=sys.stderr)
            return 1
        print(f"removed dashboard labels: no stale references to {len(removed)} removed label(s)")
        return 0
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"removed dashboard labels: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
