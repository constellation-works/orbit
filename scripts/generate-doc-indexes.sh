#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: scripts/generate-doc-indexes.sh [--check] [repo-root]" >&2
}

check_only=0
if [[ "${1:-}" == "--check" ]]; then
  check_only=1
  shift
fi

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  usage
  exit 0
fi

if [[ "$#" -gt 1 ]]; then
  usage
  exit 2
fi

repo_root="${1:-${ORBIT_REPO_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}}"
docs_dir="$repo_root/docs"
runbooks_dir="$docs_dir/runbooks"
design_dir="$docs_dir/design"
rca_dir="$docs_dir/rca"

if [[ ! -d "$runbooks_dir" || ! -d "$design_dir" || ! -d "$rca_dir" ]]; then
  echo "error: expected docs/runbooks, docs/design, and docs/rca under $repo_root" >&2
  exit 1
fi

export LC_ALL=C

frontmatter_value() {
  local file="$1"
  local key="$2"

  awk -v wanted="$key" '
    NR == 1 && $0 == "---" { in_frontmatter = 1; next }
    in_frontmatter && $0 == "---" { exit }
    in_frontmatter && index($0, wanted ":") == 1 {
      value = substr($0, length(wanted) + 2)
      sub(/^[[:space:]]+/, "", value)
      if (length(value) >= 2) {
        first = substr(value, 1, 1)
        last = substr(value, length(value), 1)
        if ((first == "\"" && last == "\"") || (first == "\047" && last == "\047")) {
          value = substr(value, 2, length(value) - 2)
        }
      }
      print value
      exit
    }
  ' "$file"
}

first_h1() {
  awk '/^# / { sub(/^# /, ""); print; exit }' "$1"
}

first_body_paragraph() {
  awk '
    NR == 1 && $0 == "---" { in_frontmatter = 1; next }
    in_frontmatter && $0 == "---" { in_frontmatter = 0; next }
    in_frontmatter { next }
    /^#/ { if (started) exit; next }
    /^>/ { if (started) exit; next }
    /^[[:space:]]*$/ { if (started) exit; next }
    {
      if (started) paragraph = paragraph " " $0
      else paragraph = $0
      started = 1
    }
    END {
      if (match(paragraph, /[.!?][[:space:]]/)) {
        paragraph = substr(paragraph, 1, RSTART)
      }
      print paragraph
    }
  ' "$1"
}

titleize_slug() {
  printf '%s\n' "$1" | awk '
    {
      gsub(/-/, " ")
      for (i = 1; i <= NF; i++) {
        $i = toupper(substr($i, 1, 1)) substr($i, 2)
        if ($i == "Mcp") $i = "MCP"
        if ($i == "Cli") $i = "CLI"
        if ($i == "Adr") $i = "ADR"
      }
      print
    }
  '
}

clean_design_title() {
  local title="$1"
  title="${title% — Overview}"
  title="${title% - Overview}"
  title="${title% — Decisions}"
  title="${title% - Decisions}"
  printf '%s\n' "$title"
}

markdown_escape() {
  local value="$1"
  value="$(printf '%s\n' "$value" | sed -E 's/\[([^][]+)\]\([^)]+\)/\1/g')"
  value="${value//|/\\|}"
  printf '%s' "$value"
}

is_versioned() {
  local file="$1"
  local relative="${file#"$repo_root/"}"

  git -C "$repo_root" ls-files --cached --others --exclude-standard --error-unmatch -- "$relative" >/dev/null 2>&1
}

design_entry_doc() {
  local directory="$1"
  local candidate

  if [[ -f "$directory/1_overview.md" ]]; then
    printf '%s\n' "$directory/1_overview.md"
    return
  fi

  candidate="$(find "$directory" -maxdepth 1 -type f -name '*.md' ! -name 'CONVENTIONS.md' | sort | sed -n '1p')"
  if [[ -z "$candidate" ]]; then
    echo "error: no top-level Markdown entry document in $directory" >&2
    return 1
  fi
  printf '%s\n' "$candidate"
}

render_runbook_rows() {
  local file
  local doc_type
  local summary
  local title
  local relative

  while IFS= read -r file; do
    if ! is_versioned "$file"; then
      continue
    fi

    doc_type="$(frontmatter_value "$file" type)"
    summary="$(frontmatter_value "$file" summary)"
    title="$(first_h1 "$file")"

    if [[ "$doc_type" != "runbook" || -z "$summary" || -z "$title" ]]; then
      echo "error: $file needs type: runbook, a summary, and an H1" >&2
      return 1
    fi

    relative="${file#"$docs_dir/"}"
    printf '| [%s](./%s) | %s |\n' \
      "$(markdown_escape "$title")" \
      "$relative" \
      "$(markdown_escape "$summary")"
  done < <(find "$runbooks_dir" -maxdepth 1 -type f -name '*.md' ! -name 'CONVENTIONS.md' | sort)
}

render_rca_rows() {
  local file
  local doc_type
  local summary
  local incident_date
  local title
  local relative

  while IFS= read -r file; do
    if ! is_versioned "$file"; then
      continue
    fi

    doc_type="$(frontmatter_value "$file" type)"
    summary="$(frontmatter_value "$file" summary)"
    incident_date="$(frontmatter_value "$file" incident_date)"
    title="$(first_h1 "$file")"

    if [[ "$doc_type" != "context" || -z "$summary" || -z "$incident_date" || -z "$title" ]]; then
      echo "error: $file needs type: context, a summary, an incident_date, and an H1" >&2
      return 1
    fi

    relative="${file#"$docs_dir/"}"
    printf '| [%s](./%s) | %s | %s |\n' \
      "$(markdown_escape "$title")" \
      "$relative" \
      "$(markdown_escape "$incident_date")" \
      "$(markdown_escape "$summary")"
  done < <(find "$rca_dir" -maxdepth 1 -type f -name '*.md' | sort -r)
}

