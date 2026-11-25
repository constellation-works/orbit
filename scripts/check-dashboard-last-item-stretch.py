#!/usr/bin/env python3
"""Keep Task detail's stacked content at its natural height.

The comment thread and the other vertical lists in Task detail must not grow
their final item to fill spare height. The grid stacks must also keep their
auto rows at the start of the available space. Full-height scroll areas,
editors and plots are intentionally outside this check.
"""

import re
import sys
from dataclasses import dataclass
from pathlib import Path

DEFAULT_ROOT = Path(__file__).resolve().parent.parent
TASK_DETAIL = "crates/orbit-web/assets/dashboard/css/task-detail.css"
COMPONENTS = "crates/orbit-web/assets/dashboard/css/components.css"


@dataclass(frozen=True)
class FlexStack:
    stylesheet: str
    parent: str
    child: str
    child_aliases: tuple[str, ...] = ()
    child_tags: tuple[str, ...] = ()


FLEX_STACKS = (
    FlexStack(TASK_DETAIL, ".comments-panel .comment-thread", ".comment-card", (".card",)),
    FlexStack(TASK_DETAIL, ".row-detail .ac-list", "li"),
    FlexStack(TASK_DETAIL, ".row-detail .file-list", "li"),
    FlexStack(TASK_DETAIL, ".task-run-list", ".task-run"),
    FlexStack(TASK_DETAIL, ".readiness-list", ".readiness-gap"),
    FlexStack(TASK_DETAIL, ".history-kv-list", "*", (".history-kv-item",), ("div",)),
    FlexStack(TASK_DETAIL, ".comment-outline", ".comment-outline-link"),
)
GRID_STACKS = (
    (TASK_DETAIL, ".artifacts-list"),
    (TASK_DETAIL, ".decision-answered-list"),
    (TASK_DETAIL, ".decision-answers"),
    (TASK_DETAIL, ".decision-fields"),
    (TASK_DETAIL, ".decision-requests"),
    (TASK_DETAIL, ".human-task-steps"),
    (TASK_DETAIL, ".owed-criteria"),
    (TASK_DETAIL, ".why-stuck-rows"),
    (TASK_DETAIL, ".why-stuck-checks"),
)

RULE = re.compile(r"([^{}]+)\{([^{}]*)\}", re.DOTALL)
CLASS_TOKEN = re.compile(r"\.((?:[^\W\d]|-)[\w-]*)(?![\w-])")
NUMBER = re.compile(r"^[+-]?(?:\d+(?:\.\d*)?|\.\d+)$")


def mask_comments_and_strings(text):
    """Blank comments and strings while preserving newlines and offsets."""
    chars = list(text)
    index = 0
    quote = None
    while index < len(text):
        if quote:
            if text[index] == "\\":
                chars[index] = " "
                if index + 1 < len(text):
                    chars[index + 1] = "\n" if text[index + 1] == "\n" else " "
                    index += 2
                    continue
            elif text[index] == quote:
                chars[index] = " "
                quote = None
            else:
                chars[index] = "\n" if text[index] == "\n" else " "
            index += 1
        elif text[index] in ("'", '"'):
            quote = text[index]
            chars[index] = " "
            index += 1
        elif text.startswith("/*", index):
            end = text.find("*/", index + 2)
            end = len(text) if end < 0 else end + 2
            for offset in range(index, end):
                chars[offset] = "\n" if text[offset] == "\n" else " "
            index = end
        else:
            index += 1
    return "".join(chars)


def declarations(body):
    found = {}
    for entry in body.split(";"):
        if ":" not in entry:
            continue
        name, value = entry.split(":", 1)
        found[name.strip().lower()] = re.sub(r"\s*!important\s*$", "", value.strip().lower())
    return found


def rules(path):
    text = mask_comments_and_strings(path.read_text(encoding="utf-8"))
    for match in RULE.finditer(text):
        selector = match.group(1).strip()
        if selector.startswith("@"):
            continue
        yield selector, declarations(match.group(2))


