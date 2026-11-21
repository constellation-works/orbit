#!/usr/bin/env bash
set -euo pipefail

if ! command -v rg >/dev/null 2>&1; then
  echo "check-dependency-direction: ripgrep (rg) is required; install it before running" >&2
  exit 1
fi

repo_root="$(cd "$(dirname "$0")/.." && pwd)"

fail=0

# Conditional scans intentionally accept rg's no-match status (1), but an IO
# error is itself a violation. Emit a match on stdout too, so command
# substitutions and exclusion pipelines cannot turn a scan error into clean.
dependency_matches() {
  local status=0
  command rg "$@" || status=$?
  if [[ "$status" -gt 1 ]]; then
    printf 'dependency scan failed (ripgrep exit %s)\n' "$status"
    return 0
  fi
  return "$status"
}

allowed_internal_deps() {
  case "$1" in
    orbit-types)
      echo ""
      ;;
    orbit-common)
      echo "orbit-types"
      ;;
    orbit-config)
      # Config owns config.toml admission, layering, and persistence; it is a
      # leaf above the shared mechanism crates and depends on nothing higher.
      echo "orbit-common orbit-types"
      ;;
    orbit-clock)
      # The host's sweep clock (clock.toml, unit rendering, install, health).
      # It names only the shared mechanisms so it compiles beside the
      # orbit-store chain, not inside the serial orbit-core library [ORB-22688].
      echo "orbit-common"
      ;;
    orbit-registry)
      # Registry owns local machine/workspace files. Since ORB-12725 this
      # machine's identity is the `[machine]` table in the global config.toml,
      # so Registry reads it through orbit-config's admission rather than
      # keeping a second parser and a second set of validators.
      echo "orbit-common orbit-config orbit-types"
      ;;
    orbit-policy | orbit-exec | orbit-store)
      echo "orbit-common orbit-types"
      ;;
    orbit-auth)
      # Credential code only: no HTTP framework, runtime handle or Orbit store,
      # so its surfaces (CLI, listener) wrap it rather than the reverse.
      echo "orbit-common orbit-types"
      ;;
    orbit-search)
      echo "orbit-common orbit-types"
      ;;
    orbit-tools)
      # Its dev-only self dependency enables the `test-support` feature.
      echo "orbit-common orbit-exec orbit-policy orbit-tools orbit-types"
      ;;
    orbit-agent)
      echo "orbit-common orbit-types"
      ;;
    orbit-engine)
      # Its dev-only self dependency enables the `test-support` feature.
      echo "orbit-agent orbit-common orbit-engine orbit-exec orbit-store orbit-tools orbit-types"
      ;;
    orbit-automation)
      # The sweep pass re-exports the clock's tick budget [ORB-22688].
      echo "orbit-clock orbit-common orbit-store orbit-types"
      ;;
    orbit-runtime)
      # The runtime kernel sits below Core's use cases. It reaches them only
      # through the port Core supplies, so it never depends on orbit-core.
      # Its dev-only self dependency enables the `test-support` feature.
      echo "orbit-common orbit-config orbit-engine orbit-exec orbit-policy orbit-runtime orbit-search orbit-store orbit-tools orbit-types"
      ;;
    orbit-core)
      # Core's script steps, CodeQL fulfilment, plugin conformance, browser kit
      # reads and host probes use Exec in production; sandbox regression tests
      # also compose Core with Exec's `test-support` feature.
      # Its dev-only self dependency enables the `test-support` feature.
      echo "orbit-automation orbit-clock orbit-common orbit-config orbit-core orbit-search orbit-engine orbit-exec orbit-policy orbit-registry orbit-runtime orbit-store orbit-tools orbit-types"
      ;;
    orbit-cmd)
      # The shared application composition layer joins Core runtime kernels to
      # machine-local Registry state for CLI and dashboard consumers.
      echo "orbit-common orbit-config orbit-core orbit-mcp orbit-registry orbit-store orbit-tools orbit-types"
      ;;
    orbit-mcp)
      # MCP owns framing, canonical discovery, and direct SSH stdio transport.
      # Its dev-only self dependency enables scoped transport fixture caps.
      echo "orbit-common orbit-mcp orbit-registry orbit-tools orbit-types"
      ;;
    orbit-web)
      # Lease token scopes and startup machine settings are host-local inputs.
      # Its dev-only self dependency enables the `test-support` feature.
      echo "orbit-auth orbit-common orbit-cmd orbit-config orbit-core orbit-registry orbit-types orbit-engine orbit-store orbit-web"
      ;;
    orbit-cli)
      # The executable assembles MCP and Web feature crates with Registry state
      # and Core's authoritative runtime dispatcher.
      echo "orbit-common orbit-cmd orbit-config orbit-core orbit-mcp orbit-registry orbit-web orbit-types orbit-engine orbit-exec orbit-tools"
      ;;
    *)
      return 1
      ;;
  esac
}

allowed_dev_only_deps() {
  case "$1" in
    orbit-tools)
      # Tools' own integration tests enable its `test-support` feature.
      echo "orbit-tools"
      ;;
    orbit-mcp)
      # Only integration tests enable its test-support transport hooks.
      echo "orbit-mcp"
      ;;
    orbit-web)
      # Only integration tests enable its test-support fixture hooks.
      echo "orbit-engine orbit-store orbit-web"
      ;;
    orbit-cli)
      echo "orbit-engine orbit-exec orbit-tools"
      ;;
    orbit-engine)
      # Engine's own tests enable its `test-support` feature to build host
      # doubles on the null host base; no other crate is reached.
      echo "orbit-engine"
      ;;
    orbit-runtime)
      # The kernel's unit tests enable its `test-support` feature.
      echo "orbit-runtime"
      ;;
    orbit-core)
      # Core's own integration tests enable its `test-support` feature to
      # substitute the detached pipeline worker, and its integration tests
      # reach orbit-policy directly; production code reaches policy through
      # orbit-runtime (and orbit-tools), never directly.
      echo "orbit-core orbit-policy"
      ;;
    *)
      echo ""
      ;;
  esac
}

contains_word() {
  local haystack="$1"
  local needle="$2"
  for word in $haystack; do
    if [[ "$word" == "$needle" ]]; then
      return 0
    fi
  done
  return 1
}

load_workspace_crates() {
  cargo metadata --format-version 1 --no-deps --manifest-path "$repo_root/Cargo.toml" |
    python3 -c '
import json
import sys

metadata = json.load(sys.stdin)
workspace_members = set(metadata["workspace_members"])
workspace_crates = sorted(
    (package["name"], package["manifest_path"])
    for package in metadata["packages"]
    if package["id"] in workspace_members
    and package["name"].startswith("orbit-")
)
for crate, manifest_path in workspace_crates:
    print(f"{crate}\t{manifest_path}")
'
}

load_workspace_dependencies() {
  cargo metadata --format-version 1 --no-deps --manifest-path "$repo_root/Cargo.toml" |
    python3 -c '
import json
import sys

metadata = json.load(sys.stdin)
workspace_members = set(metadata["workspace_members"])
workspace_crates = {
    package["name"]: package
    for package in metadata["packages"]
    if package["id"] in workspace_members
    and package["name"].startswith("orbit-")
}
for crate in sorted(workspace_crates):
    manifest = workspace_crates[crate]["manifest_path"]
    for dependency in workspace_crates[crate]["dependencies"]:
        dependency_name = dependency["name"]
        if dependency_name.startswith("orbit-"):
            kind = dependency["kind"] or "normal"
            print(f"{crate}\t{manifest}\t{dependency_name}\t{kind}")
'
}

workspace_crates=()
workspace_manifests=()
while IFS=$'\t' read -r crate manifest; do
  if [[ -n "$crate" ]]; then
    workspace_crates+=("$crate")
    workspace_manifests+=("$manifest")
  fi
done < <(load_workspace_crates)

if [[ "${#workspace_crates[@]}" -eq 0 ]]; then
  echo "no orbit workspace crates discovered from cargo metadata"
  exit 1
fi

for index in "${!workspace_crates[@]}"; do
  crate="${workspace_crates[$index]}"
  manifest="${workspace_manifests[$index]}"
  if [[ ! -f "$manifest" ]]; then
    echo "missing manifest for ${crate}: ${manifest}"
    fail=1
    continue
  fi

  if ! allowed_internal_deps "$crate" >/dev/null; then
    echo "missing dependency direction policy for workspace crate '${crate}'"
    fail=1
  fi
done

# Cargo metadata resolves workspace inheritance, so inspect the manifests to
# catch a member that pins a dependency already defined by the workspace.
# Include target-specific and dev/build dependencies, not just production ones.
if ! python3 - "$repo_root/Cargo.toml" "${workspace_manifests[@]}" <<'PY'
import pathlib
import sys
import tomllib

workspace_manifest = pathlib.Path(sys.argv[1])
workspace_deps = tomllib.loads(workspace_manifest.read_text())["workspace"]["dependencies"]
failed = False


def check_group(manifest, section, dependencies):
    global failed
    for name, declaration in dependencies.items():
        package = declaration.get("package", name) if isinstance(declaration, dict) else name
        if (name in workspace_deps or package in workspace_deps) and not (
            isinstance(declaration, dict) and declaration.get("workspace") is True
        ):
            print(
                f"{manifest}: {section}.{name} must inherit its workspace dependency "
                "with workspace = true"
            )
            failed = True


for raw_path in sys.argv[2:]:
    path = pathlib.Path(raw_path)
    manifest = path.relative_to(workspace_manifest.parent)
    data = tomllib.loads(path.read_text())
    for section in ("dependencies", "dev-dependencies", "build-dependencies"):
        check_group(manifest, section, data.get(section, {}))
    for target, groups in data.get("target", {}).items():
        for section in ("dependencies", "dev-dependencies", "build-dependencies"):
            check_group(manifest, f"target.{target}.{section}", groups.get(section, {}))

sys.exit(1 if failed else 0)
PY
then
  fail=1
fi

# Process substitution hides a failed listing from set -e and pipefail, so a
# failed cargo metadata run would look like a crate with no internal dependencies.
dependency_listing="$(mktemp)"
trap 'rm -f "$dependency_listing"' EXIT
if ! load_workspace_dependencies >"$dependency_listing"; then
  echo "workspace dependency listing failed"
  exit 1
fi

while IFS=$'\t' read -r crate manifest dependency kind; do
  allowed="$(allowed_internal_deps "$crate")"

  if contains_word "$(allowed_dev_only_deps "$crate")" "$dependency" && [[ "$kind" != "dev" ]]; then
    echo "dependency '${dependency}' in ${manifest} must remain dev-only (kind: ${kind})"
    fail=1
  fi

  if [[ -n "$dependency" ]] && ! contains_word "$allowed" "$dependency"; then
    echo "forbidden dependency '${dependency}' found in ${manifest}"
    echo "  allowed internal deps for ${crate}: ${allowed:-<none>}"
    fail=1
  fi
done < "$dependency_listing"

# Within the application layer [ORB-17924]: constants and DTOs that several
# modules share live in `application::contracts`, which imports none of the
# application modules; `task` reaches the state scheduler only through its
# `TaskAutomationPort`, and `job` reaches review admission only through its
# `ReviewAdmissionHook`. Grouped `use crate::application::{…}` trees count too.
app_src="$repo_root/crates/orbit-core/src/application"
app_modules='task|task_bundle|automation|job|review|distributed|admission|workspace_auto|backlog_forecast|pull_drain|sweep_filing'
# [ORB-22623] The clock settings and schedule display state keep contracts
# below `routines` and `auto_tasks`.
contracts_higher="${app_modules}|routines|auto_tasks"

