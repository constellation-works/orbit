#!/usr/bin/env python3
"""The STD-12 source-style checks that stable rustfmt cannot make.

Run through scripts/check-source-style.sh (R12, R13, R16, R19, R21, R22, R26) and
scripts/check-singular-names.sh (R1); each passes its mode as the first
argument. Every tracked or new `.rs` file is read, apart from a file whose
leading comment block says `@generated`, which STD-12 exempts. Each breach is
printed as file:line: rule: message, and any breach exits 1.

- R1 (`names`): a module, source directory or source file name ending in `s`
  fails unless it is allowed. `examples` and `benches` are allowed only as a
  crate-root directory, where Cargo fixes them. `tests` is allowed as the
  crate-root directory and as a module's unit-test directory and module,
  which STD-02 R19 fixes (`<module>/tests/`, `mod tests;`). Module names are
  read from `mod` declarations, inline or with `#[path]`, as well as from
  paths. ALLOWED_SINGULARS holds singulars that end in `s`.
- R12: a blank line separates the module documentation (`//!`, `/*! */` and
  inner attributes) from what follows, the use block from the items around
  it, each top-level item from the next, and each method of an `impl`
  (file-scope or in an inline module) from the member before and after it.
  Doc comments, comments and attributes belong to the item they precede, and
  a comment inside a use block does not split it.
- R13: no line is longer than 100 characters, the width R11 and R13 fix;
  `rustfmt.toml` must not set `max_width` to anything else. A line that lies
  within one string literal (plain, byte, C or raw, maybe followed by the
  punctuation that closes its statement) or is one URL (`scheme://…`, bare,
  in `<…>` or after a comment marker) is exempt.
- R16: a `use` imports from one module: its brace group holds no `::`. A `use`
  is found after an attribute on the same line or after a block comment.
- R19: a `#[rustfmt::skip]` has a comment (`//` or `/* */`, not a doc comment)
  on the line before it.
- R21: a `struct`, `enum` or `union` is followed at once by its impl blocks:
  inherent impls, then impls of standard-library traits, then impls of any
  other trait, with no other item between the type and any of them. Read per
  scope (the file, or an inline `mod`), for the impls of the type's bare name.
  An impl whose self type this scope does not define (`u32`, or a path such
  as `cw_base::Error`) belongs to the types of the scope that its trait's
  generics name (`impl From<Title> for String` belongs to `Title`), and to
  none otherwise. A trait is the standard library's when its path starts with
  `std`, `core` or `alloc`, or is the bare or `module::`-qualified name of a
  trait in STD_TRAITS.
- R22: the members of an inherent impl run in groups: associated consts and
  types; functions with no receiver, those returning `Self` (or the impl's
  type) first; `with_*` functions, whatever their receiver; `&self` methods;
  `&mut self` methods; `self` by value. Inside a group of functions, those
  with a `pub` visibility of any kind come before the private ones. Only the
  members of an impl written over several lines are read.
- R26: no path contains `super::super` (also `super::{super::…}`), read from
  the code with comments and string contents blanked.

Known limits, left to review:
- R1 reads names, so it cannot tell an irregular plural (`children`) from a
  singular, nor a plural from a singular ending in `s` outside the allow-list.
- R16 also reads a `use` written inside a macro's tokens (`stringify!`) as an
  import.
- R19 checks that the comment is there, not that it gives a real reason.
- R19 finds `rustfmt::skip` as written; a spelling with a comment inside the
  path (`#[rustfmt /* … */ :: skip]`) is not found.
- R21 and R22 read an item by its written head, so an item a macro expands
  into is not seen, and a type alias or trait stands between nothing. A
  trait named like a standard one (a project `Error` trait) counts as the
  standard library's. R22 reads a method's receiver from its first parameter
  and a constructor from `Self` or the impl's type in the return type; a
  `Self` that only appears inside a larger type (`Vec<Self>`) counts too.
  Where a group holds both constructors and other functions, R22 puts
  constructors first and, inside each, `pub` before private.
"""