def selector_parts(selector):
    parts = []
    start = 0
    depth = 0
    for index, char in enumerate(selector):
        if char in "([":
            depth += 1
        elif char in ")]":
            depth = max(0, depth - 1)
        elif char == "," and depth == 0:
            parts.append(selector[start:index].strip())
            start = index + 1
    parts.append(selector[start:].strip())
    return parts


def class_names(selector):
    return {match.group(1) for match in CLASS_TOKEN.finditer(selector)}


def strip_not_blocks(selector):
    """Remove :not(...) blocks so excluded classes read as exclusions."""
    out = []
    index = 0
    total = len(selector)
    while index < total:
        if selector[index] == ":" and selector[index:index + 5].lower() == ":not(":
            depth = 1
            index += 5
            while index < total and depth > 0:
                if selector[index] == "(":
                    depth += 1
                elif selector[index] == ")":
                    depth -= 1
                index += 1
            out.append(" ")
        else:
            out.append(selector[index])
            index += 1
    return "".join(out)


def positive_class_names(selector):
    return class_names(strip_not_blocks(selector))


def positive_compound_class_names(compound):
    """Read positive class selectors from a compound, including class chains."""
    return positive_class_names(compound)


def compound_covers_all_items(compound, stack):
    """Whether a direct-child selector covers every item in a stack."""
    if stack.child != "*":
        return True
    item_classes = {name.removeprefix(".") for name in stack.child_aliases}
    names = positive_compound_class_names(compound)
    if not names <= item_classes:
        return False
    if re.fullmatch(r"(?:\*|[A-Za-z][\w-]*)?(?:\.[\w-]+)*", compound) is None:
        return False
    element = re.match(r"[A-Za-z][\w-]*", compound)
    return element is None or element.group().lower() in stack.child_tags


def parent_pattern(parent):
    return re.compile(r"(?<![\w-])" + r"\s+".join(re.escape(part) for part in parent.split()))


def compound_matches_item(compound, stack):
    if "::" in compound:
        return False
    if stack.child == "*":
        # Callers pass only direct children of the stack. A `*` stack's items
        # are the child aliases (for example `.history-kv-item` on a div), so a
        # compound styles them only when its element and classes can match an
        # item. `*`, `*:last-child`, `.history-kv-item`, and
        # `div.history-kv-item` qualify; `em` or `.history-kv-note` do not.
        item_classes = {name.removeprefix(".") for name in stack.child_aliases}
        names = positive_compound_class_names(compound)
        if names and not (names & item_classes):
            return False
        if not names:
            element = re.match(r"[A-Za-z][\w-]*", compound)
            if element is None and compound != "*" and not compound.startswith((":", "*:")):
                return False
            if element is not None and element.group().lower() not in stack.child_tags:
                return False
        element = re.match(r"[A-Za-z][\w-]*", compound)
        return element is None or element.group().lower() in stack.child_tags
    if stack.child == "li":
        return compound == "*" or compound.startswith("li") or ":last-child" in compound
    child_class = stack.child.removeprefix(".")
    names = positive_class_names(compound)
    aliases = {child_class, *(name.removeprefix(".") for name in stack.child_aliases)}
    return compound == "*" or bool(names & aliases) or ":last-child" in compound