# Print every `super::` path or use-tree in production code under application module
# `$2` that climbs out of it to `application`, or past it to the crate root and back
# down through `application`, and names one of the `|`-separated modules `$3`; exit
# nonzero when one is found. With `$4` set to `crate-root`, `$1` is any layer under
# the crate source and the modules are crate-root modules: a `super::` chain counts
# when it climbs out to the crate root, and a `crate::` path or use-tree counts too
# [ORB-22005]. Without it, `$1` is the application layer: a `super::` chain counts
# when it climbs to `application`, and an absolute `crate::application::<module>`
# path or use-tree counts too, however it is grouped (`use crate::{X,
# application::<module>::Y}` or a nested `use crate::application::{a::{B},
# <module>::C}`).
# With `$4` set to `named-path`, match the `|`-separated path prefixes `$3`
# anywhere in code, including grouped trees, while preserving comments and
# literals as non-dependencies. A single name matches that identifier alone.
# A leading `super::` chain is resolved from the lexical module under `src/`:
# the file's path plus each enclosing inline `mod`. Two `super` segments
# inside `mod nested` in `application/probe.rs` land on `application`, so
# they do not match `crate::adapter`. The same chain at file depth, or one
# more `super` inside that module, does.
# With `$5` set to `include-tests`, also scan sibling test modules.
super_chains_into() {
  python3 - "$1" "$2" "$3" "${4:-}" "${5:-}" <<'PY'
import pathlib
import re
import sys

app = pathlib.Path(sys.argv[1])
module = sys.argv[2]
upper = set(sys.argv[3].split("|"))
crate_root = sys.argv[4] == "crate-root"
climb_target = [] if crate_root else [app.name]
path_starts = ("super", "crate")
token = re.compile(
    r"(?P<line>//[^\n]*)|(?P<block>/\*)"
    r'|(?P<raw>(?:br|cr|r)(?P<hashes>\#*)")'
    r'|(?P<literal>(?:b|c)?"(?:\\[\s\S]|[^"\\])*"'
    r"|b?'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F_]+\}|[^\n])|[^'\\\n])')"
    r"|::|[^\W\d]\w*|\S"
)
block_delimiter = re.compile(r"/\*|\*/")


def code_tokens(source):
    """Skip Rust comments and literals without losing diagnostic offsets."""
    index = 0
    while match := token.search(source, index):
        index = match.end()
        if match.lastgroup in ("line", "literal"):
            continue
        if match.group("block") is not None:
            depth = 1
            while depth and (delimiter := block_delimiter.search(source, index)):
                depth += 1 if delimiter.group() == "/*" else -1
                index = delimiter.end()
            if depth:
                return
        elif match.group("raw") is not None:
            closing = '"' + match.group("hashes")
            end = source.find(closing, index)
            if end == -1:
                return
            index = end + len(closing)
        else:
            yield match.group(), match.start()


def use_tree(tokens, index, prefix=(), aliases=None):
    """Expand relative paths, carrying the prefix through nested use groups.

    When `aliases` is a list, each `path as name` rename appends `(path, name)`.
    """
    paths = []
    while index + 1 < len(tokens) and tokens[index + 1][0] == "::":
        prefix += (tokens[index][0],)
        index += 2
    if index < len(tokens) and tokens[index][0] == "{":
        index += 1
        while index < len(tokens) and tokens[index][0] != "}":
            member_paths, index = use_tree(tokens, index, prefix, aliases)
            paths.extend(member_paths)
            if index < len(tokens) and tokens[index][0] == ",":
                index += 1
            else:
                break
        if index < len(tokens) and tokens[index][0] == "}":
            index += 1
    elif index < len(tokens):
        paths.append(prefix + (tokens[index][0],))
        index += 1
        if index < len(tokens) and tokens[index][0] == "as":
            if aliases is not None and index + 1 < len(tokens):
                aliases.append((paths[-1], tokens[index + 1][0]))
            index += 2
    return paths, index


def crate_module_parts(file_path):
    """Module path of a Rust file relative to its crate `src/` directory."""
    resolved = file_path.resolve()
    for parent in resolved.parents:
        if parent.name != "src":
            continue
        names = list(resolved.relative_to(parent).with_suffix("").parts)
        if names and names[-1] == "mod":
            names.pop()
        return names
    return None


def inline_module_suffixes(tokens):
    """Extra module names at each token from inline `mod name { … }` items.

    A `mod name { … }` item adds a segment wherever it sits inside module or
    block braces, including inside a function body, because the compiler gives
    it its own module. Tokens inside an attribute, `macro_rules!` definition,
    non-item built-in macro (such as `stringify!`), discarding/wrapper-stripping
    macro arm, or non-macro `(...)`/`[...]` delimiters do not declare inline
    modules, while item-preserving macro invocations keep lexical module depth
    through their emitted arguments. This tokenizer spells a raw identifier as
    `r`, `#`, then the name.

    Matching a macro arm is bounded by a step budget and by a nesting cap. An
    arm that exceeds either does not match. An invocation that no arm matches
    takes the summary of its arms: non-item when no arm emits items or every
    arm strips its module braces, otherwise item-preserving. A large
    pass-through invocation therefore keeps its lexical module depth instead of
    aborting the scan.
    """
    ident = re.compile(r"[^\W\d]\w*$")
    keywords = {
        "as", "async", "await", "break", "const", "continue", "crate", "dyn",
        "else", "enum", "extern", "false", "fn", "for", "if", "impl", "in",
        "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
        "self", "Self", "static", "struct", "super", "trait", "true", "type",
        "unsafe", "use", "where", "while", "yield", "try",
    }
    matching = {"(": ")", "[": "]", "{": "}"}
    non_item_macros = {
        "stringify", "concat", "concat_idents", "env", "option_env",
        "include", "include_str", "include_bytes", "file", "line",
        "column", "module_path", "format_args", "format_args_nl",
        "asm", "global_asm", "naked_asm", "cfg", "compile_error",
        "panic", "assert", "assert_eq", "assert_ne", "debug_assert",
        "debug_assert_eq", "debug_assert_ne", "print", "println",
        "eprint", "eprintln", "write", "writeln", "format", "vec",
        "todo", "unimplemented", "unreachable", "matches",
        "log_syntax", "trace_macros", "quote",
    }
    # Live `match_nodes` nesting allowed per arm. Each level costs a few Python
    # frames against the default recursion limit of 1000, so a repetition over
    # a few hundred tokens would otherwise crash the scan [ORB-26595].
    match_nesting_limit = 200

    def find_matching_close(seq, open_index):
        stack = [matching[seq[open_index][0]]]
        cursor = open_index + 1
        while cursor < len(seq):
            tok = seq[cursor][0]
            if tok in matching:
                stack.append(matching[tok])
            elif tok in (")", "]", "}") and tok in stack:
                while stack:
                    expected = stack.pop()
                    if expected == tok:
                        break
                if not stack:
                    return cursor
            cursor += 1
        return None

    def read_ident(seq, cursor, limit):
        if cursor >= limit or not ident.fullmatch(seq[cursor][0]):
            return None, cursor
        name = seq[cursor][0]
        if (name == "r" and cursor + 2 < limit and seq[cursor + 1][0] == "#"
                and ident.fullmatch(seq[cursor + 2][0])):
            return "r#" + seq[cursor + 2][0], cursor + 3
        if name in keywords:
            return None, cursor
        return name, cursor + 1

    def macro_name_at(seq, end_index, allow_keywords=False):
        if end_index < 0 or not ident.fullmatch(seq[end_index][0]):
            return None, end_index
        if (end_index >= 2 and seq[end_index - 1][0] == "#"
                and seq[end_index - 2][0] == "r"):
            return "r#" + seq[end_index][0], end_index - 2
        tok = seq[end_index][0]
        if not allow_keywords and tok in keywords:
            return None, end_index
        return tok, end_index

    def is_local_macro_start(seq, name_start):
        if name_start == 0 or seq[name_start - 1][0] != "::":
            return True
        return (name_start >= 2 and seq[name_start - 2][0] == "self"
                and (name_start == 2 or seq[name_start - 3][0] != "::"))

    def is_attr_open(seq, delim_index):
        if seq[delim_index][0] != "[" or delim_index < 1:
            return False
        if seq[delim_index - 1][0] == "#":
            return True
        return (delim_index >= 2 and seq[delim_index - 1][0] == "!"
                and seq[delim_index - 2][0] == "#")

    def macro_rules_def_name(seq, delim_index):
        if delim_index < 3:
            return None
        m_name, name_start = macro_name_at(seq, delim_index - 1, allow_keywords=True)
        if m_name is None or name_start < 2:
            return None
        if seq[name_start - 1][0] == "!" and seq[name_start - 2][0] == "macro_rules":
            return m_name
        return None

    def opened_module(seq, mod_index, allow_metavar=False):
        cursor = mod_index + 1
        limit = len(seq)
        if allow_metavar and cursor + 1 < limit and seq[cursor][0] == "$" and ident.fullmatch(seq[cursor + 1][0]):
            mod_key = ("var", seq[cursor + 1][0])
            cursor += 2
        else:
            if cursor >= limit or not ident.fullmatch(seq[cursor][0]):
                return None
            mod_name = seq[cursor][0]
            cursor += 1
            if (mod_name == "r" and cursor + 1 < limit and seq[cursor][0] == "#"
                    and ident.fullmatch(seq[cursor + 1][0])):
                mod_name = seq[cursor + 1][0]
                cursor += 2
            mod_key = ("lit", mod_name) if allow_metavar else mod_name
        if cursor < limit and seq[cursor][0] == "{":
            return mod_key, cursor
        return None

    def parse_macro_arms(body):
        arms = []
        cursor = 0
        limit = len(body)
        while cursor < limit:
            if body[cursor][0] == ";":
                cursor += 1
                continue
            if body[cursor][0] not in matching:
                break
            m_close = find_matching_close(body, cursor)
            if m_close is None or m_close + 2 >= limit:
                break
            if body[m_close + 1][0] != "=" or body[m_close + 2][0] != ">":
                break
            t_open = m_close + 3
            if t_open >= limit or body[t_open][0] not in matching:
                break
            t_close = find_matching_close(body, t_open)
            if t_close is None:
                break
            arms.append((body[cursor + 1:m_close], body[t_open + 1:t_close]))
            cursor = t_close + 1
            if cursor < limit and body[cursor][0] == ";":
                cursor += 1
        return arms

    def visible_macro(scope_stack, name):
        for scope in reversed(scope_stack):
            if name in scope:
                return scope[name]
        return None

    def analyze_transcriber(transcriber, scope_stack, visiting):
        emitted_vars = {}
        forwarded = {}
        seen_lit_mod = False
        t_mods = []
        t_frames = []
        t_opens = {}
        limit = len(transcriber)
        idx = 0
        while idx < limit:
            tok = transcriber[idx][0]
            in_non_item = bool(t_frames) and t_frames[-1][1]
            in_item_ctx = (not t_frames) or t_frames[-1][3]
            if (tok == "mod" and not in_non_item and in_item_ctx
                    and (idx == 0 or transcriber[idx - 1][0] not in ("::", "#"))):
                opened = opened_module(transcriber, idx, allow_metavar=True)
                if opened is not None:
                    t_opens[opened[1]] = opened[0]
                    seen_lit_mod = True
            if (tok == "$" and idx + 1 < limit and ident.fullmatch(transcriber[idx + 1][0])
                    and not in_non_item and in_item_ctx):
                var_name = transcriber[idx + 1][0]
                # `$name` in `mod $name { ... }` supplies the module name; it
                # is not a separate emitted item at the surrounding depth.
                if idx == 0 or transcriber[idx - 1][0] != "mod":
                    emitted_vars.setdefault(var_name, set()).add(tuple(t_mods))
            if tok in matching:
                is_rep = tok == "(" and idx >= 1 and transcriber[idx - 1][0] == "$"
                is_meta_expr = tok == "{" and idx >= 1 and transcriber[idx - 1][0] == "$"
                if is_rep:
                    t_frames.append((matching[tok], in_non_item, None, in_item_ctx))
                    idx += 1
                    continue
                non_item = in_non_item or is_meta_expr or is_attr_open(transcriber, idx)
                if not non_item and macro_rules_def_name(transcriber, idx) is not None:
                    non_item = True
                is_item_macro = False
                if not non_item and idx >= 2 and transcriber[idx - 1][0] == "!":
                    m_name, m_start = macro_name_at(transcriber, idx - 2)
                    if m_name is not None:
                        short = m_name[2:] if m_name.startswith("r#") else m_name
                        if short in non_item_macros:
                            non_item = True
                        elif is_local_macro_start(transcriber, m_start):
                            target_arms = visible_macro(scope_stack, m_name)
                            if target_arms is not None and m_name not in visiting:
                                t_close = find_matching_close(transcriber, idx)
                                sub = transcriber[idx + 1:t_close] if t_close is not None else []
                                if (len(sub) == 2 and sub[0][0] == "$"
                                        and ident.fullmatch(sub[1][0])):
                                    forwarded.setdefault(sub[1][0], []).append(m_name)
                                elif (len(sub) == 6 and sub[0][0] == "$" and sub[1][0] == "("
                                      and sub[2][0] == "$" and ident.fullmatch(sub[3][0])
                                      and sub[4][0] == ")" and sub[5][0] in ("*", "+")):
                                    forwarded.setdefault(sub[3][0], []).append(m_name)
                                target_info = summarize_macro(
                                    m_name, target_arms, scope_stack, visiting | {m_name}
                                )
                                if not target_info["can_emit_items"]:
                                    non_item = True
                                else:
                                    is_item_macro = True
                            else:
                                is_item_macro = True
                        else:
                            is_item_macro = True
                mod_item = t_opens.get(idx) if (tok == "{" and not non_item) else None
                if mod_item is not None:
                    t_mods.append(mod_item)
                allows_item = (not non_item) and (tok == "{" or is_item_macro)
                t_frames.append((matching[tok], non_item, mod_item, allows_item))
            elif tok in (")", "]", "}") and any(frame[0] == tok for frame in t_frames):
                while t_frames:
                    close, _, mod_item, _ = t_frames.pop()
                    if mod_item is not None:
                        t_mods.pop()
                    if close == tok:
                        break
            idx += 1
        return {
            "emitted_vars": emitted_vars,
            "forwarded": forwarded,
            "has_item_output": bool(emitted_vars) or seen_lit_mod,
        }

    def parse_matcher(seq):
        nodes = []
        idx = 0
        limit = len(seq)
        while idx < limit:
            tok = seq[idx][0]
            if tok == "$" and idx + 1 < limit and seq[idx + 1][0] == "(":
                close = find_matching_close(seq, idx + 1)
                if close is None:
                    return None
                children = parse_matcher(seq[idx + 2:close])
                if children is None:
                    return None
                cursor = close + 1
                sep = None
                if cursor + 1 < limit and seq[cursor + 1][0] in ("*", "+"):
                    sep = seq[cursor][0]
                    op = seq[cursor + 1][0]
                    cursor += 2
                elif cursor < limit and seq[cursor][0] in ("*", "+", "?"):
                    op = seq[cursor][0]
                    cursor += 1
                else:
                    return None
                nodes.append(("rep", children, sep, op))
                idx = cursor
            elif tok == "$" and idx + 3 < limit and ident.fullmatch(seq[idx + 1][0]) and seq[idx + 2][0] == ":":
                nodes.append(("var", seq[idx + 1][0], seq[idx + 3][0]))
                idx += 4
            elif tok in matching:
                close = find_matching_close(seq, idx)
                if close is None:
                    return None
                children = parse_matcher(seq[idx + 1:close])
                if children is None:
                    return None
                mod_key = None
                if tok == "{" and len(nodes) >= 2 and nodes[-2] == ("lit", "mod"):
                    prev = nodes[-1]
                    if prev[0] == "var" and prev[2] == "ident":
                        mod_key = ("var", prev[1])
                    elif prev[0] == "lit":
                        mod_key = ("lit", prev[1])
                nodes.append(("group", matching[tok], children, mod_key))
                idx = close + 1
            else:
                nodes.append(("lit", tok))
                idx += 1
        return nodes

    def collect_pattern_vars(nodes):
        out = set()
        for node in nodes:
            if node[0] == "var":
                out.add(node[1])
            elif node[0] in ("group", "rep"):
                out |= collect_pattern_vars(node[1] if node[0] == "rep" else node[2])
        return out

    def consume_item(arg, ai, end_ai):
        cur = ai
        while cur + 1 < end_ai and arg[cur][0] == "#":
            b_idx = cur + 1 if arg[cur + 1][0] == "[" else (
                cur + 2 if cur + 2 < end_ai and arg[cur + 1][0] == "!" and arg[cur + 2][0] == "[" else -1
            )
            if b_idx == -1:
                break
            close = find_matching_close(arg, b_idx)
            if close is None or close >= end_ai:
                return None
            cur = close + 1
        if cur < end_ai and arg[cur][0] == "pub":
            cur += 1
            if cur < end_ai and arg[cur][0] == "(":
                close = find_matching_close(arg, cur)
                if close is None or close >= end_ai:
                    return None
                cur = close + 1
        is_use = cur < end_ai and arg[cur][0] == "use"
        while cur < end_ai:
            tok = arg[cur][0]
            if tok == ";":
                return cur + 1
            if tok in matching:
                close = find_matching_close(arg, cur)
                if close is None or close >= end_ai:
                    return None
                if tok == "{" and not is_use:
                    if close + 1 < end_ai and arg[close + 1][0] == ";":
                        return close + 2
                    return close + 1
                cur = close + 1
            else:
                cur += 1
        return None

    def candidate_var_ends(frag, arg, ai, end_ai, next_lit):
        if frag == "ident":
            _, nxt = read_ident(arg, ai, end_ai)
            return [nxt] if nxt > ai else []
        if frag == "tt":
            if ai >= end_ai:
                return []
            if arg[ai][0] in matching:
                close = find_matching_close(arg, ai)
                return [close + 1] if close is not None and close < end_ai else []
            return [ai + 1]
        if frag == "block":
            if ai < end_ai and arg[ai][0] == "{":
                close = find_matching_close(arg, ai)
                return [close + 1] if close is not None and close < end_ai else []
            return []
        if frag == "lifetime":
            if ai + 1 < end_ai and arg[ai][0] == "'" and ident.fullmatch(arg[ai + 1][0]):
                return [ai + 2]
            return []
        if frag == "vis":
            ends = []
            if ai < end_ai and arg[ai][0] == "pub":
                if ai + 1 < end_ai and arg[ai + 1][0] == "(":
                    close = find_matching_close(arg, ai + 1)
                    if close is not None and close < end_ai:
                        ends.append(close + 1)
                ends.append(ai + 1)
            ends.append(ai)
            return ends
        if frag == "item":
            nxt = consume_item(arg, ai, end_ai)
            return [nxt] if nxt is not None else []
        if frag == "stmt":
            nxt = consume_item(arg, ai, end_ai)
            if nxt is not None:
                return [nxt]
            return [end_ai] if next_lit is None and ai < end_ai else []
        # Remaining fragments (`expr`, `ty`, `pat`, `path`, `meta`, `literal`)
        if next_lit is None:
            return [end_ai] if ai < end_ai else []
        ends = []
        cur = ai
        while cur < end_ai:
            if cur > ai and arg[cur][0] == next_lit:
                ends.append(cur)
            if arg[cur][0] in matching:
                close = find_matching_close(arg, cur)
                if close is None or close >= end_ai:
                    break
                cur = close + 1
            else:
                cur += 1
        return ends

    def match_nodes(nodes, ni, arg, ai, end_ai, base_offset, bindings, mod_matches, steps, on_done):
        # `steps` is per arm: [calls made, live nesting depth]. Past either
        # bound the call fails, the same as an exhausted step budget.
        steps[0] += 1
        if steps[0] > 10000 or steps[1] >= match_nesting_limit:
            return False
        steps[1] += 1
        try:
            return match_step(nodes, ni, arg, ai, end_ai, base_offset,
                              bindings, mod_matches, steps, on_done)
        finally:
            steps[1] -= 1

    def match_step(nodes, ni, arg, ai, end_ai, base_offset, bindings, mod_matches, steps, on_done):
        if ni == len(nodes):
            return on_done(ai)
        node = nodes[ni]
        kind = node[0]
        if kind == "lit":
            if ai < end_ai and arg[ai][0] == node[1]:
                return match_nodes(
                    nodes, ni + 1, arg, ai + 1, end_ai, base_offset,
                    bindings, mod_matches, steps, on_done,
                )
            return False
        if kind == "group":
            close_delim, children, mod_key = node[1:]
            if ai >= end_ai or matching.get(arg[ai][0]) != close_delim:
                return False
            g_end = find_matching_close(arg, ai)
            if g_end is None or g_end >= end_ai:
                return False
            saved_mods = len(mod_matches)
            saved_binds = {k: list(v) for k, v in bindings.items()}

            def after_group(inner_end):
                if inner_end != g_end:
                    return False
                if mod_key is not None:
                    mod_matches.append((mod_key, base_offset + ai, collect_pattern_vars(children)))
                return match_nodes(
                    nodes, ni + 1, arg, g_end + 1, end_ai, base_offset,
                    bindings, mod_matches, steps, on_done,
                )

            if match_nodes(children, 0, arg, ai + 1, g_end, base_offset, bindings, mod_matches, steps, after_group):
                return True
            del mod_matches[saved_mods:]
            bindings.clear()
            bindings.update(saved_binds)
            return False
        if kind == "var":
            var_name, frag = node[1], node[2]
            next_lit = nodes[ni + 1][1] if (ni + 1 < len(nodes) and nodes[ni + 1][0] == "lit") else None
            for nxt in candidate_var_ends(frag, arg, ai, end_ai, next_lit):
                bindings.setdefault(var_name, []).append((base_offset + ai, base_offset + nxt))
                if match_nodes(
                        nodes, ni + 1, arg, nxt, end_ai, base_offset,
                        bindings, mod_matches, steps, on_done,
                ):
                    return True
                bindings[var_name].pop()
                if not bindings[var_name]:
                    del bindings[var_name]
            return False
        if kind == "rep":
            children, sep, op = node[1:]

            def try_rep(cur_ai, count):
                if op == "?" and count > 1:
                    return False
                if (op != "+" or count >= 1) and match_nodes(
                        nodes, ni + 1, arg, cur_ai, end_ai, base_offset,
                        bindings, mod_matches, steps, on_done,
                ):
                    return True
                if op == "?" and count >= 1:
                    return False
                next_pos = cur_ai
                if count > 0 and sep is not None:
                    if next_pos >= end_ai or arg[next_pos][0] != sep:
                        return False
                    next_pos += 1
                saved_mods = len(mod_matches)
                saved_binds = {k: list(v) for k, v in bindings.items()}

                def after_iter(iter_end):
                    return iter_end > next_pos and try_rep(iter_end, count + 1)

                if match_nodes(
                        children, 0, arg, next_pos, end_ai, base_offset,
                        bindings, mod_matches, steps, after_iter,
                ):
                    return True
                del mod_matches[saved_mods:]
                bindings.clear()
                bindings.update(saved_binds)
                return False

            return try_rep(ai, 0)
        return False

    def summarize_macro(name, arms, scope_stack, visiting):
        can_emit = False
        all_strip = bool(arms)
        for matcher, transcriber in arms:
            t_info = analyze_transcriber(transcriber, scope_stack, visiting)
            if t_info["has_item_output"]:
                can_emit = True
            p_ast = parse_matcher(matcher)
            arm_strips = False
            if p_ast is not None:
                for node in p_ast:
                    if node[0] == "group" and node[3] is not None:
                        cvars = collect_pattern_vars(node[2])
                        if not any(node[3] in mods for v in cvars for mods in t_info["emitted_vars"].get(v, ())):
                            arm_strips = True
                            break
            if not arm_strips and t_info["has_item_output"]:
                all_strip = False
        return {"can_emit_items": can_emit, "all_strip": all_strip}

    def classify_macro_invocation(m_name, arms, arg, base_offset, scope_stack, visiting):
        for matcher, transcriber in arms:
            p_ast = parse_matcher(matcher)
            if p_ast is None:
                continue
            bindings = {}
            mod_matches = []
            steps = [0, 0]
            if not match_nodes(
                    p_ast, 0, arg, 0, len(arg), base_offset, bindings,
                    mod_matches, steps, lambda end: end == len(arg),
            ):
                continue
            t_info = analyze_transcriber(transcriber, scope_stack, visiting)
            if not t_info["has_item_output"]:
                return {"non_item": True, "stripped": set(), "non_item_braces": set(),
                        "non_item_ranges": [], "added_mods": ()}
            emitted_vars = t_info["emitted_vars"]
            stripped = set()
            non_item_braces = set()
            for _, brace_abs, cvars in mod_matches:
                if not any(v in emitted_vars for v in cvars):
                    non_item_braces.add(brace_abs)
                else:
                    # These braces are matcher syntax. If the transcriber
                    # emits the captured items, count its output module prefix
                    # below instead of also counting the consumed input braces.
                    stripped.add(brace_abs)
            non_item_ranges = [
                span for v, spans in bindings.items()
                if v not in emitted_vars
                for span in spans
            ]
            # Handle forwarded macros like `($($t:tt)*) => { forward!($($t)*); }`
            for v, targets in t_info["forwarded"].items():
                spans = bindings.get(v, ())
                if not spans:
                    continue
                span_start, span_end = spans[0][0], spans[-1][1]
                sub_arg = tokens[span_start:span_end]
                for target_name in targets:
                    if target_name in visiting:
                        continue
                    target_arms = visible_macro(scope_stack, target_name)
                    if target_arms is None:
                        continue
                    sub_res = classify_macro_invocation(
                        target_name, target_arms, sub_arg, span_start,
                        scope_stack, visiting | {target_name}
                    )
                    if sub_res["non_item"]:
                        non_item_ranges.append((span_start, span_end))
                    stripped |= sub_res["stripped"]
                    non_item_braces |= sub_res["non_item_braces"]
                    non_item_ranges.extend(sub_res["non_item_ranges"])
            # Keep a uniform module prefix for every emitted variable, or its
            # common depth when every emitted prefix has the same length. Module
            # names captured by the matcher resolve from the invocation; if a
            # repeated capture or differing wrapper has different names, a
            # placeholder still preserves the module level for `super`
            # resolution.
            def resolved_mod_prefix(mods):
                prefix = []
                for kind, name in mods:
                    if kind == "lit":
                        prefix.append(name)
                        continue
                    spans = bindings.get(name, ())
                    bound_names = []
                    for span_start, span_end in spans:
                        bound_name, bound_end = read_ident(tokens, span_start, span_end)
                        if bound_name is None or bound_end != span_end:
                            bound_names = []
                            break
                        bound_names.append(bound_name)
                    if bound_names and all(bound_name == bound_names[0]
                                           for bound_name in bound_names):
                        prefix.append(bound_names[0])
                    else:
                        prefix.append(f"${name}")
                return tuple(prefix)

            added_mods = ()
            emitted_prefixes = [
                resolved_mod_prefix(mods)
                for v in emitted_vars if v in bindings
                for mods in emitted_vars[v]
            ]
            if (emitted_prefixes
                    and emitted_prefixes[0]
                    and all(len(prefix) == len(emitted_prefixes[0])
                            for prefix in emitted_prefixes)):
                added_mods = tuple(
                    emitted_prefixes[0][level]
                    if all(prefix[level] == emitted_prefixes[0][level]
                           for prefix in emitted_prefixes)
                    else "$mod"
                    for level in range(len(emitted_prefixes[0]))
                )
            return {
                "non_item": False,
                "stripped": stripped,
                "non_item_braces": non_item_braces,
                "non_item_ranges": non_item_ranges,
                "added_mods": added_mods,
            }
        # Fallback if no arm matched cleanly
        summary = summarize_macro(m_name, arms, scope_stack, visiting)
        return {
            "non_item": (not summary["can_emit_items"]) or summary["all_strip"],
            "stripped": set(),
            "non_item_braces": set(),
            "non_item_ranges": [],
            "added_mods": (),
        }

    suffixes = []
    extras = []
    # Each frame: (close_delim, non_item, added_mods, allows_item, local_macros, is_item_macro)
    frames = []
    top_macros = {}
    opens = {}
    stripped_mod_braces = set()
    non_item_braces = set()
    non_item_ranges = []

    def current_macro_scopes():
        return [top_macros, *(frame[4] for frame in frames if frame[4] is not None)]

    def target_macro_scope():
        for frame in reversed(frames):
            if not frame[1] and not frame[5]:
                return frame[4]
        return top_macros

    def in_non_item_range(pos):
        return any(start <= pos < end for start, end in non_item_ranges)

    for index, (name, _) in enumerate(tokens):
        suffixes.append(tuple(extras))
        in_non_item = (bool(frames) and frames[-1][1]) or in_non_item_range(index)
        in_item_ctx = (not frames) or frames[-1][3]
        if (name == "mod" and not in_non_item and in_item_ctx
                and (index == 0 or tokens[index - 1][0] not in ("::", "#"))):
            opened = opened_module(tokens, index)
            if opened is not None and opened[1] not in stripped_mod_braces:
                opens[opened[1]] = opened[0]
        if name in matching:
            non_item = in_non_item or (name == "{" and index in non_item_braces) or is_attr_open(tokens, index)
            is_item_macro = False
            added_mods = ()
            if not non_item:
                def_name = macro_rules_def_name(tokens, index)
                if def_name is not None:
                    non_item = True
                    close = find_matching_close(tokens, index)
                    if close is not None:
                        target_macro_scope()[def_name] = parse_macro_arms(tokens[index + 1:close])
                elif index >= 2 and tokens[index - 1][0] == "!":
                    m_name, m_start = macro_name_at(tokens, index - 2)
                    if m_name is not None:
                        short = m_name[2:] if m_name.startswith("r#") else m_name
                        if short in non_item_macros:
                            non_item = True
                        elif is_local_macro_start(tokens, m_start):
                            scopes = current_macro_scopes()
                            arms = visible_macro(scopes, m_name)
                            if arms is not None:
                                close = find_matching_close(tokens, index)
                                arg = tokens[index + 1:close] if close is not None else []
                                res = classify_macro_invocation(
                                    m_name, arms, arg, index + 1, scopes, {m_name}
                                )
                                if res["non_item"]:
                                    non_item = True
                                else:
                                    is_item_macro = True
                                    stripped_mod_braces |= res["stripped"]
                                    non_item_braces |= res["non_item_braces"]
                                    non_item_ranges.extend(res["non_item_ranges"])
                                    added_mods = res["added_mods"]
                            else:
                                is_item_macro = True
                        else:
                            is_item_macro = True
            if not non_item and not is_item_macro and name == "{" and index in opens:
                added_mods = (opens[index],)
            extras.extend(added_mods)
            allows_item = (not non_item) and (name == "{" or is_item_macro)
            local_macros = {} if (name == "{" and not non_item and not is_item_macro) else None
            frames.append((matching[name], non_item, added_mods, allows_item, local_macros, is_item_macro))
        elif name in (")", "]", "}") and any(frame[0] == name for frame in frames):
            while frames:
                close, _, added_mods, _, _, _ = frames.pop()
                for _ in added_mods:
                    extras.pop()
                if close == name:
                    break
    return suffixes


def resolve_super_chain(segments, module_parts):
    """Rewrite a leading `super::` chain as a `crate::` path, or None."""
    if module_parts is None or not segments or segments[0] != "super":
        return None
    ups = 0
    while ups < len(segments) and segments[ups] == "super":
        ups += 1
    if ups > len(module_parts):
        return None
    climbed = module_parts[:len(module_parts) - ups]
    return ("crate", *climbed, *segments[ups:])


failed = False
target = app / module
if not target.exists():
    alt = target.with_name(target.name + ".rs") if target.suffix == "" else None
    if alt is not None and alt.is_file():
        target = alt
    else:
        print(f"dependency scan path missing: {app / module}", file=sys.stderr)
        sys.exit(1)
paths = [target] if target.is_file() else sorted(target.rglob("*.rs"))
for path in paths:
    rel = path.relative_to(app)
    if sys.argv[5] != "include-tests" and ("tests" in rel.parts[:-1] or rel.name == "tests.rs"):
        continue
    parts = ([] if crate_root and module else [app.name]) + list(rel.with_suffix("").parts)
    if parts[-1] == "mod":
        parts.pop()
    source = path.read_text()
    tokens = list(code_tokens(source))
    if sys.argv[4] == "named-path":
        prefixes = [tuple(name.split("::")) for name in upper]
        # A `use <path>::landing as l;` rename lets `l::checks` stand for
        # `landing::checks`: track each alias of a multi-segment prefix's first
        # segment, including aliases of aliases, and match prefixes through it.
        renames = []
        for index, (name, _) in enumerate(tokens):
            if name == "use":
                use_tree(tokens, index + 1, (), renames)
        expanded = []
        for prefix in prefixes:
            if len(prefix) < 2:
                continue
            heads = {prefix[0]}
            grown = True
            while grown:
                grown = False
                for segments, alias in renames:
                    if segments[-1] == "self" and len(segments) > 1:
                        segments = segments[:-1]
                    if segments[-1] in heads and alias not in heads and alias != "_":
                        heads.add(alias)
                        grown = True
            expanded.extend((head, *prefix[1:]) for head in heads - {prefix[0]})
        prefixes = [*prefixes, *expanded]
        module_parts = crate_module_parts(path)
        suffixes = inline_module_suffixes(tokens)

        def matches(segments, at):
            def hit(candidate):
                return any(candidate[:len(prefix)] == prefix for prefix in prefixes)
            if hit(segments):
                return True
            here = None if module_parts is None else [*module_parts, *suffixes[at]]
            absolute = resolve_super_chain(segments, here)
            return absolute is not None and hit(absolute)

        for index, (name, start) in enumerate(tokens):
            starts_prefix = any(name == prefix[0] for prefix in prefixes)
            # Only the first `super` of a path. A later `super` has climbed
            # less and would name a deeper module than the chain does.
            starts_super = (name == "super" and (index == 0 or tokens[index - 1][0] != "::")
                            and index + 1 < len(tokens) and tokens[index + 1][0] == "::")
            if not starts_prefix and not starts_super:
                continue
            named_paths, _ = use_tree(tokens, index)
            if any(matches(segments, index) for segments in named_paths):
                number = source.count("\n", 0, start) + 1
                print(f"{path}:{number}:{source.splitlines()[number - 1]}")
                failed = True
        continue
    index = 0
    while index + 1 < len(tokens):
        if tokens[index][0] not in path_starts or tokens[index + 1][0] != "::":
            index += 1
            continue
        start = tokens[index][1]
        paths, index = use_tree(tokens, index)
        for segments in paths:
            ups = 0
            while ups < len(segments) and segments[ups] == "super":
                ups += 1
            if segments[0] == "crate":
                absolute = segments[1:] if crate_root else segments[2:]
                named = (len(absolute) > 0 and absolute[0] in upper
                         and (crate_root or segments[1:2] == (app.name,)))
            else:
                # Resolve the chain from this file's module depth [ORB-22674]. It
                # names an upper module when it lands on the climb target and an
                # `upper` module follows, or when it climbs past the target to the
                # crate root and descends `application::<upper>`. A landing inside
                # a module, such as `application::task::distributed`, names none.
                climbed = parts[: max(len(parts) - ups, 0)]
                resolved = [*climbed, *segments[ups:]]
                depth = len(climb_target)
                named = (len(climbed) <= depth
                         and resolved[:depth] == climb_target
                         and len(resolved) > depth
                         and resolved[depth] in upper)
            if named:
                number = source.count("\n", 0, start) + 1
                print(f"{path}:{number}:{source.splitlines()[number - 1]}")
                failed = True
                break
sys.exit(1 if failed else 0)
PY
}