import os
import re
import subprocess
import sys

WIDTH = 100
ALLOWED_SINGULARS = {"status"}
GENERATED = "@generated"

URL = re.compile(r"^(//[/!]?|/\*[*!]?|\*)?\s*<?[A-Za-z][A-Za-z0-9+.-]*://\S+?>?[.,;:)]*(\s*\*/)?$")
USE_START = re.compile(r"(?:^|(?<=[;{}\]]))\s*(?:pub(?:\([^)]*\))?\s+)?use\s")
FN_HEAD = re.compile(
    r"^(pub(\([^)]*\))?\s+)?((const|async|unsafe|default)\s+|extern\s+(\"[^\"]*\"\s+)?)*fn\b"
)
IMPL_HEAD = re.compile(r"^((unsafe|default)\s+)*impl\b")
MOD_HEAD = re.compile(r"^(pub(\([^)]*\))?\s+)?mod\s+(r#)?([A-Za-z_][A-Za-z0-9_]*)\s*([;{])")
USE_HEAD = re.compile(r"^(pub(\([^)]*\))?\s+)?use\b")


class Line:
    """One source line as the lexer saw it."""

    def __init__(self, text, start):
        self.text = text
        self.start = start  # offset of the line's first character in the file
        self.start_depth = 0  # brace depth before the line's first character
        self.end_depth = 0
        self.code = []  # the line with comments and literal contents blanked
        self.closes = []  # depths a `;` or a closing `}` on the line returns to
        self.comments = []  # (kind, text) of each comment piece on the line
        self.starts_in_comment = False

    @property
    def has_code(self):
        return "".join(self.code).strip() != ""

    @property
    def code_text(self):
        return "".join(self.code)

    def is_blank(self):
        return self.text.strip() == "" and not self.starts_in_comment


def lex(text):
    """Lines with brace depth, code, comments, and the literal tokens' spans."""
    lines = []
    offset = 0
    for raw in text.split("\n"):
        lines.append(Line(raw, offset))
        offset += len(raw) + 1
    line_of = []
    for index, line in enumerate(lines):
        line_of.extend([index] * (len(line.text) + 1))
    literals = []  # (start, end) offsets of every string literal token
    depth = 0
    pos = 0
    length = len(text)
    current = 0

    def emit(char, at):
        lines[line_of[at]].code.append(char)

    def advance_to(new_pos):
        nonlocal current
        while current + 1 < len(lines) and lines[current + 1].start <= new_pos:
            current += 1
            lines[current].start_depth = depth
            lines[current].end_depth = depth

    while pos < length:
        advance_to(pos)
        char = text[pos]
        nxt = text[pos + 1] if pos + 1 < length else ""
        if char == "\n":
            pos += 1
            continue
        if char == "/" and nxt == "/":
            end = text.find("\n", pos)
            end = length if end == -1 else end
            body = text[pos:end]
            if body.startswith("//!"):
                kind = "inner_doc"
            elif body.startswith("///") and not body.startswith("////"):
                kind = "outer_doc"
            else:
                kind = "line"
            lines[line_of[pos]].comments.append((kind, body))
            for at in range(pos, end):
                emit(" ", at)
            pos = end
            continue
        if char == "/" and nxt == "*":
            if text.startswith("/*!", pos):
                kind = "inner_doc"
            elif text.startswith("/**", pos) and not text.startswith("/**/", pos):
                kind = "outer_doc"
            else:
                kind = "block"
            level = 0
            scan = pos
            while scan < length:
                if text.startswith("/*", scan):
                    level += 1
                    scan += 2
                elif text.startswith("*/", scan):
                    level -= 1
                    scan += 2
                    if level == 0:
                        break
                else:
                    scan += 1
            for at in range(pos, scan):
                if text[at] != "\n":
                    emit(" ", at)
            first, last = line_of[pos], line_of[min(scan, length) - 1]
            for row in range(first, last + 1):
                lo = max(pos, lines[row].start)
                hi = min(scan, lines[row].start + len(lines[row].text))
                lines[row].comments.append((kind, text[lo:hi]))
                if row > first:
                    lines[row].starts_in_comment = True
            pos = scan
            continue
        boundary = pos == 0 or not (text[pos - 1].isalnum() or text[pos - 1] == "_")
        raw = re.match(r'(?:b|c)?r(#*)"', text[pos:]) if boundary else None
        if raw:
            hashes = raw.group(1)
            close = text.find('"' + hashes, pos + len(raw.group(0)))
            end = length if close == -1 else close + 1 + len(hashes)
            literals.append((pos, end))
            for at in range(pos, end):
                if text[at] != "\n":
                    emit('"' if at in (pos, end - 1) else " ", at)
            pos = end
            continue
        plain = re.match(r'(?:b|c)?"', text[pos:]) if boundary or char == '"' else None
        if plain:
            scan = pos + len(plain.group(0))
            while scan < length and text[scan] != '"':
                scan += 2 if text[scan] == "\\" else 1
            end = min(scan + 1, length)
            literals.append((pos, end))
            for at in range(pos, end):
                if text[at] != "\n":
                    emit('"' if at in (pos, end - 1) else " ", at)
            pos = end
            continue
        if char == "'" or (char == "b" and nxt == "'" and boundary):
            start = pos + (1 if char == "b" else 0)
            char_lit = re.match(r"'(\\(x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]+\}|.)|[^\\'\n])'", text[start:])
            if char_lit:
                end = start + len(char_lit.group(0))
                for at in range(pos, end):
                    emit(" ", at)
                pos = end
                continue
        if char in "{([":
            depth += 1
        elif char in "})]":
            depth -= 1
            if char == "}":
                lines[line_of[pos]].closes.append(depth)
        elif char == ";":
            lines[line_of[pos]].closes.append(depth)
        emit(char, pos)
        lines[line_of[pos]].end_depth = depth
        pos += 1
    advance_to(length)
    return lines, literals