def direct_child(selector, stack, *, all_items=False):
    match = parent_pattern(stack.parent).search(selector)
    if match is None:
        return False
    tail = selector[match.end():]
    prefix = re.match(r"\s*>\s*", tail)
    if prefix is None:
        return False
    pos = prefix.end()
    depth = 0
    end = pos
    while end < len(tail):
        char = tail[end]
        if char in "([":
            depth += 1
        elif char in ")]":
            depth = max(0, depth - 1)
        elif depth == 0 and (char.isspace() or char in ">+~"):
            break
        end += 1
    compound = tail[pos:end].strip()
    if not compound:
        return False
    rest = tail[end:]
    if rest.strip() == "":
        if all_items:
            return compound_covers_all_items(compound, stack)
        return compound_matches_item(compound, stack)
    if all_items:
        return False
    # The selector continues past the direct child. Only a pure sibling
    # chain (+/~) keeps the styled target as a direct child of the stack;
    # a descendant (space) or nested child (>) styles something inside the
    # item instead.
    stripped = rest.strip()
    nested_depth = 0
    for char in stripped:
        if char in "([":
            nested_depth += 1
        elif char in ")]":
            nested_depth = max(0, nested_depth - 1)
        elif nested_depth == 0 and char == ">":
            return False
    if not stripped.startswith(("+", "~")):
        return False
    parts = []
    current: list[str] = []
    depth = 0
    for char in stripped:
        if char in "([":
            depth += 1
            current.append(char)
        elif char in ")]":
            depth = max(0, depth - 1)
            current.append(char)
        elif depth == 0 and char in "+~":
            parts.append("".join(current))
            current = []
        else:
            current.append(char)
    parts.append("".join(current))
    for part in parts:
        inner = part.strip()
        if inner == "":
            continue
        nested = 0
        for char in inner:
            if char in "([":
                nested += 1
            elif char in ")]":
                nested = max(0, nested - 1)
            elif nested == 0 and char.isspace():
                return False
    sibling_target = parts[-1].strip()
    if not sibling_target:
        return False
    return compound_matches_item(sibling_target, stack)


def last_compound(selector):
    start = 0
    depth = 0
    for index, char in enumerate(selector):
        if char in "([":
            depth += 1
        elif char in ")]":
            depth = max(0, depth - 1)
        elif depth == 0 and (char.isspace() or char in ">+~"):
            start = index + 1
    return selector[start:].strip()


def matches_item(selector, stack):
    if direct_child(selector, stack):
        return True
    compound = last_compound(selector)
    aliases = {stack.child.removeprefix("."), *(name.removeprefix(".") for name in stack.child_aliases)}
    if stack.child == "li":
        return re.match(r"li(?![\w-])", compound) is not None and "::" not in compound
    if stack.child == "*":
        # A `*` stack has no class of its own, so only its declared item
        # aliases identify an item reached through a descendant combinator.
        # Classes inside :not() are exclusions, not item references.
        return bool(positive_class_names(compound) & aliases) and "::" not in compound
    return bool(positive_class_names(compound) & aliases)


def matches_container(selector, stack):
    match = parent_pattern(stack.parent).search(selector)
    if match is None:
        return False
    tail = selector[match.end():]
    if re.match(r"\s*>", tail):
        return False
    return re.fullmatch(r"(?:[.#][\w-]+|\[[^\]]+\]|:[\w-]+(?:\([^)]*\))?)*", tail) is not None


def grow_value(value):
    token = value.split()[0].lower() if value.split() else ""
    if token == "none" or token == "initial":
        return 0.0
    if token == "auto":
        return 1.0
    if NUMBER.fullmatch(token):
        return float(token)
    return None


def check_flex_stacks(root):
    errors = []
    cache = {}
    for stack in FLEX_STACKS:
        path = root / stack.stylesheet
        if not path.is_file():
            errors.append(f"missing stylesheet: {stack.stylesheet}")
            continue
        cache.setdefault(stack.stylesheet, list(rules(path)))
        item_has_zero_grow = False
        for selector_group, props in cache[stack.stylesheet]:
            for selector in selector_parts(selector_group):
                item_match = matches_item(selector, stack)
                container_match = matches_container(selector, stack)
                if not item_match and not container_match:
                    continue
                location = f"{stack.stylesheet}: {selector}"
                for prop in ("flex", "flex-grow"):
                    if prop not in props:
                        continue
                    grow = grow_value(props[prop]) if prop == "flex" else (
                        float(props[prop]) if NUMBER.fullmatch(props[prop]) else None
                    )
                    if grow is None:
                        errors.append(f"{location} has an uncheckable {prop}: {props[prop]}")
                    elif grow > 0:
                        errors.append(f"{location} lets a stacked item or list grow with {prop}: {props[prop]}")
                    elif item_match:
                        if stack.child == "*":
                            item_has_zero_grow = item_has_zero_grow or direct_child(
                                selector, stack, all_items=True
                            )
                        else:
                            item_has_zero_grow = True
                if item_match and "height" in props and props["height"] not in {
                    "auto", "fit-content", "max-content", "min-content", "inherit", "initial", "unset", "revert", "revert-layer"
                }:
                    errors.append(f"{location} gives a stacked item a fixed height: {props['height']}")
        if not item_has_zero_grow:
            errors.append(f"{stack.stylesheet}: {stack.parent} items need an explicit flex:none or flex-grow:0 rule")
    return errors