# orbit-core is one crate, but its internal boundaries are directional too.
# The runtime kernel lives in orbit-runtime, which cannot reach Core's use
# cases or adapters (the dependency policy above); composition is the only
# owner allowed to join resolved config, bootstrap, runtime construction, and
# adapter registration.
# Grouped `use crate::{adapter::…}` trees count as an import too.
if ! super_chains_into "$repo_root/crates/orbit-core/src/application" "" 'crate::adapter' named-path; then
  echo "forbidden orbit-core application-to-adapter import"
  fail=1
fi

# Resolve relative use trees at each file's module depth: grouped and multiline
# imports into higher modules count, while imports within contracts stay legal.
if dependency_matches -n "crate::application::(${contracts_higher})\\b" \
  "$app_src/contracts" -g '*.rs' ||
  dependency_matches -n -U "crate::application::\\{[^}]*\\b(${contracts_higher})\\b" \
    "$app_src/contracts" -g '*.rs' ||
  ! super_chains_into "$app_src" contracts "$contracts_higher"; then
  echo "orbit-core application contracts must not import the application modules"
  fail=1
fi

# Config path primitives are below aliases and operation, which must not
# depend on layered admission. The leaf may only use TOML and std.
config_src="$repo_root/crates/orbit-config/src"
# These are policy owners, not optional files. A rename must update the rule,
# rather than silently dropping that boundary from the scan.
for module in aliases.rs seeding.rs config_document.rs crew operation provider_limit \
  layering plugin_enablement.rs unknown.rs resolved/compatibility.rs config_io.rs \
  registry crew_defaults.rs; do
  if [[ ! -e "$config_src/$module" ]]; then
    echo "dependency scan path missing: $config_src/$module" >&2
    exit 1
  fi