def leading_comments_say_generated(lines):
    """True when a comment in the file's leading comment block says @generated."""
    for line in lines:
        if line.has_code:
            return False
        if any(GENERATED in body for _, body in line.comments):
            return True
    return False


# -- R12 ---------------------------------------------------------------------

def strip_attributes(code):
    """The code after any outer attributes that open the line."""
    rest = code.lstrip()
    while rest.startswith("#[") or rest.startswith("#!["):
        level = 0
        for at, char in enumerate(rest):
            if char == "[":
                level += 1
            elif char == "]":
                level -= 1
                if level == 0:
                    rest = rest[at + 1 :].lstrip()
                    break
        else:
            return ""
    return rest


def is_comment_only(line):
    return not line.has_code and not line.is_blank()


def attribute_end(lines, row):
    """The last line of an attribute that opens at `row` and spans lines."""
    depth = lines[row].start_depth
    while row < len(lines) - 1 and lines[row].end_depth > depth:
        row += 1
    return row


def module_header_end(lines):
    """The index past the module documentation and inner attributes, or 0."""
    row = 0
    last = -1
    while row < len(lines):
        line = lines[row]
        code = line.code_text.strip()
        if line.is_blank() or is_comment_only(line):
            if any(kind == "inner_doc" for kind, _ in line.comments):
                last = row
            row += 1
            continue
        if code.startswith("#!["):
            row = attribute_end(lines, row)
            last = row
            row += 1
            continue
        break
    return last + 1


def check_blank_lines(path, lines, report):
    begin = module_header_end(lines)
    if begin and begin < len(lines) and not lines[begin].is_blank():
        report(path, begin + 1, "R12", "no blank line after the module documentation")
    scan_units(path, lines, begin, len(lines), 0, "module", report)