def check_shared_card_style(root):
    path = root / COMPONENTS
    if not path.is_file():
        return [f"missing stylesheet: {COMPONENTS}"]
    errors = []
    for selector_group, props in rules(path):
        for selector in selector_parts(selector_group):
            if "card" not in class_names(selector):
                continue
            location = f"{COMPONENTS}: {selector}"
            if "flex" in props:
                grow = grow_value(props["flex"])
                if grow is None:
                    errors.append(f"{location} has an uncheckable flex value: {props['flex']}")
                elif grow > 0:
                    errors.append(f"{location} lets comment cards grow with flex: {props['flex']}")
            if "flex-grow" in props:
                value = props["flex-grow"]
                grow = float(value) if NUMBER.fullmatch(value) else None
                if grow is None:
                    errors.append(f"{location} has an uncheckable flex-grow value: {value}")
                elif grow > 0:
                    errors.append(f"{location} lets comment cards grow with flex-grow: {value}")
            if "height" in props and props["height"] not in {
                "auto", "fit-content", "max-content", "min-content", "inherit", "initial", "unset", "revert", "revert-layer"
            }:
                errors.append(f"{location} gives comment cards a fixed height: {props['height']}")
    return errors


def check_grid_stacks(root):
    errors = []
    cache = {}
    for stylesheet, container in GRID_STACKS:
        path = root / stylesheet
        if not path.is_file():
            errors.append(f"missing stylesheet: {stylesheet}")
            continue
        cache.setdefault(stylesheet, list(rules(path)))
        has_start_alignment = False
        for selector_group, props in cache[stylesheet]:
            if container.removeprefix(".") not in class_names(selector_group):
                continue
            if props.get("display") == "grid" or "align-content" in props:
                if props.get("align-content") in {"start", "flex-start"}:
                    has_start_alignment = True
                elif props.get("align-content") in {"stretch", "normal"}:
                    errors.append(f"{stylesheet}: {selector_group.strip()} lets grid rows stretch with align-content: {props['align-content']}")
            # Rows may be declared in a rule apart from display and alignment,
            # so check every rule that targets the stack, not only those.
            for prop in ("grid-template-rows", "grid-auto-rows"):
                if "fr" in props.get(prop, ""):
                    errors.append(f"{stylesheet}: {selector_group.strip()} fills a stacked grid row with {prop}")
        if not has_start_alignment:
            errors.append(f"{stylesheet}: {container} needs align-content:start so auto rows stay content-sized")
    return errors


def check(root):
    return check_flex_stacks(root) + check_shared_card_style(root) + check_grid_stacks(root)


def main():
    root = DEFAULT_ROOT
    if len(sys.argv) == 3 and sys.argv[1] == "--root":
        root = Path(sys.argv[2])
    elif len(sys.argv) != 1:
        print("usage: check-dashboard-last-item-stretch.py [--root PATH]", file=sys.stderr)
        return 2
    errors = check(root)
    if errors:
        for error in errors:
            print(f"dashboard last-item stretch: {error}", file=sys.stderr)
        return 1
    print("dashboard last-item stretch: Task detail stacks keep content-sized items")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