done
for module in aliases.rs operation; do
  if ! super_chains_into "$config_src" "$module" layering crate-root; then
    echo "orbit-config ${module} must not name layering"
    fail=1
  fi
done
# [ORB-22707] The leaf also owns the typed path read, so it may use serde and
# orbit-common's error and redaction, but never a sibling module.
if dependency_matches -n '\b(crate|super|self)::|^[[:space:]]*extern crate[[:space:]]' "$config_src/config_document.rs"; then
  echo "orbit-config config_document must remain a TOML/serde/orbit-common/std leaf"
  fail=1
fi
if dependency_matches -n '^[[:space:]]*(pub(\([^)]*\))?[[:space:]]+)?use[[:space:]]' "$config_src/config_document.rs" |
  grep -vE '[[:space:]]use[[:space:]]+(std|toml|serde|orbit_common)::|:use[[:space:]]+(std|toml|serde|orbit_common)::'; then
  echo "orbit-config config_document must remain a TOML/serde/orbit-common/std leaf"
  fail=1
fi

# [ORB-22707] The key registry sits above the domain admission modules whose
# admitters and choices its key table calls. Those modules read TOML through
# config_document and leave snapshot projections to registry/settings, so none
# names the registry; its crate-root re-exports count as the same edge.
config_registry_names='registry|AutoConcurrencySettings|CONFIG_KEY_REGISTRY|ConfigKeyDescriptor|ConfigKeyTier|ConfigSection|ConfigSnapshot|ExperimentSettings|ExploreBriefSettings|ExploreSettings|LeaseListenerSettings|LeaseListenerTlsSettings|MachineSettings|MidRunSteeringSettings|NoticeKindHours|NoticeSettings|RegressionRiskSettings|ResourceThrottleSettings|WorkerContainmentSettings|admit_config_key|admit_settable_config_key|config_key_options|describe_config_key|immutable_key_note|is_global_only_key'
for module in aliases.rs seeding.rs crew operation provider_limit; do
  if ! super_chains_into "$config_src" "$module" "$config_registry_names" crate-root; then
    echo "orbit-config ${module} must not name the registry; read TOML through config_document"
    fail=1
  fi
done

# [ORB-22315] File reads and path admission live in the config_io leaf, below
# loading and diagnostics and below the mutable ConfigStore those modules must
# not reach. The crate-root re-exports of the store's types count as the same
# edge. The leaf itself uses only std and orbit-common: no sibling module and no
# TOML or serde parsing.
config_store_names='store|ConfigStore|ConfigScope|WorkspaceInitMode'
for module in layering plugin_enablement.rs unknown.rs seeding.rs; do
  if ! super_chains_into "$config_src" "$module" "$config_store_names" crate-root; then
    echo "orbit-config ${module} must not name the store; read through config_io"
    fail=1
  fi
done
if ! super_chains_into "$config_src" resolved/compatibility.rs store crate-root; then
  echo "orbit-config resolved/compatibility must read through config_io, not the store"
  fail=1
fi
if dependency_matches -n '\b(crate|super|self|toml|toml_edit|serde)::|^[[:space:]]*extern crate[[:space:]]' "$config_src/config_io.rs"; then
  echo "orbit-config config_io must remain a std/orbit-common leaf"
  fail=1
fi
if dependency_matches -n '^[[:space:]]*(pub(\([^)]*\))?[[:space:]]+)?use[[:space:]]' "$config_src/config_io.rs" |
  grep -vE '[[:space:]]use[[:space:]]+(std|orbit_common)::|:use[[:space:]]+(std|orbit_common)::'; then
  echo "orbit-config config_io must remain a std/orbit-common leaf"
  fail=1
fi

# [ORB-22417] Built-in crew defaults live in the crew_defaults leaf, below
# snapshot admission, resolved config, layering and seeding, so the registry
# admits the default crews without naming resolved config (the crate-root
# re-exports of resolved config count as the same edge). The leaf itself uses
# only std, orbit-common and orbit-types: no sibling module.
config_resolved_names='resolved|ResolvedConfig|PrSettings|CodexExecutionPolicy|ExecutionEnvPolicy|CompatibilityConfigKey|CompatibilityConfigKeyDisposition|RETIRED_SYSTEM_CREW|RETIRED_SYSTEM_CREW_WARNING|compatibility_config_keys_at|compatibility_config_keys_present|disabled_crew_message|executor_host_os|names_retired_system_crew'
if ! super_chains_into "$config_src" registry "$config_resolved_names" crate-root; then
  echo "orbit-config registry must not name resolved config; read crew defaults through crew_defaults"
  fail=1