def scan_units(path, lines, begin, end, depth, mode, report):
    """The items at `depth` in lines[begin:end], in `mode` module, impl or nested."""
    row = begin
    previous = None
    while row < end:
        line = lines[row]
        if line.is_blank():
            row += 1
            continue
        if line.start_depth != depth:
            row += 1
            continue
        if line.has_code and line.code_text.strip().startswith("}") and line.end_depth < depth:
            break
        unit_start = row
        head_row = None
        cursor = row
        while cursor < end:
            current = lines[cursor]
            if current.is_blank() and head_row is None:
                break
            if is_comment_only(current):
                cursor += 1
                continue
            stripped = strip_attributes(current.code_text)
            if stripped == "" and current.code_text.strip().startswith("#"):
                cursor = attribute_end(lines, cursor) + 1
                continue
            head_row = cursor
            break
        if head_row is None:
            row = cursor if cursor > row else row + 1
            continue
        head = strip_attributes(lines[head_row].code_text)
        unit_end = head_row
        while unit_end < end:
            current = lines[unit_end]
            if current.end_depth < depth:
                break
            if current.end_depth == depth and depth in current.closes:
                break
            unit_end += 1
        unit_end = min(unit_end, end - 1)
        if USE_HEAD.match(head):
            kind = "use"
        elif mode == "impl" and FN_HEAD.match(head):
            kind = "method"
        else:
            kind = "item"
        if previous is not None and mode != "nested":
            gap = lines[previous[1] + 1 : unit_start]
            separated = any(between.is_blank() for between in gap)
            same_use_block = previous[0] == "use" and kind == "use"
            grouped_members = mode == "impl" and "method" not in (previous[0], kind)
            if not separated and not same_use_block and not grouped_members:
                if mode == "impl":
                    where = "member of an impl"
                elif "use" in (kind, previous[0]):
                    where = "use block"
                else:
                    where = "top-level item"
                report(path, unit_start + 1, "R12", f"no blank line before this {where}")
        if mode != "impl":
            body = body_range(lines, head_row, unit_end, depth)
            if body is not None and IMPL_HEAD.match(head):
                scan_units(path, lines, body[0], body[1], depth + 1, "impl", report)
            elif body is not None and MOD_HEAD.match(head):
                scan_units(path, lines, body[0], body[1], depth + 1, "nested", report)
        previous = (kind, unit_end)
        row = unit_end + 1


def body_range(lines, head_row, unit_end, depth):
    """The rows inside the item's braces, when its body spans lines."""
    for row in range(head_row, unit_end + 1):
        if lines[row].end_depth == depth + 1 and "{" in lines[row].code_text:
            return (row + 1, unit_end + 1) if row < unit_end else None
    return None


# -- R13 ---------------------------------------------------------------------

def within_one_literal(line, literals):
    """True when the line's text lies inside one string literal token."""
    text = line.text
    first = len(text) - len(text.lstrip())
    last = len(text.rstrip())
    tail = text[:last].rstrip(",;)")
    lo, hi = line.start + first, line.start + len(tail)
    if hi <= lo:
        return False
    return any(start <= lo and hi <= end for start, end in literals)


def check_width(path, lines, literals, report):
    for row, line in enumerate(lines, 1):
        if len(line.text) <= WIDTH:
            continue
        if within_one_literal(line, literals):
            continue
        if URL.match(line.text.strip()):
            continue
        report(path, row, "R13", f"{len(line.text)} characters, over {WIDTH}")


# -- R16 ---------------------------------------------------------------------

def check_use_groups(path, lines, report):
    for row, line in enumerate(lines):
        code = line.code_text
        for found in USE_START.finditer(code):
            statement = code[found.end() :]
            cursor = row
            while ";" not in statement and cursor + 1 < len(lines):
                cursor += 1
                statement += " " + lines[cursor].code_text
            statement = statement.split(";", 1)[0]
            brace = statement.find("{")
            if brace != -1 and "::" in statement[brace:]:
                report(path, row + 1, "R16", "a use brace group holds a path; one use per module")


# -- R19 ---------------------------------------------------------------------

def check_skips(path, lines, report):
    for row, line in enumerate(lines):
        if "rustfmt::skip" not in line.code_text:
            continue
        before = lines[row - 1] if row > 0 else None
        reason = before is not None and not before.has_code and any(
            kind in ("line", "block") for kind, _ in before.comments
        )
        if not reason:
            report(path, row + 1, "R19", "rustfmt::skip needs a comment on the line before saying why")