render_design_rows() {
  local root="$1"
  local directory
  local entry
  local title
  local summary
  local status
  local owner
  local relative
  local body_summary

  while IFS= read -r directory; do
    entry="$(design_entry_doc "$directory")"
    if ! is_versioned "$entry"; then
      continue
    fi

    title="$(frontmatter_value "$entry" title)"
    summary="$(frontmatter_value "$entry" summary)"
    status="$(frontmatter_value "$entry" status)"
    owner="$(frontmatter_value "$entry" owner)"

    if [[ -z "$title" ]]; then
      if [[ "$(basename "$entry")" == "1_overview.md" ]]; then
        title="$(first_h1 "$entry")"
      else
        title="$(titleize_slug "$(basename "$directory")")"
      fi
    fi
    title="$(clean_design_title "$title")"

    if [[ -z "$summary" ]]; then
      summary="$(first_h1 "$entry")"
    fi
    case "$summary" in
      *" — Overview" | *" - Overview")
        body_summary="$(first_body_paragraph "$entry")"
        summary="${body_summary:-$summary}"
        ;;
    esac
    status="${status:-Draft}"
    owner="${owner:-—}"

    if [[ -z "$title" || -z "$summary" ]]; then
      echo "error: unable to derive title and summary from $entry" >&2
      return 1
    fi

    relative="${entry#"$docs_dir/"}"
    printf '| [%s](./%s) | %s | %s | %s |\n' \
      "$(markdown_escape "$title")" \
      "$relative" \
      "$(markdown_escape "$summary")" \
      "$(markdown_escape "$status")" \
      "$(markdown_escape "$owner")"
  done < <(find "$root" -mindepth 1 -maxdepth 1 -type d ! -name '_archive' ! -name '_templates' | sort)
}

render_index() {
  local output="$1"
  local last_validated

  last_validated="$(frontmatter_value "$docs_dir/INDEX.md" last_validated)"

  {
    printf '%s\n' '---'
    printf '%s\n' 'type: context'
    printf '%s\n' 'summary: Entry point for Orbit feature designs, operational runbooks, and incident root cause analyses.'
    if [[ -n "$last_validated" ]]; then
      printf 'last_validated: %s\n' "$last_validated"
    fi
    printf '%s\n' 'tags: [docs, index, design, operations, runbooks, incidents]'
    printf '%s\n' 'related_features: [orbit-docs, activity-job, auditability, routines]'
    printf '%s\n' 'related_artifacts: [ORB-10014, ORB-11416]'
    printf '%s\n' '---'
    printf '\n# Orbit Documentation Index\n\n'
    printf '%s\n' '<!-- Generated by scripts/generate-doc-indexes.sh; do not edit directly. -->'
    printf '\nUse this index to find active feature designs, day-2 operational procedures,\n'
    printf 'and incident root cause analyses.\n\n'
    printf '## Runbooks\n\n'
    printf 'Each runbook is scoped to one\n'
    printf 'operator goal or tightly coupled failure mode. Commands ported from the original\n'
    printf 'operations guide were verified against `orbit 0.13.0`; update the source runbook when\n'
    printf 'CLI behavior, state layout, or recovery semantics change.\n\n'
    printf '| Runbook | Purpose |\n'
    printf '| --- | --- |\n'
    render_runbook_rows
    printf '\nAuthoring rules live in [runbook conventions](./runbooks/CONVENTIONS.md).\n\n'
    printf 'These docs are host-agnostic. Host inventories, credentials, ports, enabled units,\n'
    printf 'and machine-specific sync topology belong in the operator\047s private knowledge base.\n\n'
    printf 'Related reference: [CONFIG.md](./CONFIG.md) documents configuration.\n\n'
    printf '## Root Cause Analyses\n\n'
    printf 'Incident reviews preserve impact, timelines, causal evidence, uncertainty, and follow-up tracking.\n\n'
    printf '| Incident | Date | Summary |\n'
    printf '| --- | --- | --- |\n'
    render_rca_rows
    printf '\n'
    printf '## Designs\n\n'
    printf 'Active feature designs live under `docs/design/<feature>/`. Each row links to\n'
    printf '`1_overview.md` when present, or to the folder\047s best available top-level entry doc.\n'
    printf 'Metadata comes from that document\047s frontmatter; folders without frontmatter use\n'
    printf 'a conservative title/status fallback.\n\n'
    printf '| Feature | Summary | Status | Lead |\n'
    printf '| --- | --- | --- | --- |\n'
    render_design_rows "$design_dir"
    printf '\nFolder layout, frontmatter, ADR, and ownership rules live in\n'
    printf '[design conventions](./design/CONVENTIONS.md).\n\n'
    printf 'Regenerate this file with `scripts/generate-doc-indexes.sh`.\n'
  } > "$output"
}

write_or_check() {
  local generated="$1"
  local target="$2"
  local display="${target#"$repo_root/"}"

  if cmp -s "$generated" "$target"; then
    if [[ "$check_only" -eq 1 ]]; then
      echo "verified $display"
    else
      echo "unchanged $display"
    fi
    return 0
  fi

  if [[ "$check_only" -eq 1 ]]; then
    echo "error: $display is stale; run scripts/generate-doc-indexes.sh" >&2
    return 1
  fi

  cp "$generated" "$target"
  echo "updated $display"
}

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/orbit-doc-indexes.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT

render_index "$tmp_dir/INDEX.md"

result=0
write_or_check "$tmp_dir/INDEX.md" "$docs_dir/INDEX.md" || result=1
exit "$result"