fi
if dependency_matches -n '\b(crate|super|self|toml|toml_edit|serde|serde_json|jsonschema|tracing)::|^[[:space:]]*extern crate[[:space:]]' "$config_src/crew_defaults.rs"; then
  echo "orbit-config crew_defaults must remain a std/orbit-common/orbit-types leaf"
  fail=1
fi
if dependency_matches -n '^[[:space:]]*(pub(\([^)]*\))?[[:space:]]+)?use[[:space:]]' "$config_src/crew_defaults.rs" |
  grep -vE '[[:space:]]use[[:space:]]+(std|orbit_common|orbit_types)::|:use[[:space:]]+(std|orbit_common|orbit_types)::'; then
  echo "orbit-config crew_defaults must remain a std/orbit-common/orbit-types leaf"
  fail=1
fi

# Print every call in production code under application module `$2` (outside
# the `|`-separated path prefixes `$5`, relative to `$1`) to an inherent
# `OrbitRuntime` method that an `impl OrbitRuntime` block under one of the
# `|`-separated higher modules `$3` defines with a visibility that reaches
# `$2`; exit nonzero when one is found. The names are derived, so a new
# inherent method is covered without being listed. A call whose
# `<path>:<line>:<text>` matches the regex `$4` is a named exact-line
# allowance for a same-named method on another type (a store read).
# `$6` (default `$1`) is the directory `$3` is relative to, when the defining
# modules are not application modules: `$3` empty means all of it [ORB-22004].
# An entry of `$3` may name one source file rather than a module directory, and
# `$1` may be another layer under the crate source [ORB-22005].
inherent_calls_into() {
  python3 - "$1" "$2" "$3" "$4" "$5" "${6:-}" <<'PY'
import pathlib, re, sys
app = pathlib.Path(sys.argv[1]); module = sys.argv[2]
upper = sys.argv[3].split("|")
allowed = re.compile(sys.argv[4]) if len(sys.argv) > 4 and sys.argv[4] else None
exempt = [p for p in (sys.argv[5].split("|") if len(sys.argv) > 5 and sys.argv[5] else [])]
defs_root = pathlib.Path(sys.argv[6]) if len(sys.argv) > 6 and sys.argv[6] else app
def_prefix = list(defs_root.relative_to(app.parent).parts)
token = re.compile(
    r"(?P<line>//[^\n]*)|(?P<block>/\*)"
    r'|(?P<raw>(?:br|cr|r)(?P<hashes>\#*)")'
    r'|(?P<literal>(?:b|c)?"(?:\\[\s\S]|[^"\\])*"'
    r"|b?'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F_]+\}|[^\n])|[^'\\\n])')"
    r"|::|[^\W\d]\w*|\S"
)
block_delimiter = re.compile(r"/\*|\*/")
def code_tokens(source):
    index = 0
    while match := token.search(source, index):
        index = match.end()
        if match.lastgroup in ("line", "literal"):
            continue
        if match.group("block") is not None:
            depth = 1
            while depth and (d := block_delimiter.search(source, index)):
                depth += 1 if d.group() == "/*" else -1
                index = d.end()
            if depth:
                return
        elif match.group("raw") is not None:
            closing = '"' + match.group("hashes")
            end = source.find(closing, index)
            if end == -1:
                return
            index = end + len(closing)
        else:
            yield match.group(), match.start()
def production(root, base):
    for path in [root] if root.is_file() else sorted(root.rglob("*.rs")):
        rel = path.relative_to(base)
        if "tests" in rel.parts[:-1] or rel.name == "tests.rs":
            continue
        yield path, rel
def visibility(toks, i, parts):
    """The module path an item's `pub` restriction names, or None if private."""
    while i > 0 and toks[i - 1][0] in ("async", "const", "unsafe"):
        i -= 1
    if i > 0 and toks[i - 1][0] == "pub":
        return []
    if i > 0 and toks[i - 1][0] == ")":
        j = i - 1
        while j > 0 and toks[j][0] != "(":
            j -= 1
        if j > 0 and toks[j - 1][0] == "pub":
            inner = [t for t, _ in toks[j + 1:i - 1] if t != "::"]
            if inner == ["crate"]:
                return []
            if inner == ["super"]:
                return parts[:-1]
            if inner[:1] == ["in"] and inner[1:2] == ["crate"]:
                return inner[2:]
    return None
lower_path = [app.name, *[part for part in module.split("/") if part]]
names = set()
for top in upper:
    # A module is a directory or, like `tag_instructions`, a single file.
    # An empty name is the defining root itself. A missing module is a scan
    # hole, not an empty layer.
    top_root = defs_root / top
    if not top_root.exists() and top != "" and (defs_root / f"{top}.rs").is_file():
        top_root = defs_root / f"{top}.rs"
    if not top_root.exists():
        missing = defs_root / top if top else defs_root
        print(f"dependency scan path missing: {missing}", file=sys.stderr)
        sys.exit(1)
    for path, rel in production(top_root, defs_root):
        parts = [*def_prefix, *rel.with_suffix("").parts]
        if parts[-1] == "mod":
            parts.pop()
        toks = list(code_tokens(path.read_text()))
        i = 0
        while i < len(toks):
            t = toks[i][0]
            j = i + 1
            if t == "impl":
                if j + 1 < len(toks) and toks[j][0] == "crate" and toks[j + 1][0] == "::":
                    j += 2
                if j + 1 < len(toks) and toks[j][0] == "OrbitRuntime" and toks[j + 1][0] == "{":
                    depth = 0
                    k = j + 1
                    while k < len(toks):
                        tk = toks[k][0]
                        if tk == "{":
                            depth += 1
                        elif tk == "}":
                            depth -= 1
                            if depth == 0:
                                break
                        elif tk == "fn" and depth == 1 and k + 1 < len(toks):
                            scope = visibility(toks, k, parts)
                            if scope is not None and lower_path[:len(scope)] == scope:
                                names.add(toks[k + 1][0])
                        k += 1
                    i = k
            i += 1
identifier = re.compile(r"[^\W\d]\w*")
def qualified_type(toks, end):
    """The type that the `::` after `toks[end]` qualifies, or None.

    `Type::` and `<Type>::` (or `<path::Type>::`) name the type; `<Type>` is
    the qualified-self form. A turbofish (`Vec::<T>::`) names `Vec`, not `T`.
    """
    if end < 0:
        return None
    if toks[end][0] != ">":
        return toks[end][0]
    k = end - 1
    if k < 0 or not identifier.fullmatch(toks[k][0]):
        return None
    name = toks[k][0]
    while k >= 2 and toks[k - 1][0] == "::" and identifier.fullmatch(toks[k - 2][0]):
        k -= 2
    if k >= 1 and toks[k - 1][0] == "<" and not (k >= 2 and toks[k - 2][0] == "::"):
        return name
    return None
failed = False
scan_root = app / module
if not scan_root.exists():
    alt = scan_root.with_name(scan_root.name + ".rs") if scan_root.suffix == "" else None
    if alt is not None and alt.is_file():
        scan_root = alt
    else:
        print(f"dependency scan path missing: {app / module}", file=sys.stderr)
        sys.exit(1)
for path, rel in production(scan_root, app):
    if any(str(rel).startswith(e) for e in exempt):
        continue
    source = path.read_text()
    lines = source.splitlines()
    toks = list(code_tokens(source))
    for i, (t, start) in enumerate(toks):
        if t not in names or i + 1 >= len(toks) or i == 0 or toks[i - 1][0] == "fn":
            continue
        nxt = toks[i + 1][0]
        if nxt != "(" and not (nxt == "::" and i + 2 < len(toks) and toks[i + 2][0] == "<"):
            continue
        # An inherent method is reached by `receiver.name(`, `OrbitRuntime::name(`
        # (or `Self::`), or the qualified-self form `<OrbitRuntime>::name(` and
        # `<crate::OrbitRuntime>::name(` [ORB-22159]. A bare `name(` is a free
        # function, which the path checks already cover, so it is not an up-call.
        prev = toks[i - 1][0]
        if prev != "." and not (prev == "::" and qualified_type(toks, i - 2) in ("OrbitRuntime", "Self")):
            continue
        number = source.count("\n", 0, start) + 1
        hit = f"{rel}:{number}:{lines[number - 1]}"
        if allowed and allowed.search(hit):
            continue
        print(f"{path}:{number}:{lines[number - 1]}")
        failed = True
sys.exit(1 if failed else 0)
PY
}

# [ORB-22004] The `crate::adapter` grep above cannot see an inherent `OrbitRuntime`
# method, which carries no import. The adapter defines those on `impl OrbitRuntime`
# blocks; application reaches them only through the ports the adapter implements
# (`ToolInvocationPort`, `PullTransportPort`). The names are derived from the
# adapter's blocks, so a new adapter method is covered without being listed.
# Same-named methods on receivers that are not `OrbitRuntime`: the skill catalog's
# `doctor` and the plugin store's `list_plugins`. Exact lines only.
adapter_upcall_allowed='^health/artifact_diagnosis\.rs:[0-9]+:.*runtime\.skill_catalog\(\)\.doctor\(|^skill\.rs:[0-9]+:.*self\.skill_catalog\(\)\.doctor\(|^plugin/inspect/summary\.rs:[0-9]+:\s+\.list_plugins\(\)\?$|^plugin/secrets\.rs:[0-9]+:.*stores\(\)\.plugins\(\)\.list_plugins\(|^tool_name_migration/plugin_manifest\.rs:[0-9]+:.*stores\(\)\.plugins\(\)\.list_plugins\('
if ! inherent_calls_into "$app_src" "" "" "$adapter_upcall_allowed" '' "$repo_root/crates/orbit-core/src/adapter"; then
  echo "orbit-core application must not call inherent OrbitRuntime methods defined under adapter; reach them through an application-owned port [ORB-22004]"
  fail=1
fi

# [ORB-22005] Composition is the only joiner, and `bootstrap -> application`
# stays because bootstrap seeds through application-owned definitions, so
# neither application nor the adapter may name bootstrap or composition or call
# an inherent `OrbitRuntime` method either defines. Request-time task bundle
# operations live in `application::task_bundle`, which the adapter calls
# directly [ORB-24534].
# The names are derived from the `impl OrbitRuntime` blocks under `composition/`
# and under `bootstrap/`. Exact lines allowed: the lexical index store's own
# `reindex_tasks`, and the `#[cfg(test)]` fixture that opens a runtime.
core_src="$repo_root/crates/orbit-core/src"
joiner_upcall_allowed='^search/runtime\.rs:[0-9]+:.*lexical_index\(\)\.store\(\)\?\.reindex_tasks\(|^test_support\.rs:[0-9]+:.*OrbitRuntime::from_roots\('
for layer in application adapter; do
  if ! super_chains_into "$core_src/$layer" "" 'bootstrap|composition' crate-root ||
    ! inherent_calls_into "$core_src/$layer" "" 'composition|bootstrap' \
      "$joiner_upcall_allowed" '' "$core_src"; then
    echo "orbit-core ${layer} must not name bootstrap or composition or call their inherent OrbitRuntime methods; reach them through an application-owned port [ORB-22005]"
    fail=1
  fi
done

# Runtime is the lowest core layer. Composition supplies its application port;
# runtime must not name higher layers or call their inherent runtime methods.
if ! super_chains_into "$core_src/runtime" "" 'application|adapter|bootstrap|composition' crate-root ||
  ! inherent_calls_into "$core_src/runtime" "" 'application|adapter' '' '' "$core_src"; then
  echo "orbit-core runtime must not name higher layers or call application or adapter inherent OrbitRuntime methods"
  fail=1
fi

# [ORB-17924], [ORB-17943] `task` reaches the state scheduler and its bounded
# `Source` Git reader only through `task::TaskAutomationPort` and
# `task::SourceGit`, with no allowlist. A call to an inherent `OrbitRuntime`
# method that automation defines is the same edge by name [ORB-22035].
if dependency_matches -n 'crate::application::automation\b|(super::)+automation\b' \
  "$app_src/task" -g '*.rs' -g '!**/tests/**' ||
  dependency_matches -n -U 'crate::application::\{[^}]*\bautomation\b' \
    "$app_src/task" -g '*.rs' -g '!**/tests/**' ||
  ! super_chains_into "$app_src" task automation ||
  ! inherent_calls_into "$app_src" task automation '' ''; then
  echo "orbit-core task must reach automation through task::TaskAutomationPort and must not call its inherent methods"
  fail=1
fi