# -- R21, R22 ---------------------------------------------------------------

STD_TRAITS = {
    "AsMut", "AsRef", "Borrow", "BorrowMut", "Clone", "Copy", "Debug", "Default",
    "Deref", "DerefMut", "Display", "Drop", "Eq", "Error", "Extend", "From",
    "FromIterator", "FromStr", "Hash", "Index", "IndexMut", "Into", "IntoIterator",
    "Iterator", "Ord", "PartialEq", "PartialOrd", "Send", "Sync", "ToOwned",
    "ToString", "TryFrom", "TryInto", "Unpin", "Add", "Sub", "Mul", "Div", "Neg",
    "Not", "Rem", "AddAssign", "SubAssign", "MulAssign", "DivAssign", "BitAnd",
    "BitOr", "BitXor", "Shl", "Shr", "Read", "Write", "Seek", "BufRead", "Future",
    "Termination", "Pointer", "LowerHex", "UpperHex", "Binary", "Octal",
}
VIS = r"(?:pub(?:\s*\([^)]*\))?\s+)?"
TYPE_HEAD = re.compile(r"^" + VIS + r"(struct|enum|union)\s+(?:r#)?([A-Za-z_][A-Za-z0-9_]*)")
CONST_HEAD = re.compile(r"^" + VIS + r"(const|static)\b")
ASSOC_TYPE_HEAD = re.compile(r"^" + VIS + r"(?:default\s+)?type\b")
FN_NAME = re.compile(r"\bfn\s+(?:r#)?([A-Za-z_][A-Za-z0-9_]*)")
PUBLIC = re.compile(r"^pub\b(?!\s*\(\s*(?:self|in\s+self)\s*\))")


class Item:
    """One item at a scope: its kind, name, header text and rows."""

    def __init__(self, kind, name, head, header, start, head_row, end):
        self.kind = kind
        self.name = name
        self.head = head  # the item's head with its attributes stripped
        self.header = header  # the head up to the first `{` or `;`
        self.start = start  # first row, doc comment and attributes included
        self.head_row = head_row
        self.end = end  # last row


def classify(head):
    """The item's kind and name, from the head with attributes stripped."""
    declared = MOD_HEAD.match(head)
    if declared:
        return "mod", declared.group(4)
    if USE_HEAD.match(head):
        return "use", None
    if FN_HEAD.match(head):
        found = FN_NAME.search(head)
        return "fn", found.group(1) if found else None
    if IMPL_HEAD.match(head):
        return "impl", None
    type_def = TYPE_HEAD.match(head)
    if type_def:
        return type_def.group(1), type_def.group(2)
    if CONST_HEAD.match(head):
        return "const", None
    if ASSOC_TYPE_HEAD.match(head):
        return "type", None
    return "other", None


def parse_items(lines, begin, end, depth):
    """The items at `depth` in lines[begin:end], in source order."""
    items = []
    row = begin
    while row < end:
        line = lines[row]
        if line.is_blank() or line.start_depth != depth:
            row += 1
            continue
        if line.has_code and line.code_text.strip().startswith("}") and line.end_depth < depth:
            break
        head_row = None
        cursor = row
        while cursor < end:
            current = lines[cursor]
            if current.is_blank():
                cursor += 1
                continue
            if is_comment_only(current):
                cursor += 1
                continue
            stripped = strip_attributes(current.code_text)
            if stripped == "" and current.code_text.strip().startswith("#"):
                cursor = attribute_end(lines, cursor) + 1
                continue
            head_row = cursor
            break
        if head_row is None:
            break
        head = strip_attributes(lines[head_row].code_text)
        unit_end = head_row
        while unit_end < end:
            current = lines[unit_end]
            if current.end_depth < depth:
                break
            if current.end_depth == depth and depth in current.closes:
                break
            unit_end += 1
        unit_end = min(unit_end, end - 1)
        header = head
        cursor = head_row
        while not re.search(r"[{;]", header) and cursor < unit_end:
            cursor += 1
            header += " " + lines[cursor].code_text.strip()
        header = re.split(r"[{;]", header, maxsplit=1)[0].strip()
        kind, name = classify(head)
        items.append(Item(kind, name, head, header, row, head_row, unit_end))
        row = unit_end + 1
    return items


