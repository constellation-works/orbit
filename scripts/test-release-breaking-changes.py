#!/usr/bin/env python3
"""Drive release-breaking-changes.py over fixture repositories.

Each test builds a throwaway Git repository with miniature copies of the
surfaces the detector reads, commits fixture diffs under `[ORB-N]` subjects and
runs the script's CLI over the tagged range. Task tags and execution summaries
come from a stub `orbit` binary that serves a JSON fixture.
"""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import unittest

import self_test_hang

SCRIPT = Path(__file__).resolve().parent / "release-breaking-changes.py"

SCHEMA = "crates/orbit-store/src/driver/sqlite/migration/ledger/registry.rs"
LAYOUT = "crates/orbit-store/src/workflow/layout/registry.rs"
GOLDENS = "crates/orbit-cli/tests/help_goldens"
MCP = "crates/orbit-cli/tests/snapshots/mcp_tools_list.json"
TYPES = "crates/orbit-types/src/task/artifacts.rs"
KEYS = "crates/orbit-config/src/registry/keys.rs"
ACTIVITY = "crates/orbit-core/assets/activities/sweep.yaml"

ROOT_HELP = """\
orbit <VERSION>

Usage: orbit [OPTIONS] <COMMAND>

Knowledge:
  task        Create, update, and manage tasks
  search      Search tasks and frictions

Options:
      --format <MODE>
          Output format

          Possible values:
          - auto:   Decide from the sink
          - json:   A single JSON document

  -h, --help
          Print help
"""

TASK_HELP = """\
Create, update, and manage tasks

Usage: orbit task [OPTIONS] <COMMAND>

Commands:
  show  Show a task by ID
  list  List tasks

Options:
  -h, --help
          Print help
"""

SHOW_HELP = """\
Show a task by ID

Usage: orbit task show [OPTIONS] <ID>

Arguments:
  <ID>
          Task ID

Options:
      --view <VIEW>
          Projection

          [possible values: full, compact]

      --pretty
          Indent the JSON

      --format <MODE>
          Output format

          [possible values: auto, json]

  -h, --help
          Print help
"""

MCP_TOOLS = [
    {
        "name": "orbit_task_show",
        "description": "Show a task.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "id": {"type": "string", "description": "Task ID"},
                "view": {"type": "string", "enum": ["full", "compact"]},
                "fields": {"type": "array", "items": {"type": "string"}},
            },
            "required": ["id"],
        },
    },
    {
        "name": "orbit_task_list",
        "description": "List tasks.",
        "inputSchema": {"type": "object", "properties": {"limit": {"type": "integer"}}},
    },
]

TYPES_RS = """\
use serde::{Deserialize, Serialize};

/// The persisted task envelope.
/// Older binaries refuse unknown fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskEnvelopeV2 {
    pub id: String,
    /// Tags; see [`crate::task::Task::tags`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RelationTarget {
    Task { id: String },
    Commit {
        sha: String,
    },
}

/// A display row, never persisted with deny_unknown_fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRow {
    pub id: String,
}
"""

KEYS_RS = """\
pub(crate) const DEPRECATED_CONFIG_KEYS: &[(&str, &str)] = &[
    (
        "operation.review_minutes",
        "translated to review.minutes",
    ),
    ("review.before_pr", REVIEW_TIMING_NOTE),
];
"""

FAKE_ORBIT = """\
#!/usr/bin/env python3
import json, os, sys
tasks = json.load(open(os.environ["FAKE_ORBIT_TASKS"]))
args = sys.argv[1:]
if args[:2] != ["task", "show"] or "--json" not in args:
    sys.exit(2)
fields = args[args.index("--fields") + 1].split(",")
task = tasks.get(args[2])
if task is None:
    print(f"task {args[2]} not found", file=sys.stderr)
    sys.exit(1)
print(json.dumps({key: task.get(key) for key in fields}))
"""