# [ORB-17936] `task` is the lowest of the ten modules above contracts. It
# reaches `job`, `review`, `distributed`, `pull_drain`, `admission`,
# `sweep_filing` and `workspace_auto` only through the ports it owns
# (`TaskJobPort`, `TaskReviewPort`, `TaskDistributedPort`,
# `TaskSweepFilingPort`, `TaskWorkspaceAutoPort`), which those modules
# implement on `OrbitRuntime`. A path, a grouped tree, a `super::` chain that
# climbs out of `task` into one of them, or a call to an inherent method they
# define is a reintroduced edge [ORB-17972], [ORB-22003]. The names are
# derived from their `impl OrbitRuntime` blocks, so port methods use distinct
# names. `tag_instructions` sits outside the module order but above `task`
# [ORB-22289]: task reads commit-pinned files through its own
# `task::base_revision`, never through it. `backlog_forecast` is above
# `workspace_auto` and task has no port to it [ORB-22287]: only the two
# re-export lines below name it, a compatibility shim for `lib.rs` and
# `orbit-web`. `codebase` sits outside the order but above `task` [ORB-22625]:
# task owns the pure block classification (`task::classify_block`) and records
# a done landing's diff stats through `TaskCodebasePort`, which `codebase`
# implements on `OrbitRuntime`; `codebase` reads `distributed` itself.
# `resident` sits outside the order but above `task` [ORB-22955]: it drives
# task writes, so a decision answer applies a resident grant, and a task
# comment write admits a resident record and refuses one needing redaction,
# only through `TaskResidentPort`, which `resident` implements on
# `OrbitRuntime`. `experiment` sits outside the order but above `task`
# [ORB-23015]: it reads tasks and their crew source back, so task show's
# assignment readout and the provider-limit gate's experiment-pool check go
# only through `TaskExperimentPort`, which `experiment` implements on
# `OrbitRuntime`. `search` sits above `task` [ORB-23016]: it reads task
# candidates and re-exports task-owned selector matching; task must never
# reach search for that pure helper. `notice` sits outside the order but
# above `task` [ORB-23017]: it reads tasks for poster authority and
# linked-task status, so a transition into done closes linked notices only
# through `TaskNoticePort`, which `notice` implements on `OrbitRuntime` in
# `notice/close.rs` by delegating to the existing materializer. Tick repair,
# dedupe, revision checks and owner fencing stay in notice. `evolution` sits
# above `task` [ORB-23018]: it reads tasks and replays their comments, so task
# projects committed checkpoint comments only through `TaskEvolutionPort`,
# implemented in `evolution/operations.rs`. Ingestion and source-keyed replay
# stay in evolution, including post-commit error propagation. `agent_corpus`
# sits outside the order but above `task` [ORB-23421]: it reads decision
# rules, task history and tasks back, and reads blobs through
# `task::base_revision`, so task's decision-answer append, precedent digest
# and final-recovery appends go only through `TaskCorpusPort`, which
# `agent_corpus` implements on `OrbitRuntime` in `agent_corpus.rs` by
# forwarding to its inherent methods.
task_upper_modules='search|experiment|job|review|distributed|pull_drain|admission|backlog_forecast|sweep_filing|workspace_auto|tag_instructions|codebase|resident|notice|evolution|agent_corpus'
# Same-named methods on other types are not upper inherent methods; only
# these exact lines are allowed: store reads through `stores().jobs()` and
# `stores().tasks()`.
task_upper_store_allowed='^task/steering\.rs:[0-9]+:.*stores\(\)\.jobs\(\)\.read_run_state|^task/steering\.rs:[0-9]+:\s+\.read_run_state\(run_id\)|^task/steering\.rs:[0-9]+:\s+\.read_run_state\(&source\.run_id\)|^task/pilot/drain_promotion\.rs:[0-9]+:\s+\.read_run_state\(run_id\)|^task/external_merge/candidate\.rs:[0-9]+:\s+\.landing_start_requests\(\)'
task_upper_paths="$(dependency_matches -n "crate::application::(${task_upper_modules})\\b" \
  "$app_src/task" -g '*.rs' -g '!**/tests/**' || true)"
if { [[ -n "$task_upper_paths" ]] && printf '%s\n' "$task_upper_paths"; } ||
  dependency_matches -n -U "crate::application::\\{[^}]*\\b(${task_upper_modules})\\b" \
    "$app_src/task" -g '*.rs' -g '!**/tests/**' ||
  ! inherent_calls_into "$app_src" task "$task_upper_modules" \
    "$task_upper_store_allowed" '' ||
  ! super_chains_into "$app_src" task "$task_upper_modules"; then
  echo "orbit-core task must reach job, review, distributed, pull_drain, admission, sweep_filing, workspace_auto, codebase, resident, experiment, notice, evolution and agent_corpus through its task ports, and must not name tag_instructions, search or backlog_forecast"
  fail=1
fi

# [ORB-17937] `job` reaches the distributed drain and pull settlement only
# through the ports it owns (`JobDrainPort`, `JobPullSettlementPort`), which
# `distributed` and `pull_drain` implement on `OrbitRuntime`; the shapes they
# share (`DrainClaimedLeaf`, `HostProviderHold`) are contracts. A path, a
# grouped tree, a `super::` chain out of `job` into either, or a call to an
# inherent method either defines (derived from their `impl OrbitRuntime`
# blocks [ORB-22003]) is a reintroduced edge.
job_drain_modules='distributed|pull_drain'
if dependency_matches -n "crate::application::(${job_drain_modules})\\b" \
  "$app_src/job" -g '*.rs' -g '!**/tests/**' ||
  dependency_matches -n -U "crate::application::\\{[^}]*\\b(${job_drain_modules})\\b" \
    "$app_src/job" -g '*.rs' -g '!**/tests/**' ||
  ! inherent_calls_into "$app_src" job "$job_drain_modules" '' '' ||
  ! super_chains_into "$app_src" job "$job_drain_modules"; then
  echo "orbit-core job must reach distributed and pull_drain through job::JobDrainPort and job::JobPullSettlementPort"
  fail=1
fi

# [ORB-17938] `job` reaches review admission, state automation, backlog
# admission and the auto drain's concurrency mode and leaf settlement only
# through the ports it owns (`ReviewAdmissionHook`, `JobAutomationPort`,
# `JobAdmissionPort`, `JobAutoConcurrencyPort`, `JobDrainLeafPort`
# [ORB-23198]), which `review`, `automation`, `admission` and
# `workspace_auto` implement on `OrbitRuntime`; the job name and filing tags
# it shares with `review` and `sweep_filing` are contracts. A path, a grouped
# tree, a `super::` chain out of `job` into any of them, or a call to an
# inherent method they define (derived from their `impl OrbitRuntime`
# blocks [ORB-22003]) is a reintroduced edge. Their free functions are
# reachable only by the paths these checks refuse. `experiment` sits above
# `job` [ORB-23015]: crew admission, fresh claim routing, claimed-leaf crew
# installation and claimed-leaf capture validation reach it only through
# `JobExperimentPort`, and the pool they read is the
# `contracts::ExperimentCrewPool` value.
job_upper_modules='admission|experiment|automation|backlog_forecast|review|sweep_filing|workspace_auto'
if dependency_matches -n "crate::application::(${job_upper_modules})\\b" \
  "$app_src/job" -g '*.rs' -g '!**/tests/**' ||
  dependency_matches -n -U "crate::application::\\{[^}]*\\b(${job_upper_modules})\\b" \
    "$app_src/job" -g '*.rs' -g '!**/tests/**' ||
  ! inherent_calls_into "$app_src" job "$job_upper_modules" '' '' ||
  ! super_chains_into "$app_src" job "$job_upper_modules"; then
  echo "orbit-core job must reach review, automation, admission, workspace_auto and experiment through its job ports"
  fail=1
fi

# [ORB-17939] Above `job`, each module names only the modules below it in
# `automation < review < distributed < pull_drain < admission <
# workspace_auto < backlog_forecast < sweep_filing`. The cut edges go through ports the lower
# module owns: `automation::AutomationReviewPort` (before-PR coverage and the
# reviewer pools, implemented by `review`), `review::ReviewDistributedPort`
# (the owner's ship contract and machine) and `distributed::
# DistributedAdmissionPort` (the executor census); automation's pilot
# reservations and review's handoff console take the owner's commit boundary
# and completion authority through `job::JobDrainPort`, and admission's
# backlog listing takes the live leaves' tasks through
# `task::TaskWorkspaceAutoPort`. The distributed mutation gate is a contract.
# A path, a grouped tree, a `super::` chain into a higher module, or a call
# to the inherent methods and functions those ports wrap is a reintroduced
# edge.

# Fail when production code under application module `$1` names one of the
# `|`-separated higher modules `$2` or calls one of the `|`-separated inherent
# methods and functions `$3` (empty for none).
guard_upper_layer() {
  local module="$1" higher="$2" calls="$3"
  if dependency_matches -n "crate::application::(${higher})\\b" \
    "$app_src/$module" -g '*.rs' -g '!**/tests/**' ||
    dependency_matches -n -U "crate::application::\\{[^}]*\\b(${higher})\\b" \
      "$app_src/$module" -g '*.rs' -g '!**/tests/**' ||
    { [[ -n "$calls" ]] && dependency_matches -n "\\b(${calls})\\s*(::<[^>]*>)?\\s*\\(" \
      "$app_src/$module" -g '*.rs' -g '!**/tests/**'; } ||
    ! super_chains_into "$app_src" "$module" "$higher"; then
    echo "orbit-core ${module} must reach ${higher//|/, } only through ports the lower module owns"
    fail=1
  fi
}
guard_upper_layer automation 'review|distributed|pull_drain|admission|workspace_auto|backlog_forecast|sweep_filing' \
  'configured_pool_members|review_pool_by_os|select_review_crew|review_crew_candidates|admission_boundary'
guard_upper_layer review 'distributed|pull_drain|admission|workspace_auto|backlog_forecast|sweep_filing' \
  'admission_boundary|owner_completion_authority|owner_ship_contract|distributed_owner_machine_id'
guard_upper_layer distributed 'pull_drain|admission|workspace_auto|backlog_forecast|sweep_filing' 'executor_census'
guard_upper_layer pull_drain 'admission|workspace_auto|backlog_forecast|sweep_filing' ''
guard_upper_layer admission 'workspace_auto|backlog_forecast|sweep_filing' 'read_live_leaf_runs'
guard_upper_layer workspace_auto 'backlog_forecast|sweep_filing' ''
guard_upper_layer backlog_forecast 'sweep_filing' ''

# [ORB-23334] `auto_tasks` sits above `automation`: its scheduler evaluates
# delivery and conditions through automation, its listing projection reads
# automation inspection and its enable path uses automation's preview.
# Automation reads the catalog, validated definitions, discovery paths,
# accepted history, open-instance dedupe and template/mint parameters only
# through `automation::AutomationAutoTaskPort`, which `auto_tasks` implements on
# `OrbitRuntime` in `auto_tasks/scheduler.rs`. A path, a grouped tree, a
# `super::` chain into `auto_tasks`, or a call to an inherent method it defines
# (names derived, UFCS included, so the port methods keep distinct names) is a
# reintroduced edge. The downward edges from `auto_tasks` stay legal.
if dependency_matches -n 'crate::application::auto_tasks\b' \
  "$app_src/automation" -g '*.rs' -g '!**/tests/**' ||
  dependency_matches -n -U 'crate::application::\{[^}]*\bauto_tasks\b' \
    "$app_src/automation" -g '*.rs' -g '!**/tests/**' ||
  ! inherent_calls_into "$app_src" automation auto_tasks '' '' ||
  ! super_chains_into "$app_src" automation auto_tasks; then
  echo "orbit-core automation must reach auto_tasks through automation::AutomationAutoTaskPort and must not call its inherent methods"
  fail=1
fi

# [ORB-22416] `experiment` sits outside the order, above `distributed`: its
# readout reads the claim receipt `distributed` owns, so the owner's capture of a
# claimed task's experiments reaches `distributed` only through
# `distributed::DistributedExperimentPort`, which `experiment` implements on
# `OrbitRuntime`. A path, a grouped tree, a `super::` chain into `experiment`,
# or a call to an inherent method it defines (names derived, so the port method
# keeps a distinct name) is a reintroduced edge. The reverse edge, `experiment`
# reading the claim receipt, stays legal.
if dependency_matches -n 'crate::application::experiment\b' \
  "$app_src/distributed" -g '*.rs' -g '!**/tests/**' ||
  dependency_matches -n -U 'crate::application::\{[^}]*\bexperiment\b' \
    "$app_src/distributed" -g '*.rs' -g '!**/tests/**' ||
  ! inherent_calls_into "$app_src" distributed experiment '' '' ||
  ! super_chains_into "$app_src" distributed experiment; then
  echo "orbit-core distributed must reach experiment through distributed::DistributedExperimentPort and must not call its inherent methods"
  fail=1
fi

# Experiment readout consumes agent facts; agents must not reach back into
# experiment by path or by an inherent OrbitRuntime method it owns.
if dependency_matches -n 'crate::application::experiment\b' \
  "$app_src/agents" -g '*.rs' -g '!**/tests/**' ||
  dependency_matches -n -U 'crate::application::\{[^}]*\bexperiment\b' \
    "$app_src/agents" -g '*.rs' -g '!**/tests/**' ||
  ! inherent_calls_into "$app_src" agents experiment '' '' ||
  ! super_chains_into "$app_src" agents experiment; then
  echo "orbit-core agents must not name experiment or call its inherent methods"
  fail=1
fi