def body_rows(lines, item, depth):
    """The rows (begin, end) inside the item's braces, when they span lines."""
    found = body_range(lines, item.head_row, item.end, depth)
    if found is None:
        return None
    return found[0], item.end


def angle_end(text, start):
    """The index just past the `>` that closes the `<` at text[start]."""
    level = 0
    flat = text.replace("->", "--")
    for at in range(start, len(flat)):
        if flat[at] == "<":
            level += 1
        elif flat[at] == ">":
            level -= 1
            if level == 0:
                return at + 1
    return len(text)


def impl_target(header):
    """(trait path or None, self-type name or None) of an impl's header."""
    text = re.sub(r"^(?:(?:unsafe|default)\s+)*impl\b\s*", "", header)
    if text.startswith("<"):
        text = text[angle_end(text, 0) :].lstrip()
    text = re.split(r"\bwhere\b", text, maxsplit=1)[0].strip()
    level = 0
    split = None
    flat = text.replace("->", "--")
    for found in re.finditer(r"[<>()\[\]]|\bfor\b(?!\s*<)", flat):
        token = found.group(0)
        if token in "<([":
            level += 1
        elif token in ">)]":
            level -= 1
        elif level == 0:
            split = found
            break
    if split is None:
        trait, self_type = None, text
    else:
        trait, self_type = text[: split.start()].strip(), text[split.end() :].strip()
    path = re.sub(r"^(?:&\s*(?:'[A-Za-z_]+\s+)?(?:mut\s+)?|dyn\s+)+", "", self_type)
    named = re.match(r"((?:[A-Za-z_][A-Za-z0-9_]*\s*::\s*)*)(?:r#)?([A-Za-z_][A-Za-z0-9_]*)", path)
    if not named:
        return trait, None
    prefix = re.sub(r"\s+", "", named.group(1))
    if prefix and prefix.split("::")[0] not in ("crate", "self", "super", "Self"):
        return trait, None  # another crate's type
    return trait, named.group(2)


def is_std_trait(trait):
    path = re.sub(r"\s+", "", re.sub(r"<.*", "", trait)).lstrip("!")
    parts = path.split("::")
    if parts[0] in ("std", "core", "alloc"):
        return True
    if parts[0] in ("crate", "self", "super"):
        return False
    return parts[-1] in STD_TRAITS


def owners(item, defined):
    """The type names in `defined` an impl belongs to.

    An impl belongs to its own type when this scope defines it. An impl for
    another type (`impl From<Title> for String`) belongs to the types of this
    scope that its trait's generics name.
    """
    trait, name = impl_target(item.header)
    if name in defined:
        return {name}
    if trait is None:
        return set()
    return {word for word in re.findall(r"[A-Za-z_][A-Za-z0-9_]*", trait) if word in defined}


def check_adjacency(path, lines, items, report):
    """R21 over one scope's items."""
    defined = {item.name for item in items if item.kind in ("struct", "enum", "union")}
    for position, item in enumerate(items):
        if item.kind not in ("struct", "enum", "union") or item.name is None:
            continue
        run = []
        for follower in items[position + 1 :]:
            if follower.kind != "impl" or item.name not in owners(follower, defined):
                break
            run.append(follower)
        previous = 0
        for follower in run:
            trait, _ = impl_target(follower.header)
            rank = 0 if trait is None else 1 if is_std_trait(trait) else 2
            if rank < previous:
                report(path, follower.head_row + 1, "R21",
                       f"impls of `{item.name}`: inherent, then standard-library traits, "
                       "then the project's")
            previous = max(previous, rank)
        for other in items:
            if other.kind == "impl" and other not in run and item.name in owners(other, defined):
                report(path, other.head_row + 1, "R21",
                       f"this impl of `{item.name}` does not directly follow the type "
                       "and its other impls")


