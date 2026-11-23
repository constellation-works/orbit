# Configuration: pull requests, commits and branches

## Common settings: pull requests, commits and branches

| Key | Purpose |
|---|---|

| `pr.task_url_template` | URL template linking a task ID in PR descriptions. |
| `pr.delivery_authors` | Forge logins whose PRs count as Orbit-authored for closing them (Orbit closes a task's open delivery and `[BLOCKED]` PRs when it lands, is rejected or archived, moves back to backlog or someday, or a newer attempt opens its own PR) and for settling a task to done from its delivery PR merged outside Orbit. Empty uses the `gh` login on this machine. |
| `pr.title_template`, `pr.body_template` | Single-task PR title and body templates with `{{placeholder}}` values (`task_id`, `title`, `type`, `description`, `acceptance_criteria`, `execution_summary`, `validation`, `freshness`, `task_url`, `ext.<system>`). The body is `"repo"` (the repository's `pull_request_template.md`) or a repository-relative path, read at the delivery's base revision. An unknown placeholder fails the config load. A rendered body ends with an Orbit comment naming the task, so a template that omits `{{task_id}}` still identifies the delivery. |
| `pr.agent_sections`, `pr.required_sections`, `pr.missing_required_section` | Body headings the implementer fills through the `pr-sections.json` task artifact. An unfilled required section opens the PR as a draft (`draft`, default) or opens none (`refuse`), recorded in task history. |
| `pr.labels`, `pr.reviewers` | Labels and reviewers applied when Orbit opens a PR. Orbit opens PRs ready for review, as a draft only for a required-section gap; a draft is never merged automatically. |
| `pr.task_tag_labels`, `pr.task_type_labels` | Opt-in task labels after PR creation or reuse: exact, case-sensitive tag allowlist (array, default `[]`), and a table mapping `feature`, `bug`, `refactor`, `chore` to label names (default `{}`, e.g. `{ bug = "bug", feature = "enhancement" }`). Names must be non-empty and cannot start with `-`; unknown types are refused. Unioned over every delivered task; missing labels are created and existing labels accepted. Failures warn and are recorded in `pr_open.task_labels.applied` / `.failed` without failing the PR step. Empty settings add no commands; labels are never removed or synced after later task edits. |
| `commit.subject_template`, `commit.body_template` | Single-task commit templates; the subject must contain `[{{task_id}}]` and keeps a 72-character budget. |
| `branch.name_template` | Single-task delivery branch name (default `{{prefix}}/{{task_id}}-{{hash}}`); must contain `{{task_id}}` and `{{hash}}`. Names with `..` or a leading `-` are refused. |

## PR screenshot section

A single-task pull request body for a task tagged `ui` (matched ignoring
case) carries the optional `## Screenshots` table that Orbit builds from the
task's `screenshots/` artifacts. An untagged task gets no section: `pr_open`
reads no screenshots and pushes no evidence ref. This is fixed; the former
`pr.screenshot_section` key is ignored with a deprecation warning.

A bundle of several tasks keeps Orbit's batch body and never gets the
section, and a `body` supplied to `pr_open` is published as given, without
it. The `ui` tag's implementer routing is described in
[Tag instructions](configuration-tags.md#tag-instructions).

This rule governs only PR publication. Task-artifact evidence is a
separate duty: a diff that changes something a user sees still attaches its
screenshots as task artifacts when the workspace guide requires them,
whatever its tags. Name each artifact
`screenshots/<view>-<theme>-<width>[-before|-after].<ext>`. The width is the
last number in the name, the theme is the token before it, and the view is
the rest. A name without a suffix is an After shot; the Before shot is
optional.

Only PNG, JPEG, GIF and WebP files within the 1 MiB artifact limit
are published. Any other artifact under `screenshots/` is skipped with a
warning in the run's log.

When the section applies and no screenshot is publishable, the body carries
the neutral note `No screenshots attached.` That note is not a required-section
gap: `pr.missing_required_section` never drafts or refuses the PR for it.
Only `pr.required_sections` gaps do that.

### Verify the outcome

Check a real delivery, without opening a PR for the test:

- Before `pr_open`, `orbit task show <task-id> --fields artifacts` lists
  the task's `screenshots/` artifacts.
- After the PR opens, `gh pr view <pr-number> --json body --jq .body`
  should show the expected body:
  - a `## Screenshots` table with one row per view, width and theme (the
    theme is named only when the shots carry several), with Before and
    After columns, when publishable shots exist;
  - the `No screenshots attached.` note, when the section applies and no
    shot is publishable;
  - no `## Screenshots` section on an untagged task.