# [ORB-23019] `experiment` sits above `tag_instructions`: its overlays read
# `LazyRevision`, `TagInstructionFailure` and `read_blob_at`, and call
# `claim_base_revision`. Tag instructions reach overlay policy only through
# `tag_instructions::TagInstructionExperimentPort`, which `experiment`
# implements on `OrbitRuntime`. `tag_instructions` is one file, so the caller
# scan names that file. A path, a grouped tree, a `super::` chain into
# `experiment`, or a call to an inherent method it defines (names derived, so
# the port method keeps a distinct name) is a reintroduced edge. The reverse
# edge, experiment reading tag-instruction helpers, stays legal.
if [[ ! -f "$app_src/tag_instructions.rs" ]]; then
  echo "dependency scan path missing: $app_src/tag_instructions.rs" >&2
  exit 1
fi
if dependency_matches -n 'crate::application::experiment\b' \
  "$app_src/tag_instructions.rs" ||
  dependency_matches -n -U 'crate::application::\{[^}]*\bexperiment\b' \
    "$app_src/tag_instructions.rs" ||
  ! inherent_calls_into "$app_src" tag_instructions.rs experiment '' '' ||
  ! super_chains_into "$app_src" tag_instructions.rs experiment; then
  echo "orbit-core tag_instructions must reach experiment through tag_instructions::TagInstructionExperimentPort and must not call its inherent methods"
  fail=1
fi

# [ORB-22708] Landing sits above distributed. Task
# owns the pure handoff-to-claim read; review records authority and reaches
# dispatch only through its own ReviewLandingDispatchPort. Refuse paths,
# grouped trees, relative chains and calls into landing's inherent methods.
# These exact lines read the task store, whose method shares a runtime name.
landing_store_reads_allowed='^task/external_merge/candidate\.rs:[0-9]+:\s+\.landing_attempts\(\)$|^task/external_merge/rescued\.rs:[0-9]+:\s+if tasks\.landing_attempts\(\)\?\.iter\(\)\.any\(\|attempt\| \{$|^review/handoff/console\.rs:[0-9]+:\s+self\.stores\(\)\.tasks\(\)\.landing_attempts\(\)\?$'
for lower_module in task job review distributed; do
  if dependency_matches -n 'crate::application::landing\b' \
    "$app_src/$lower_module" -g '*.rs' -g '!**/tests/**' ||
    dependency_matches -n -U 'crate::application::\{[^}]*\blanding\b' \
      "$app_src/$lower_module" -g '*.rs' -g '!**/tests/**' ||
    ! inherent_calls_into "$app_src" "$lower_module" landing "$landing_store_reads_allowed" '' ||
    ! super_chains_into "$app_src" "$lower_module" landing; then
    echo "orbit-core ${lower_module} must not name landing or call its inherent methods; task owns handoff claim resolution, and review dispatches through review::ReviewLandingDispatchPort"
    fail=1
  fi
done

# [ORB-22623] `routines` (the sweep clock) sits outside the order, above
# `automation` and `auto_tasks`: it calls automation's evaluation and the
# auto-task scheduler, and reaches `review`, `pull_drain` and `backlog_forecast`.
# The clock settings and schedule display state those two read live in
# `contracts`. A path, a grouped tree, a `super::` chain into `routines`, or a
# call to an inherent method it defines (names derived) is a reintroduced edge.
# Resident reads the selected-checkout cycle switch through its own
# ResidentRoutineStatusPort, implemented by routines [ORB-22956], so the
# routines -> automation -> resident path has no back-edge into routines.
for lower_module in automation auto_tasks resident; do
  if dependency_matches -n 'crate::application::routines\b' \
    "$app_src/$lower_module" -g '*.rs' -g '!**/tests/**' ||
    dependency_matches -n -U 'crate::application::\{[^}]*\broutines\b' \
      "$app_src/$lower_module" -g '*.rs' -g '!**/tests/**' ||
    ! inherent_calls_into "$app_src" "$lower_module" routines '' '' ||
    ! super_chains_into "$app_src" "$lower_module" routines; then
    echo "orbit-core ${lower_module} must not name routines or call its inherent methods; use lower-owned contracts or resident::ResidentRoutineStatusPort"
    fail=1
  fi
done

# [ORB-22287] The forecast has no ports, so nothing below it may call its
# inherent methods (names derived); `task` and `job` are covered above, through
# their upper-module lists.
for lower_module in automation review distributed pull_drain admission workspace_auto; do
  if ! inherent_calls_into "$app_src" "$lower_module" backlog_forecast '' ''; then
    echo "orbit-core ${lower_module} must not call backlog_forecast's inherent methods"
    fail=1
  fi
done

# Selective-review retirement removed review's risk-model freeze and its
# codebase dependency. Keep that direction closed: review has no codebase
# exception, including calls indexed from codebase's inherent runtime methods.
if ! super_chains_into "$app_src" review codebase ||
  ! inherent_calls_into "$app_src" review codebase '' ''; then
  echo "orbit-core review must not name codebase or call its inherent methods"
  fail=1
fi

# [ORB-22287] `codebase` sits outside the order. The forecast's one authorized
# edge into it is `runtime.task_riskiness_model()` in `assembly.rs`, whose
# returned model answers `forecast_regression_prior`. Any other path or inherent
# call into `codebase`, or any `codebase` path or call back into the forecast,
# is a new edge.
forecast_codebase_allowed='^backlog_forecast/assembly\.rs:[0-9]+:\s+let risk_model = runtime\.task_riskiness_model\(\)\?;$'
if dependency_matches -n 'crate::application::codebase\b' \
  "$app_src/backlog_forecast" -g '*.rs' -g '!**/tests/**' ||
  dependency_matches -n -U 'crate::application::\{[^}]*\bcodebase\b' \
    "$app_src/backlog_forecast" -g '*.rs' -g '!**/tests/**' ||
  ! inherent_calls_into "$app_src" backlog_forecast codebase "$forecast_codebase_allowed" '' ||
  ! super_chains_into "$app_src" backlog_forecast codebase; then
  echo "orbit-core backlog_forecast may reach codebase only through runtime.task_riskiness_model() in assembly.rs"
  fail=1
fi
if dependency_matches -n 'crate::application::backlog_forecast\b' \
  "$app_src/codebase" -g '*.rs' -g '!**/tests/**' ||
  dependency_matches -n -U 'crate::application::\{[^}]*\bbacklog_forecast\b' \
    "$app_src/codebase" -g '*.rs' -g '!**/tests/**' ||
  ! inherent_calls_into "$app_src" codebase backlog_forecast '' '' ||
  ! super_chains_into "$app_src" codebase backlog_forecast; then
  echo "orbit-core codebase must not name backlog_forecast or call its inherent methods"
  fail=1
fi

shared_contracts='DISTRIBUTED_MUTATION_ENTRY_POINTS_ENABLED|PULL_DRAIN_JOB|DrainClaimedLeaf|ClaimedLeafStage|HostProviderHold|ESCALATION_TRIAGE_JOB|FINDING_VERIFICATION_JOB|DrainEntryPoint|PullSettlementEntry|PullSettlementOutcome|SUPERSEDED_BY_SOURCE|PilotFingerprints|review_layer_json|CreationCrewAssignment|CrewCandidate|InspectionPool|PURPOSE_SOURCE_PREFIX|PROVIDER_LIMIT_REASSIGNMENT_PREFIX|auto_task_minted|automatic_crew_source|RetryKey|LOCAL_ROUTE_JOB|CI_FAILURE_TAG|DEPENDABOT_TAG|CODE_SCANNING_TAG|RECONCILIATION_FOLLOW_UP_TAG_PREFIX|PublishedDelivery|PrecedentAssistState|PrecedentAssistOutcomeSource|PrecedentAssistOutcome|PrecedentAssistDemotion|PrecedentAssistEnd|PrecedentAssistEnablement|ClockSettings|DEFAULT_CLOCK_CADENCE_SECONDS|LAUNCHD_LABEL|SYSTEMD_UNIT|load_clock_settings|save_clock_settings|ScheduleDisplayState'
if dependency_matches -n "\\b(const|enum|struct|fn) (${shared_contracts})\\b" \
  "$repo_root/crates/orbit-core/src" -g '*.rs' -g "!**/application/contracts/**" ||
  dependency_matches -n "\\b(${app_modules})(::[a-z_]+)*::(${shared_contracts})\\b" \
    "$repo_root/crates" -g '*.rs' ||
  dependency_matches -n -U "\\b(${app_modules})(::[a-z_]+)*::\\{[^}]*\\b(${shared_contracts})\\b" \
    "$repo_root/crates" -g '*.rs'; then
  echo "shared application contracts are defined and imported only at application::contracts"
  fail=1
fi

if dependency_matches -n 'ResolvedConfig::load|ConfigRoots::' \
  "$repo_root/crates/orbit-runtime/src" \
  -g '*.rs' -g '!**/tests/**'; then
  echo "orbit-runtime must consume resolved config supplied by composition"
  fail=1
fi

# orbit-store is intentionally one crate, but its internal persistence graph
# is directional: contracts and shared fs mechanics are leaves; each driver is
# isolated; only repositories, workflows, and composition may join drivers.
store_src="$repo_root/crates/orbit-store/src"
if ! super_chains_into "$store_src" contracts 'crate::driver|crate::repository|crate::workflow' named-path ||
  dependency_matches -n '\brusqlite\b' \
    "$store_src/contracts" -g '*.rs' -g '!**/tests/**'; then
  echo "orbit-store contracts must not import persistence implementations"
  fail=1
fi

if ! super_chains_into "$store_src" driver/file 'crate::driver::sqlite|crate::repository|crate::workflow|crate::Store|crate::StoreTx' named-path; then
  echo "orbit-store file driver must not import SQLite, repositories, or workflows"
  fail=1
fi

if ! super_chains_into "$store_src" driver/sqlite 'crate::driver::file|crate::repository|crate::workflow' named-path; then
  echo "orbit-store SQLite driver must not import file drivers, repositories, or workflows"
  fail=1
fi

if ! super_chains_into "$store_src" fs 'crate::driver|crate::repository|crate::workflow' named-path; then
  echo "orbit-store filesystem primitives must not import drivers or orchestration"
  fail=1
fi

for retired_path in \
  "$store_src/backend" \
  "$store_src/file" \
  "$store_src/sqlite" \
  "$store_src/state_io" \
  "$store_src/task_migration"; do
  if [[ -e "$retired_path" ]]; then
    echo "retired orbit-store ownership path still exists: $retired_path"
    fail=1
  fi
done

for retired_path in \
  "$repo_root/crates/orbit-core/src/command" \
  "$repo_root/crates/orbit-runtime/src/orbit_tool_host" \
  "$repo_root/crates/orbit-runtime/src/engine/runtime_host.rs"; do
  if [[ -e "$retired_path" ]]; then
    echo "retired runtime ownership path still exists: $retired_path"
    fail=1
  fi
done

# State consumers share Automation's evaluator and Store checkpoint owner.
# Core may gather authoritative facts, but must not grow a second checkpoint
# implementation or hashing/coverage acceptance path [ORB-11331].
if dependency_matches -n 'automation_commit\(|Sha256' \
  "$repo_root/crates/orbit-core/src/application/automation" \
  --glob '*.rs' --glob '!**/tests/**'; then
  echo "Core automation must use the shared scheduling/checkpoint contract"
  fail=1
fi

# Before-PR review [ORB-11333] composes authority and Git evidence in Core and
# persists through Store; the coverage acceptance and exclusion rules, task
# meaning digests, and landing classification stay in Automation's `review`
# module. Core's review module must not grow its own digest, validator,
# checkpoint, or evaluator.
if dependency_matches -n 'automation_commit\(|Sha256|fn evaluate\b|fn exclusion\b|fn certificate_acceptable\b|fn task_meaning_digest\b|definition_epoch' \
  "$repo_root/crates/orbit-core/src/application/review" \
  --glob '*.rs' --glob '!**/tests/**'; then
  echo "Core review must use orbit-automation's shared coverage rules and Store persistence"
  fail=1
fi

if dependency_matches -n 'ReviewStoreBackend|review_certificate_record|review_reserve' \
  "$repo_root/crates/orbit-automation/src" \
  --glob '*.rs' --glob '!**/tests/**'; then
  echo "orbit-automation must not persist review evidence; Store owns ledgers and certificates"
  fail=1
fi

# [ORB-24097] `vcs::conventions` is the lower owner of the shared task
# projections; it must not name `pr` again, and the named commit/handoff
# consumers must read the projections from conventions, not PR rendering.
# Unrelated PR operations in those consumers stay legal.
vcs_src="$repo_root/crates/orbit-engine/src/executor/automation/vcs"
if dependency_matches -n '\bpr\b' "$vcs_src/conventions.rs"; then
  echo "orbit-engine vcs conventions must not reference pr; it owns the shared task projections"
  fail=1
fi
projection_names='meaningful_execution_summary|render_acceptance_criteria|task_url|is_placeholder_execution_summary'
for consumer in handoff.rs commit/actions.rs commit/summary.rs commit/already_resolved.rs; do
  if dependency_matches -n "\b(pr|body)::(${projection_names})\b|\bpr::\*|\bpr::body::\b" "$vcs_src/$consumer" ||
    dependency_matches -n -U "\bpr::\{[^}]*\b(${projection_names})\b" "$vcs_src/$consumer" ||
    dependency_matches -n -U "\bpr::body::\{[^}]*\b(${projection_names})\b" "$vcs_src/$consumer"; then
    echo "orbit-engine vcs ${consumer} must read task projections from conventions, not pr"
    fail=1
  fi
done