def receiver_group(params):
    """3, 4 or 5 for a `&self`, `&mut self` or `self` first parameter; else None."""
    first = params.split(",", 1)[0] if params else ""
    first = re.sub(r"\s+", " ", first.strip())
    flat = re.sub(r"^&\s*(?:'[A-Za-z_]+\s+)?", "&", first)
    if re.match(r"&mut self$", flat.replace("& mut", "&mut")):
        return 4
    if flat == "&self":
        return 3
    if re.match(r"(mut )?self$", flat):
        return 5
    typed = re.match(r"(?:mut )?self\s*:\s*(.*)$", first)
    if typed:
        kind = typed.group(1)
        if "&mut" in kind.replace("& mut", "&mut"):
            return 4
        if "&" in kind:
            return 3
        return 5
    return None


def parameters(header):
    """The text between the parentheses of a fn header, or None."""
    start = header.find("(", header.find("fn "))
    if start == -1:
        return None, ""
    level = 0
    for at in range(start, len(header)):
        if header[at] == "(":
            level += 1
        elif header[at] == ")":
            level -= 1
            if level == 0:
                return header[start + 1 : at], header[at + 1 :]
    return header[start + 1 :], ""


GROUP_NAMES = {
    0: "an associated const or type",
    1: "an associated function",
    2: "a `with_*` method",
    3: "a `&self` method",
    4: "a `&mut self` method",
    5: "a method taking `self` by value",
}


def member_key(member, owner):
    """(sort key, description) of one impl member, or None when not ranked."""
    if member.kind in ("const", "type"):
        return (0, 0, 0), GROUP_NAMES[0]
    if member.kind != "fn" or member.name is None:
        return None
    params, tail = parameters(member.header)
    receiver = receiver_group(params) if params is not None else None
    private = 0 if PUBLIC.match(member.head) else 1
    prefix = "private " if private else ""
    if member.name.startswith("with_"):
        return (2, 0, private), prefix + GROUP_NAMES[2]
    if receiver is not None:
        return (receiver, 0, private), prefix + GROUP_NAMES[receiver]
    returns_self = re.search(r"\bSelf\b", tail) or (
        owner is not None and re.search(r"\b" + re.escape(owner) + r"\b", tail.split("where")[0])
    )
    description = "a constructor" if returns_self else GROUP_NAMES[1]
    return (1, 0 if returns_self else 1, private), prefix + description


def check_member_order(path, lines, item, depth, report):
    """R22 over one inherent impl."""
    rows = body_rows(lines, item, depth)
    if rows is None:
        return
    owner = impl_target(item.header)[1]
    previous = None
    for member in parse_items(lines, rows[0], rows[1], depth + 1):
        ranked = member_key(member, owner)
        if ranked is None:
            continue
        key, description = ranked
        if previous is not None and key < previous[0]:
            report(path, member.head_row + 1, "R22",
                   f"{description} comes after {previous[1]}; the order is "
                   "associated consts and types, constructors, other associated functions, "
                   "`with_*` methods, `&self`, `&mut self`, then `self` by value, "
                   "`pub` before private in each group")
        else:
            previous = (key, description)


def check_item_order(path, lines, begin, end, depth, report):
    items = parse_items(lines, begin, end, depth)
    check_adjacency(path, lines, items, report)
    for item in items:
        if item.kind == "impl" and impl_target(item.header)[0] is None:
            check_member_order(path, lines, item, depth, report)
        elif item.kind == "mod":
            rows = body_rows(lines, item, depth)
            if rows is not None:
                check_item_order(path, lines, rows[0], rows[1], depth + 1, report)


# -- R26 ---------------------------------------------------------------------

DEEP_SUPER = re.compile(r"\bsuper\s*::\s*(?:\{[^{}]*?\bsuper\b|super\b)")