class FixtureRepo:
    def __init__(self, root):
        self.root = Path(root)
        self.root.mkdir()
        self.env = dict(os.environ, GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1",
                        GIT_AUTHOR_NAME="fixture", GIT_AUTHOR_EMAIL="fixture@example.invalid",
                        GIT_COMMITTER_NAME="fixture", GIT_COMMITTER_EMAIL="fixture@example.invalid")
        self.git("init", "-q", "-b", "agent-main")
        self.write(SCHEMA, "pub const SUPPORTED_SCHEMA_VERSION: u32 = 67;\n")
        self.write(LAYOUT, "pub const SUPPORTED_LAYOUT_VERSION: u32 = 3;\n")
        self.write(f"{GOLDENS}/root.txt", ROOT_HELP)
        self.write(f"{GOLDENS}/task.txt", TASK_HELP)
        self.write(f"{GOLDENS}/task/show.txt", SHOW_HELP)
        self.write_mcp(MCP_TOOLS)
        self.write(TYPES, TYPES_RS)
        self.write(KEYS, KEYS_RS)
        self.write(ACTIVITY, "id: sweep\n")
        self.commit("chore: prepare v0.35.0 release [ORB-1]")
        self.git("tag", "v0.35.0")

    def git(self, *args):
        result = subprocess.run(["git", "-C", str(self.root), *args], env=self.env,
                                capture_output=True, text=True,
                                timeout=self_test_hang.HANG_CEILING_SECONDS)
        if result.returncode != 0:
            raise AssertionError(f"git {args}: {result.stderr}")
        return result.stdout

    def write(self, path, text):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)

    def read(self, path):
        return (self.root / path).read_text()

    def replace(self, path, old, new):
        text = self.read(path)
        if old not in text:
            raise AssertionError(f"{old!r} not in {path}")
        self.write(path, text.replace(old, new, 1))

    def write_mcp(self, tools):
        self.write(MCP, json.dumps(tools, indent=2) + "\n")

    def mcp(self):
        return json.loads(self.read(MCP))

    def remove(self, path):
        self.git("rm", "-q", path)

    def commit(self, subject):
        self.git("add", "-A")
        self.git("commit", "-q", "--allow-empty", "-m", subject)


class ReleaseBreakingChangesTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.repo = FixtureRepo(Path(self.tmp.name) / "repo")
        self.orbit = Path(self.tmp.name) / "orbit"
        self.orbit.write_text(FAKE_ORBIT)
        self.orbit.chmod(0o755)
        self.tasks = Path(self.tmp.name) / "tasks.json"
        self.tasks.write_text("{}")

    def survey(self, *extra, tasks=None):
        args = [sys.executable, str(SCRIPT), "--repo", str(self.repo.root), *extra]
        env = dict(self.repo.env)
        if tasks is None:
            args.append("--offline")
        else:
            self.tasks.write_text(json.dumps(tasks))
            env["FAKE_ORBIT_TASKS"] = str(self.tasks)
            args += ["--orbit-bin", str(self.orbit)]
        result = subprocess.run(args, env=env, capture_output=True, text=True,
                                timeout=self_test_hang.HANG_CEILING_SECONDS)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout

    def report(self, *extra, tasks=None):
        return json.loads(self.survey("--json", *extra, tasks=tasks))

    def signals(self, report):
        return {(s["task"], s["signal"], s["detail"]) for s in report["signals"]}

    def test_schema_bump_and_deny_unknown_field_decide_minor(self):
        # The v0.35.0..v0.36.0 shape: a SQLite migration and a new optional
        # field on the persisted task envelope, next to an additive MCP field.
        self.repo.replace(SCHEMA, "= 67;", "= 68;")
        self.repo.commit("feat: Count-based windows [ORB-19199] (#6539)")
        self.repo.replace(TYPES, "    pub tags: Vec<String>,\n",
                          "    pub tags: Vec<String>,\n"
                          "    #[serde(default, skip_serializing_if = \"Option::is_none\")]\n"
                          "    pub not_before: Option<String>,\n")
        tools = self.repo.mcp()
        tools[0]["inputSchema"]["properties"]["not_before"] = {"type": "string"}
        self.repo.write_mcp(tools)
        self.repo.commit("feat: Scheduled tasks, core: a `not_before` task field [ORB-19214] (#6544)")
        self.repo.write("docs/notes.md", "notes\n")
        self.repo.commit("docs: notes [ORB-19215] (#6545)")
        self.repo.git("tag", "v0.36.0")

        report = self.report("--range", "v0.35.0..v0.36.0")
        self.assertEqual(report["bump"], "minor")
        self.assertEqual(report["next_version"], "0.36.0")
        self.assertEqual(self.signals(report), {
            ("ORB-19199", "schema-version", "SUPPORTED_SCHEMA_VERSION 67 → 68"),
            ("ORB-19214", "persisted-field",
             "field `not_before` added to deny_unknown_fields type `TaskEnvelopeV2`"),
        })
        self.assertEqual(report["detected_untagged"], ["ORB-19199", "ORB-19214"])
        self.assertEqual(report["task_source"], "commit-subjects")
        text = self.survey("--range", "v0.35.0..v0.36.0")
        self.assertIn("Bump: minor → 0.36.0", text)
        self.assertIn("ORB-19214 [detected, untagged]", text)

    def test_unconditionally_skipped_fields_do_not_decide_minor(self):
        self.repo.replace(TYPES, "    pub tags: Vec<String>,\n",
                          "    pub tags: Vec<String>,\n"
                          "    #[serde(skip)]\n"
                          "    pub local_cache: Option<String>,\n"
                          "    #[serde(skip_serializing)]\n"
                          "    pub transient_state: Option<String>,\n")
        self.repo.commit("feat: Add local task state [ORB-52] (#1)")
        self.repo.git("tag", "v0.35.1")

        report = self.report("--range", "v0.35.0..v0.35.1")
        self.assertEqual(report["signals"], [])
        self.assertEqual(report["bump"], "patch")
        self.assertEqual(report["next_version"], "0.35.1")

    def test_quoted_serde_examples_do_not_hide_persisted_fields(self):
        additions = (
            '    #[doc = "Persisted field; unlike #[serde(skip)], it is on disk."]\n'
            "    pub documented_skip: Option<String>,\n"
            '    #[doc = "Persisted field; unlike #[serde(skip_serializing)], it is on disk."]\n'
            "    pub documented_skip_serializing: Option<String>,\n"
            '    #[serde(rename = "#[serde(skip)]")]\n'
            "    pub renamed_example: Option<String>,\n"
        )
        self.repo.replace(TYPES, "    pub tags: Vec<String>,\n",
                          "    pub tags: Vec<String>,\n" + additions)
        self.repo.commit("feat: Add persisted task envelope fields [ORB-2]")
        self.repo.git("tag", "v0.35.1")

        report = self.report("--range", "v0.35.0..v0.35.1")
        self.assertEqual(report["bump"], "minor")
        self.assertEqual(report["next_version"], "0.36.0")
        self.assertEqual(self.signals(report), {
            ("ORB-2", "persisted-field",
             "field `documented_skip` added to deny_unknown_fields type `TaskEnvelopeV2`"),
            ("ORB-2", "persisted-field",
             "field `documented_skip_serializing` added to deny_unknown_fields type `TaskEnvelopeV2`"),
            ("ORB-2", "persisted-field",
             "field `renamed_example` added to deny_unknown_fields type `TaskEnvelopeV2`"),
        })

    def test_rustfmt_normalized_quoted_serde_examples_decide_minor(self):
        additions = (
            '    #[doc = "Persisted field; unlike #[serde(skip)], it is on disk."]\n'
            "    pub documented_skip: Option<String>,\n"
            '    #[doc = "Persisted field; unlike #[serde(skip_serializing)], it is on disk."]\n'
            "    pub documented_skip_serializing: Option<String>,\n"
        )
        self.repo.replace(TYPES, "    pub tags: Vec<String>,\n",
                          "    pub tags: Vec<String>,\n" + additions)
        source = self.repo.root / TYPES
        formatted = subprocess.run(
            ["rustfmt", "--edition", "2021", str(source)], capture_output=True,
            text=True, timeout=self_test_hang.HANG_CEILING_SECONDS)
        self.assertEqual(formatted.returncode, 0, formatted.stderr)
        self.repo.commit("feat: Add documented persisted fields [ORB-3]")
        self.repo.git("tag", "v0.35.1")

        report = self.report("--range", "v0.35.0..v0.35.1")
        self.assertEqual(report["bump"], "minor")
        self.assertEqual(report["next_version"], "0.36.0")
        self.assertEqual(self.signals(report), {
            ("ORB-3", "persisted-field",
             "field `documented_skip` added to deny_unknown_fields type `TaskEnvelopeV2`"),
            ("ORB-3", "persisted-field",
             "field `documented_skip_serializing` added to deny_unknown_fields type `TaskEnvelopeV2`"),
        })

    def test_additive_changes_decide_patch(self):
        # New optional field on a type without deny_unknown_fields.
        self.repo.replace(TYPES, "pub struct TaskRow {\n    pub id: String,\n",
                          "pub struct TaskRow {\n    pub id: String,\n    pub title: Option<String>,\n")
        # A new deny_unknown_fields type is new surface, not a changed record.
        self.repo.write(TYPES, self.repo.read(TYPES) + textwrap.dedent("""\

            #[serde(deny_unknown_fields)]
            pub struct Schedule {
                pub at: String,
            }
            """))
        self.repo.commit("feat: Optional title on rows [ORB-20] (#1)")
        # MCP: description-only edits and a new optional property.
        tools = self.repo.mcp()
        tools[0]["description"] = "Show one task by ID."
        tools[0]["inputSchema"]["properties"]["id"]["description"] = "The task's ID"
        tools[1]["inputSchema"]["properties"]["status"] = {"type": "string"}
        self.repo.write_mcp(tools)
        self.repo.commit("feat: Filter task list by status [ORB-21] (#2)")
        # A new command, a new flag and a new possible value.
        self.repo.replace(f"{GOLDENS}/task.txt", "  list  List tasks\n",
                          "  list  List tasks\n  lint  Lint tasks\n")
        self.repo.write(f"{GOLDENS}/task/lint.txt",
                        "Lint tasks\n\nUsage: orbit task lint [OPTIONS]\n\nOptions:\n"
                        "  -h, --help\n          Print help\n")
        self.repo.replace(f"{GOLDENS}/task/show.txt", "full, compact]", "full, compact, ids]")
        self.repo.replace(f"{GOLDENS}/task/show.txt", "      --pretty\n",
                          "      --wide\n          Wide rows\n\n      --pretty\n")
        self.repo.commit("feat: orbit task lint [ORB-22] (#3)")
        # A new seeded asset and a removed (not added) deprecated key are additive.
        self.repo.write("crates/orbit-core/assets/jobs/new_pipeline.yaml", "id: new\n")
        self.repo.replace(KEYS, '    ("review.before_pr", REVIEW_TIMING_NOTE),\n', "")
        self.repo.commit("feat: New pipeline [ORB-23] (#4)")
        self.repo.git("tag", "v0.35.1")

        report = self.report("--range", "v0.35.0..v0.35.1")
        self.assertEqual(report["signals"], [])
        self.assertEqual(report["bump"], "patch")
        self.assertEqual(report["next_version"], "0.35.1")
        self.assertEqual(report["changelog_breaking"], [])

    def test_required_children_of_new_optional_mcp_object_decide_patch(self):
        tools = self.repo.mcp()
        tools[0]["inputSchema"]["properties"]["options"] = {
            "type": "object",
            "properties": {"mode": {"type": "string"}},
            "required": ["mode"],
        }
        self.repo.write_mcp(tools)
        self.repo.commit("feat: Add optional task options [ORB-49] (#1)")

        report = self.report()
        self.assertEqual(report["signals"], [])
        self.assertEqual(report["bump"], "patch")
        self.assertEqual(report["next_version"], "0.35.1")

    def test_required_field_added_to_existing_mcp_object_decides_minor(self):
        tools = self.repo.mcp()
        tools[0]["inputSchema"]["properties"]["options"] = {
            "type": "object",
            "properties": {"mode": {"type": "string"}},
        }
        self.repo.write_mcp(tools)
        self.repo.commit("feat: Add optional task options [ORB-50] (#1)")

        tools = self.repo.mcp()
        tools[0]["inputSchema"]["properties"]["options"]["required"] = ["mode"]
        self.repo.write_mcp(tools)
        self.repo.commit("feat: Require option mode [ORB-51] (#2)")

        report = self.report()
        self.assertEqual(self.signals(report), {
            ("ORB-51", "mcp-schema",
             "`orbit_task_show` property `inputSchema.options.mode` newly required"),
        })
        self.assertEqual(report["bump"], "minor")
        self.assertEqual(report["next_version"], "0.36.0")

    def test_allof_required_on_existing_mcp_object_decides_minor(self):
        tools = self.repo.mcp()
        tools[0]["inputSchema"]["allOf"] = [{"required": ["view"]}]
        self.repo.write_mcp(tools)
        self.repo.commit("feat: Require view via allOf [ORB-52] (#1)")

        report = self.report()
        self.assertEqual([(s["task"], s["signal"]) for s in report["signals"]],
                         [("ORB-52", "mcp-schema")])
        self.assertEqual(report["bump"], "minor")
        self.assertEqual(report["next_version"], "0.36.0")

    def test_front_inserted_mcp_alternative_is_additive(self):
        for keyword in ("anyOf", "oneOf"):
            with self.subTest(keyword=keyword):
                tools = self.repo.mcp()
                schema = tools[0]["inputSchema"]
                schema.pop("anyOf", None)
                schema.pop("oneOf", None)
                schema[keyword] = [{"required": ["view"]}, {"required": ["mode"]}]
                self.repo.write_mcp(tools)
                self.repo.commit(f"feat: Add {keyword} alternatives [ORB-58] (#1)")
                self.repo.git("tag", "-f", "v0.35.0")

                schema[keyword].insert(0, {"required": ["fields"]})
                self.repo.write_mcp(tools)
                self.repo.commit(f"feat: Insert {keyword} alternative at front [ORB-59] (#2)")

                report = self.report("--range", "v0.35.0..HEAD")
                self.assertEqual(report["signals"], [])
                self.assertEqual(report["bump"], "patch")
                self.assertEqual(report["next_version"], "0.35.1")

    def test_existing_mcp_alternative_tightening_decides_minor(self):
        for keyword in ("anyOf", "oneOf"):
            with self.subTest(keyword=keyword):
                tools = self.repo.mcp()
                schema = tools[0]["inputSchema"]
                schema["properties"]["mode"] = {"type": "string"}
                schema.pop("anyOf", None)
                schema.pop("oneOf", None)
                schema[keyword] = [{"required": ["view"]}, {"required": ["mode"]}]
                self.repo.write_mcp(tools)
                self.repo.commit(f"feat: Add {keyword} alternatives [ORB-55] (#1)")
                self.repo.git("tag", "-f", "v0.35.0")

                schema[keyword].append({"required": ["fields"]})
                self.repo.write_mcp(tools)
                self.repo.commit(f"feat: Append {keyword} alternative [ORB-56] (#2)")

                additive = self.report()
                self.assertEqual(additive["signals"], [])
                self.assertEqual(additive["bump"], "patch")
                self.assertEqual(additive["next_version"], "0.35.1")

                schema[keyword][0]["required"].append("mode")
                self.repo.write_mcp(tools)
                self.repo.commit(f"feat: Tighten {keyword} alternative [ORB-57] (#3)")

                report = self.report()
                self.assertEqual(self.signals(report), {
                    ("ORB-57", "mcp-schema",
                     f"`orbit_task_show` property `inputSchema{{{keyword}0}}.mode` newly required"),
                })
                self.assertEqual(report["bump"], "minor")
                self.assertEqual(report["next_version"], "0.36.0")

    def test_existing_boolean_schema_property_gaining_required_child_decides_minor(self):
        tools = self.repo.mcp()
        tools[0]["inputSchema"]["properties"]["options"] = True
        self.repo.write_mcp(tools)
        self.repo.commit("feat: Add task options [ORB-53] (#1)")

        tools = self.repo.mcp()
        tools[0]["inputSchema"]["properties"]["options"] = {
            "type": "object",
            "properties": {"mode": {"type": "string"}},
            "required": ["mode"],
        }
        self.repo.write_mcp(tools)
        self.repo.commit("feat: Require option mode [ORB-54] (#2)")

        report = self.report()
        mcp = {(s["task"], s["signal"]) for s in report["signals"]}
        self.assertIn(("ORB-54", "mcp-schema"), mcp)
        self.assertEqual(report["bump"], "minor")
        self.assertEqual(report["next_version"], "0.36.0")

    def test_tag_forces_minor_and_breaking_notes_draft_bullets(self):
        self.repo.write("docs/ship.md", "ship\n")
        self.repo.commit("feat: Retire ship-sweep [ORB-30] (#1)")
        self.repo.replace(LAYOUT, "= 3;", "= 4;")
        self.repo.commit("feat: Move run logs [ORB-31] (#2)")
        self.repo.write("docs/other.md", "other\n")
        self.repo.commit("fix: Unrelated [ORB-32] (#3)")
        tasks = {
            "ORB-30": {"title": "Retire ship-sweep", "tags": ["release", "breaking-change"],
                       "execution_summary": "## Summary\n\nDone.\n\nBreaking: orbit run ship-sweep "
                                            "removed → use orbit run auto drains or orbit run ship\n"},
            "ORB-31": {"title": "Move run logs", "tags": [], "execution_summary": "No note."},
            "ORB-32": {"title": "Unrelated", "tags": [],
                       "execution_summary": "Not breaking: nothing to migrate."},
        }

        report = self.report("--range", "v0.35.0..HEAD", tasks=tasks)
        self.assertEqual(report["task_source"], "orbit")
        self.assertEqual(report["bump"], "minor")
        self.assertEqual(report["tagged_without_signal"], ["ORB-30"])
        self.assertEqual(report["detected_untagged"], ["ORB-31"])
        self.assertEqual(report["changelog_breaking"][0],
                         "- **Retire ship-sweep**: orbit run ship-sweep removed → use orbit run "
                         "auto drains or orbit run ship ([ORB-30])")
        placeholder = report["changelog_breaking"][1]
        self.assertTrue(placeholder.startswith("- **Move run logs**: TODO: ORB-31 has no "
                                               "`Breaking:` note"), placeholder)
        self.assertTrue(placeholder.endswith("([ORB-31])"), placeholder)
        self.assertEqual(len(report["changelog_breaking"]), 2)

        # The tag alone decides minor when nothing is detected.
        only_tag = self.report("--range", "v0.35.0..HEAD~2", tasks=tasks)
        self.assertEqual((only_tag["bump"], only_tag["signals"]), ("minor", []))

        # Unknown to Orbit, a task falls back to its commit subject.
        fallback = self.report("--range", "v0.35.0..HEAD", tasks={})
        self.assertEqual(fallback["task_source"], "commit-subjects")
        self.assertEqual(fallback["detected_untagged"], ["ORB-31"])

    def test_each_mechanical_signal(self):
        # A subcommand's copy of a global option moves to the condensed
        # Global options list: the option still exists at the program root.
        self.repo.replace(f"{GOLDENS}/task/show.txt",
                          "      --format <MODE>\n          Output format\n\n"
                          "          [possible values: auto, json]\n\n", "")
        self.repo.write(f"{GOLDENS}/task/show.txt", self.repo.read(f"{GOLDENS}/task/show.txt")
                        + "\nGlobal options (see 'orbit --help' for descriptions):\n"
                          "  --format <MODE>\n")
        self.repo.commit("feat: condense global options [ORB-39] (#0)")
        self.repo.replace(LAYOUT, "= 3;", "= 4;")
        self.repo.commit("feat: layout [ORB-40] (#1)")
        self.repo.replace(f"{GOLDENS}/task.txt", "  list  List tasks\n", "")
        self.repo.commit("feat: drop task list [ORB-41] (#2)")
        self.repo.replace(f"{GOLDENS}/task/show.txt", "      --pretty\n          Indent the JSON\n\n", "")
        self.repo.commit("feat: drop --pretty [ORB-42] (#3)")
        self.repo.replace(f"{GOLDENS}/task/show.txt", "full, compact]", "full]")
        self.repo.replace(f"{GOLDENS}/root.txt", "          - json:   A single JSON document\n", "")
        self.repo.commit("feat: drop values [ORB-43] (#4)")
        tools = self.repo.mcp()
        del tools[1]
        self.repo.write_mcp(tools)
        self.repo.commit("feat: drop tool [ORB-44] (#5)")
        tools = self.repo.mcp()
        schema = tools[0]["inputSchema"]
        del schema["properties"]["fields"]
        schema["properties"]["view"]["enum"] = ["full"]
        schema["properties"]["id"]["type"] = "integer"
        schema["properties"]["limit"] = {"type": "integer"}
        schema["required"] = ["id", "limit"]
        self.repo.write_mcp(tools)
        self.repo.commit("feat: reshape task show [ORB-45] (#6)")
        self.repo.replace(TYPES, "    Task { id: String },", "    Task { id: String, workspace: String },")
        self.repo.replace(TYPES, "        sha: String,\n", "        sha: String,\n        repo: String,\n")
        self.repo.commit("feat: relation targets [ORB-46] (#7)")
        self.repo.replace(KEYS, "];", '    ("workflow.old_key", "translated"),\n];')
        self.repo.commit("feat: retire a key [ORB-47] (#8)")
        self.repo.remove(ACTIVITY)
        self.repo.commit("feat: retire sweep [ORB-48] (#9)")

        report = self.report()  # default range: the last vX.Y.Z tag..HEAD
        self.assertEqual(report["range"], "v0.35.0..HEAD")
        self.assertEqual(self.signals(report), {
            ("ORB-40", "layout-version", "SUPPORTED_LAYOUT_VERSION 3 → 4"),
            ("ORB-41", "cli-help", "command `orbit task list` removed"),
            ("ORB-42", "cli-help", "flag `orbit task show --pretty` removed"),
            ("ORB-43", "cli-help", "possible value `compact` of `orbit task show --view` removed"),
            ("ORB-43", "cli-help", "possible value `json` of `orbit --format` removed"),
            ("ORB-44", "mcp-schema", "tool `orbit_task_list` removed"),
            ("ORB-45", "mcp-schema",
             "`orbit_task_show` property `inputSchema.fields` removed or renamed"),
            ("ORB-45", "mcp-schema", "`orbit_task_show` `inputSchema.id` type \"string\" → \"integer\""),
            ("ORB-45", "mcp-schema", "`orbit_task_show` `inputSchema.view` enum value \"compact\" removed"),
            ("ORB-45", "mcp-schema", "`orbit_task_show` property `inputSchema.limit` newly required"),
            ("ORB-46", "persisted-field",
             "field `Task.workspace` added to deny_unknown_fields type `RelationTarget`"),
            ("ORB-46", "persisted-field",
             "field `Commit.repo` added to deny_unknown_fields type `RelationTarget`"),
            ("ORB-47", "deprecated-key", "`workflow.old_key` added to DEPRECATED_CONFIG_KEYS"),
            ("ORB-48", "seeded-asset", "seeded asset removed"),
        })
        self.assertEqual(report["bump"], "minor")


if __name__ == "__main__":
    unittest.main()