# Shared hosted-check handling sits below landing. Scan Rust identifiers and
# use trees so the word landing in unchanged evidence and docs is not an edge.
# PR, base and claim must consume checks directly, never through landing.
if ! super_chains_into "$vcs_src" checks landing named-path; then
  echo "orbit-engine vcs checks must not reference landing"
  fail=1
fi
for consumer in pr base claim; do
  if ! super_chains_into "$vcs_src" "$consumer" 'landing::checks' named-path; then
    echo "orbit-engine vcs ${consumer} must consume checks directly, not landing::checks"
    fail=1
  fi
done

# [ORB-25311] `vcs::pr_answers` owns the PR read model and sits below checks,
# pr, landing, base, push and claim. A Rust path segment naming one of those
# modules is an edge; a bare identifier or a string (a pull-request number,
# the `checks` field, a `base` pin) is not. checks, base, push, claim
# publication and candidate_resume read the model from pr_answers and must not
# path-name `pr` [ORB-25493]. The order is `pr_answers < candidate_resume < pr`.
# Print every such path as `<file>:<line>:<text>` and exit nonzero when one
# is found. A missing scan root fails closed.
module_paths_into() {
  python3 - "$1" "$2" "$3" <<'PY'
import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1])
module = sys.argv[2]
forbidden = set(sys.argv[3].split("|"))
target = root / module
if not target.exists():
    print(f"dependency scan path missing: {target}", file=sys.stderr)
    sys.exit(1)
token = re.compile(
    r"(?P<line>//[^\n]*)|(?P<block>/\*)"
    r'|(?P<raw>(?:br|cr|r)(?P<hashes>\#*)")'
    r'|(?P<literal>(?:b|c)?"(?:\\[\s\S]|[^"\\])*"'
    r"|b?'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F_]+\}|[^\n])|[^'\\\n])')"
    r"|::|r\#[^\W\d]\w*|[^\W\d]\w*|\S"
)
block_delimiter = re.compile(r"/\*|\*/")


def segment_name(token_text):
    # [ORB-26107] `r#builtin` names the module `builtin`. Keywords keep their
    # raw spelling, so `r#use` is never read as a `use` item.
    return token_text.removeprefix("r#")


def code_tokens(source):
    index = 0
    while match := token.search(source, index):
        index = match.end()
        if match.lastgroup in ("line", "literal"):
            continue
        if match.group("block") is not None:
            depth = 1
            while depth and (delimiter := block_delimiter.search(source, index)):
                depth += 1 if delimiter.group() == "/*" else -1
                index = delimiter.end()
            if depth:
                return
        elif match.group("raw") is not None:
            closing = '"' + match.group("hashes")
            end = source.find(closing, index)
            if end == -1:
                return
            index = end + len(closing)
        else:
            yield match.group(), match.start()


def use_tree(tokens, index, prefix=()):
    paths = []
    while index + 1 < len(tokens) and tokens[index + 1][0] == "::":
        prefix += (segment_name(tokens[index][0]),)
        index += 2
    if index < len(tokens) and tokens[index][0] == "{":
        index += 1
        while index < len(tokens) and tokens[index][0] != "}":
            member_paths, index = use_tree(tokens, index, prefix)
            paths.extend(member_paths)
            if index < len(tokens) and tokens[index][0] == ",":
                index += 1
            else:
                break
        if index < len(tokens) and tokens[index][0] == "}":
            index += 1
    elif index < len(tokens):
        paths.append(prefix + (segment_name(tokens[index][0]),))
        index += 1
        if index < len(tokens) and tokens[index][0] == "as":
            index += 2
    return paths, index


failed = False
paths = [target] if target.is_file() else sorted(target.rglob("*.rs"))
for path in paths:
    rel = path.relative_to(root)
    if "tests" in rel.parts[:-1] or rel.name == "tests.rs":
        continue
    source = path.read_text()
    tokens = list(code_tokens(source))
    reported = set()

    def report(start):
        global failed
        number = source.count("\n", 0, start) + 1
        if number in reported:
            return
        reported.add(number)
        print(f"{path}:{number}:{source.splitlines()[number - 1]}")
        failed = True

    for index, (name, start) in enumerate(tokens):
        if name != "use":
            continue
        imported, _ = use_tree(tokens, index + 1)
        if any(segment in forbidden for segments in imported for segment in segments):
            report(start)
    for index, (name, start) in enumerate(tokens):
        if segment_name(name) not in forbidden:
            continue
        previous = tokens[index - 1][0] if index else ""
        following = tokens[index + 1][0] if index + 1 < len(tokens) else ""
        if previous == "::" or following == "::":
            report(start)
sys.exit(1 if failed else 0)
PY
}

if ! module_paths_into "$vcs_src" pr_answers 'pr|checks|landing|base|push|claim'; then
  echo "orbit-engine vcs pr_answers must not name pr, checks, landing, base, push or claim"
  fail=1
fi
for consumer in checks base push.rs claim/published.rs candidate_resume; do
  if ! module_paths_into "$vcs_src" "$consumer" pr; then
    echo "orbit-engine vcs ${consumer} must not name pr; read the PR model from pr_answers"
    fail=1
  fi
done

# The RuntimeHost port owns its dispatch contracts below activity execution.
# Include test-support and test fixtures; paths into orbit-types' workflow
# modules remain legal. The contract leaf must name no engine module at all.
engine_src="$repo_root/crates/orbit-engine/src"
if [[ ! -d "$engine_src/context" ]]; then
  echo "dependency scan path missing: $engine_src/context" >&2
  exit 1
fi
if ! super_chains_into "$engine_src" context activity_job crate-root include-tests; then
  echo "orbit-engine context must not name activity_job; RuntimeHost owns its dispatch contracts"
  fail=1
fi
if ! module_paths_into "$engine_src/context" dispatch_contract.rs crate; then
  echo "orbit-engine dispatch contracts must not name any crate:: module"
  fail=1
fi

# [ORB-26097] orbit-engine: `sandbox_spawn` sits below both
# `executor/automation` and `activity_job`, and `executor/automation` sits below
# `activity_job`. `sandbox_spawn.rs` may name `crate::context` (for
# `ResolvedSandbox`) and no other crate-root module or crate-root re-export.
# `executor/automation` must not name `activity_job` or any crate-root re-export
# of an `activity_job` item. Children of `activity_job` must read
# `DECLARED_FAILURE_EVIDENCE_KEY` from `context::dispatch_contract`, not the
# `activity_job` root.
if [[ ! -d "$engine_src/executor/automation" ]]; then
  echo "dependency scan path missing: $engine_src/executor/automation" >&2
  exit 1
fi
if [[ ! -f "$engine_src/sandbox_spawn.rs" ]]; then
  echo "dependency scan path missing: $engine_src/sandbox_spawn.rs" >&2
  exit 1
fi
for child in cli_runner job_executor; do
  if [[ ! -d "$engine_src/activity_job/$child" ]]; then
    echo "dependency scan path missing: $engine_src/activity_job/$child" >&2
    exit 1
  fi
done
engine_activity_job_names='activity_job|DispatchError|DispatchInvocationTrace|DispatchOutcome|JobOutcome|LinuxRuntimeWriteAuthority|ResolvedCliExecutor|ResolvedSandbox|ResolvedShellExecutor|SandboxMask|V2AuditWriter|V2DispatchInput|V2SqliteSink|dispatch_error_to_orbit|dispatch_v2_activity|execute_job_with_resume|load_activity_asset|resolve_crew_settings|resolve_job_catalog_refs_for_execution|resolve_job_catalog_refs_with_scripts_for_execution|validate_job|validate_job_deterministic_actions'
if ! super_chains_into "$engine_src" executor/automation "$engine_activity_job_names" crate-root; then
  echo "orbit-engine executor/automation must not name activity_job or its crate-root re-exports"
  fail=1
fi
engine_spawn_forbidden="$engine_activity_job_names|test_support|ci_log_signature|ci_run_event|execute_deterministic_action|normalize_review_decision|observe_accepted_published_handoff|observe_candidate|observe_no_diff_candidate|observe_published_candidate|verify_handoff_pull_request_head|BaseSyncMode|landing_wait_gate_cleared|review_gate|run_private_vcs_operation|BaselineHoldStatus|CheckState|ClaimedKnownRed|KnownRedUnconfirmed|PullRequestStatus|WORKTREE_IDENTITY_INPUT_KEYS|WorktreeGcOptions|WorktreeGcRecovery|WorktreeGcResult|WorktreeReclaimReport|baseline_hold_status|classify_rollup_context|collect_worktrees|compose_squash_merge_message|fetch_remote_base|judge_light_delivery|judge_light_delivery_with_key|recorded_baseline_hold_status|review_skip_decision|run_deferral_probe|run_review_skip_guard|run_worktree_has_reclaim_output|run_worktree_paths|validate_claim_new_paths|verify_claimed_known_red|verify_claimed_known_red_base|provider_supports_continuity|candidate_commit_completion_hold|candidate_completion_hold"
for entry in "$engine_src"/*; do
  module="$(basename "$entry")"
  module="${module%.rs}"
  case "$module" in
    context | lib | sandbox_spawn | tests) continue ;;
  esac
  if [[ -d "$entry" || "$entry" == *.rs ]]; then
    engine_spawn_forbidden="${engine_spawn_forbidden}|${module}"
  fi
done
if ! super_chains_into "$engine_src" sandbox_spawn.rs "$engine_spawn_forbidden" crate-root; then
  echo "orbit-engine sandbox_spawn must not name any crate:: module except context"
  fail=1
fi
if ! super_chains_into "$engine_src/activity_job" "" DECLARED_FAILURE_EVIDENCE_KEY; then
  echo "orbit-engine activity_job children must read DECLARED_FAILURE_EVIDENCE_KEY from context::dispatch_contract, not the activity_job root"
  fail=1
fi

# [ORB-24781] `job::run` (a run record's lifecycle: queries, cancellation,
# reconciliation and drain controls) sits below `job::pipeline` (submission,
# admission and worker execution). Run owns its audit trail (`audit_record`),
# worker-exit evidence and scope reads (`worker_exit`, `worker_scope`) and
# definition reads (`definition`); pipeline imports those and never the
# reverse. A path, a grouped tree or a `super::` chain from production code
# under `job/run` into `pipeline`, or a call to an inherent method an `impl
# OrbitRuntime` block under `job/pipeline` defines (derived, so a new method is
# covered without being listed), is a reintroduced edge. A grouped tree runs to
# the `;` ending its `use` item, not to its first `}`, so a nested group ahead of
# `pipeline` cannot hide it [ORB-24832].
run_src="$app_src/job/run"
if dependency_matches -n '\b(crate::application::job|super::super|super)::pipeline\b' \
  "$run_src" -g '*.rs' -g '!**/tests/**' ||
  dependency_matches -n -U '\b(crate::application::job|super::super|super)::\{[^;]*\bpipeline\b' \
    "$run_src" -g '*.rs' -g '!**/tests/**' ||
  dependency_matches -n -U 'crate::application::\{[^;]*\bjob::(pipeline\b|\{[^;]*\bpipeline\b)' \
    "$run_src" -g '*.rs' -g '!**/tests/**' ||
  ! inherent_calls_into "$app_src" job/run pipeline '' '' "$app_src/job"; then
  echo "orbit-core job::run must not reach job::pipeline; run owns its audit writer, worker-exit evidence and definition reads"
  fail=1
fi

# [ORB-25492] orbit-web: api and serve sit above state, projections, lease and
# the leaves, including runtime_memo. Production code in the lower modules
# names neither api nor serve.
# lease may name api::leases; it must not name serve. A missing scan root fails
# closed. Tests are skipped inside module_paths_into.
web_src="$repo_root/crates/orbit-web/src"
for module in \
  state why_stuck.rs projections.rs connect.rs host_tunnels.rs health.rs \
  assets.rs run_failure.rs shutdown.rs runtime_memo.rs; do
  if ! module_paths_into "$web_src" "$module" 'api|serve'; then
    echo "orbit-web ${module} must not name api or serve"
    fail=1
  fi
done
if ! module_paths_into "$web_src" lease serve; then
  echo "orbit-web lease must not name serve"
  fail=1
fi

# [ORB-26098] orbit-tools: `builtin` sits above every other crate-root module.
# Only `lib.rs` (its re-exports), `registry.rs` (through `register_builtins`)
# and `github_cli.rs` (the documented re-exports of builtin GitHub builders)
# name it; any other crate-root module's production code, `super::` chains
# and grouped trees included, must not. Shared pieces live in crate-root
# leaves (`gh_request`, `github_run_log_cache`, `task_guarded_selectors`).
# Modules are enumerated, so a new one is covered without being listed. A
# missing scan root fails closed. Tests are skipped inside module_paths_into.
tools_src="$repo_root/crates/orbit-tools/src"
if [[ ! -d "$tools_src/builtin" ]]; then
  echo "dependency scan path missing: $tools_src/builtin" >&2
  exit 1
fi
for entry in "$tools_src"/*; do
  module="$(basename "$entry")"
  case "$module" in
    builtin | lib.rs | registry.rs | github_cli.rs | tests) continue ;;
  esac
  if [[ -d "$entry" || "$module" == *.rs ]]; then
    if ! module_paths_into "$tools_src" "$module" builtin; then
      echo "orbit-tools ${module} must not name builtin; move the shared piece into a crate-root leaf"
      fail=1
    fi
  fi
done

if [[ "$fail" -ne 0 ]]; then
  exit 1
fi

echo "dependency direction guard passed"