def check_deep_super(path, lines, report):
    pieces = []
    owner = []  # the line each character of the joined code came from
    for row, line in enumerate(lines):
        code = line.code_text + " "
        pieces.append(code)
        owner.extend([row] * len(code))
    for found in DEEP_SUPER.finditer("".join(pieces)):
        report(path, owner[found.start()] + 1, "R26", "`super::super`: name the item from `crate::`")


# -- R1 ----------------------------------------------------------------------

def crate_root(path):
    """The directory of the nearest Cargo.toml above `path`, or None."""
    folder = os.path.dirname(path)
    while folder:
        if os.path.exists(os.path.join(folder, "Cargo.toml")):
            return folder
        folder = os.path.dirname(folder)
    return "." if os.path.exists("Cargo.toml") else None


def plural(name):
    return name.endswith("s") and name not in ALLOWED_SINGULARS


def check_names(paths, report):
    seen = set()
    for path in paths:
        root = crate_root(path)
        if root is None:
            continue
        rel = os.path.relpath(path, root)
        parts = rel.split(os.sep)
        for index, part in enumerate(parts):
            is_file = index == len(parts) - 1
            name = part[:-3] if is_file and part.endswith(".rs") else part
            shown = os.path.join(root, *parts[: index + 1]) + ("" if is_file else "/")
            if shown in seen or not plural(name):
                continue
            seen.add(shown)
            if index == 0 and not is_file and name in ("tests", "examples", "benches"):
                continue
            if index > 0 and not is_file and name == "tests" and parts[0] == "src":
                continue
            report(shown, 0, "R1", "name it in the singular")
        with open(path, encoding="utf-8") as handle:
            lines, _ = lex(handle.read())
        for row, line in enumerate(lines):
            head = strip_attributes(line.code_text)
            declared = MOD_HEAD.match(head)
            if declared and plural(declared.group(4)) and declared.group(4) != "tests":
                report(path, row + 1, "R1", f"module `{declared.group(4)}`: name it in the singular")


# -- Entry -------------------------------------------------------------------

def tracked_sources():
    listing = subprocess.run(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard", "--", "*.rs"],
        check=True,
        capture_output=True,
    ).stdout.decode()
    return sorted(
        path for path in set(listing.split("\0"))
        if path and not path.startswith("target/") and os.path.exists(path)
    )


def configured_width():
    """The max_width rustfmt.toml sets, or None when it sets none."""
    if not os.path.exists("rustfmt.toml"):
        return None
    with open("rustfmt.toml", encoding="utf-8") as handle:
        for raw in handle:
            found = re.match(r"\s*max_width\s*=\s*(\S+)", raw)
            if found:
                return found.group(1)
    return None


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else "style"
    breaches = []

    def report(path, row, rule, message):
        where = f"{path}:{row}" if row else path
        breaches.append(f"{where}: {rule}: {message}")

    sources = []
    for path in tracked_sources():
        with open(path, encoding="utf-8") as handle:
            text = handle.read()
        lines, literals = lex(text[:-1] if text.endswith("\n") else text)
        if leading_comments_say_generated(lines):
            continue
        sources.append((path, lines, literals))

    if mode == "names":
        check_names([path for path, _, _ in sources], report)
        label = "singular-names"
        rules = "R1"
    else:
        width = configured_width()
        if width is not None and width != str(WIDTH):
            report("rustfmt.toml", 0, "R13", f"max_width is {width}; STD-12 R11 fixes it at {WIDTH}")
        for path, lines, literals in sources:
            check_blank_lines(path, lines, report)
            check_width(path, lines, literals, report)
            check_use_groups(path, lines, report)
            check_skips(path, lines, report)
            check_item_order(path, lines, 0, len(lines), 0, report)
            check_deep_super(path, lines, report)
        label = "source-style"
        rules = "R12, R13, R16, R19, R21, R22, R26"

    if breaches:
        print("\n".join(breaches), file=sys.stderr)
        print(f"{label}: {len(breaches)} breach(es) of STD-12 ({rules})", file=sys.stderr)
        return 1
    print(f"{label}: ok ({len(sources)} files)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
