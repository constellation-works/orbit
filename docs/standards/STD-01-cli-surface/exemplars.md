# Exemplars — STD-01 CLI surface

Worked examples of the rules in [STD-01](STD-01.md), as pointers into the Orbit
codebase. Orbit is the example, not the audience: this file is optional reading, and
useful only with an Orbit checkout.

Paths are relative to the constellation root. Live `orbit` invocations were checked
against orbit 0.24.0 on 2026-09-26. Exemplar paths for R26–R36 were checked against
`codebases/orbit` at `05d32aaac` on the same date.

- R1 — `codebases/orbit/crates/orbit-cli/src/command/mod.rs#Commands`, and live
  `orbit task --help` (noun `task`, verbs grouped under `Tasks:`/`Health:`/`Bundles:`).
- R2 — `codebases/orbit/docs/design/terminal-interface/references/detail-commands.md#Covered`
  (each `<noun> list` paired with `<noun> show`).
- R3 — `codebases/orbit/crates/orbit-cli/src/command/task/add.rs` line 29
  (`#[arg(long = "tag", action = ArgAction::Append, …)]`), and
  `codebases/orbit/crates/orbit-cli/src/command/tests/mod.rs#cli_command_tree_debug_assert_rejects_duplicate_long_flags`.
- R4 — `codebases/orbit/crates/orbit-cli/src/command/mod.rs#Cli` (`--root`, `--workspace`
  with `global = true`), `codebases/orbit/crates/orbit-cli/src/main.rs#install_format_arg`,
  and `codebases/orbit/crates/orbit-cli/src/tests/cli_format.rs#format_is_accepted_before_and_after_the_subcommand`.
- R26 — `codebases/orbit/crates/orbit-cli/src/command/workspace/remove.rs#WorkspaceRemoveArgs`
  and `…/command/workspace/teardown.rs` (`#[arg(value_name = "WORKSPACE", id =
  "workspace_selector")]`), with the behaviour regression
  `codebases/orbit/crates/orbit-cli/tests/workspace_selector.rs#workspace_remove_deregisters_deleted_checkout_by_name_id_and_path`.
- R5 — `codebases/orbit/crates/orbit-cli/src/command/mod.rs#require_confirmation`, and
  `codebases/orbit/docs/design-patterns/command.md#Destructive CLI confirmation`.
- R27 — `codebases/orbit/crates/orbit-cli/src/command/init/prompt_stdin.rs#STDIN_CLOSED_BEFORE_PROMPT`
  (the message names `--task-prefix`/`--machine-name` and `--non-interactive`) and
  `#NON_TTY_PROMPT_TIMEOUT`, plus
  `codebases/orbit/crates/orbit-cli/src/command/init/tests/command.rs#task_prefix_prompt_rejects_closed_stdin_without_retrying`.
- R28 — `codebases/orbit/crates/orbit-cli/src/command/mcp/command.rs#ServeArgs::execute_without_runtime`
  (refuses a root override: "does not accept a workspace root override"),
  `codebases/orbit/crates/orbit-cli/tests/update.rs#update_preserves_explicit_and_environment_roots_from_another_checkout`,
  and `codebases/orbit/crates/orbit-cli/tests/workspace_selector.rs#migrate_dry_run_honors_selected_checkout_and_confirm_uses_the_same_one`.
- R29 — `codebases/orbit/crates/orbit-common/src/protocol/tool_input.rs#reject_retired_task_add_input_fields`
  (names the field and the tool that accepts it),
  `codebases/orbit/crates/orbit-tools/src/builtin/orbit/task/tests/strict_input.rs#add_rejects_unknown_key`,
  and `codebases/orbit/docs/design/user-interface/4_decisions.md#All-or-Nothing Rejection of Unsupported Task Body Fields`.
- R30 — `codebases/orbit/crates/orbit-cli/src/command/mcp/setup/dispatch.rs#format_action_summary`
  (`mcp init: claude -> <checkout> (<ws id>)`),
  `…/mcp/setup/tests/dispatch.rs#action_summary_names_the_resolved_checkout_and_workspace_id`,
  and `codebases/orbit/crates/orbit-core/src/adapter/command/registry.rs#set_tool_enabled_state`
  (refuses rather than reporting a no-op success).
- R31 — `codebases/orbit/crates/orbit-cmd/src/registry/runtime/factory.rs#initialize_read_only_with_overrides`,
  `codebases/orbit/crates/orbit-common/src/storage/tests/sqlite.rs#private_read_only_filesystem_path_has_no_side_effects`,
  and `codebases/orbit/crates/orbit-cli/tests/mcp_roundtrip.rs#read_only_registry_files_keep_uncheckpointed_wal_reads_observational`.
  Partial: `codebases/orbit/crates/orbit-cli/src/command/run/steps.rs#RunRead::from_no_reconcile`
  (see *Deviations* in [`STD-01.md`](STD-01.md#deviations)).
- R32 — `codebases/orbit/crates/orbit-cmd/src/registry/runtime/tests/selection.rs#local_host_qualified_selector_binds_and_foreign_host_fails_closed`,
  and `codebases/orbit/crates/orbit-config/src/layering.rs#effective_values` (the listed
  settings come only from the key registry, so every listed key is `get`/`set`-admitted).
- R33 — `codebases/orbit/crates/orbit-cli/src/command/task/list.rs` (`--status` is opt-in;
  `--limit` defaults to `DEFAULT_TASK_LIST_LIMIT` and applies after filters), and
  `codebases/orbit/crates/orbit-cli/tests/task_list.rs#older_someday_task_discoverable_ahead_of_50_newer_done_tasks`.
- R6 — `codebases/orbit/crates/orbit-cli/src/output/payload.rs#Payload`,
  `codebases/orbit/crates/orbit-cli/src/output/render.rs#emit`, and
  `codebases/orbit/docs/design/terminal-interface/4_decisions.md#Terminal Output Is a Rendering of a Structured Payload`.
- R7 — `codebases/orbit/crates/orbit-cli/src/main.rs#format_arg`
  (`auto|table|json|ndjson`), and `codebases/orbit/crates/orbit-cli/src/output/render.rs#ndjson_records`.
- R8 — `codebases/orbit/crates/orbit-cli/src/output/sink.rs#resolve_mode`.
- R9 — `codebases/orbit/crates/orbit-cli/src/output/table.rs#render_plain`, and
  `codebases/orbit/crates/orbit-cli/tests/output_goldens/task_list.plain.txt`.
- R10 — `codebases/orbit/crates/orbit-cli/tests/json_output_stability.rs#json_flag_output_is_untouched_by_the_global_format_machinery`,
  and `codebases/orbit/docs/design/terminal-interface/specs/output-modes.md#4. Payload Rules`.
- R11 — `codebases/orbit/docs/design/terminal-interface/specs/output-modes.md#4. Payload Rules`.
  Live check: `orbit task show <id> --json` carries `"parent_id": null` and
  `"created_at": "…+00:00"`.
- R12 — `codebases/orbit/crates/orbit-cli/src/output/table.rs#trailing_notice`,
  `codebases/orbit/crates/orbit-cli/tests/task_list.rs#task_list_truncation_notice_reaches_stderr_in_json_and_ndjson_modes`,
  and `codebases/orbit/crates/orbit-cli/src/output/sink.rs#progress_allowed`.
- R13 — `codebases/orbit/crates/orbit-cli/src/output/pipe.rs#install_handler`.
- R34 — `codebases/orbit/crates/orbit-core/src/adapter/tool_host/task_tools.rs#list`
  (the `orbit.task.list` tool returns `{tasks, total, truncated}`), and
  `codebases/orbit/crates/orbit-core/src/adapter/tool_host/tests/task_tools.rs#task_list_tool_is_status_aware_and_bounded`.
  The CLI's grandfathered bare array is covered under *Deviations* in [`STD-01.md`](STD-01.md#deviations).
- R14 — `codebases/orbit/crates/orbit-cli/src/output/table.rs#add_row` (the only row
  constructor, height capped at 1), `…/output/table.rs#Column::number`, and
  `codebases/orbit/docs/design/terminal-interface/specs/table-rendering.md`.
- R15 — `codebases/orbit/crates/orbit-cli/src/output/table.rs#Column::path` (middle
  truncation for identifying tails),
  `codebases/orbit/docs/design/terminal-interface/references/detail-commands.md`, and
  `codebases/orbit/crates/orbit-cli/src/output/tests/sink.rs#absent_width_disables_truncation_rather_than_defaulting_to_eighty`.
- R16 — `codebases/orbit/crates/orbit-cli/tests/table_rendering.rs#a_list_with_no_matches_leaves_stdout_empty_and_explains_itself_on_stderr`,
  `codebases/orbit/crates/orbit-cli/tests/json_output_stability.rs#empty_list_json_is_exactly_an_empty_array`,
  and `codebases/orbit/crates/orbit-cli/src/output/table.rs#empty_message`.
- R17 — `codebases/orbit/crates/orbit-cli/src/output/sink.rs#resolve_color`,
  `…/output/sink.rs#apply_color_policy`, `…/output/sink.rs#resolve_width` (`COLUMNS`
  preferred, then the `TIOCGWINSZ` query in `#query_terminal_width`; width `0` means "do
  not truncate"), and
  `codebases/orbit/scripts/check-terminal-state-guard.sh`.
- R18 — `codebases/orbit/crates/orbit-cli/src/output/color.rs#role_for`, and
  `codebases/orbit/docs/design/terminal-interface/specs/color-and-styling.md#3. Rules`.
- R19 — `codebases/orbit/crates/orbit-cli/src/main.rs#print_error`, and
  `codebases/orbit/crates/orbit-cli/src/output/json.rs#error_payload` / `#error_code`.
  Orbit's `main.rs#parse_cli` lets clap print its plain usage error and exit `2` in every
  mode, which is the form R19 allows when the mode is not yet known.
- R20 — `codebases/orbit/crates/orbit-cli/src/main.rs#finish_command`. Live check:
  `orbit task show <missing>` exits `1`, and `orbit task list --bogus` exits `2`.
- R21 — `codebases/orbit/crates/orbit-cli/src/command/mod.rs#require_confirmation`
  (`… is irreversible; pass --confirm to proceed`), and
  `codebases/orbit/crates/orbit-cli/src/main.rs#repair_crew_flag_suggestion`. Live check:
  `orbit task lst` prints the tip `some similar subcommands exist: 'lint', 'list'`.
- R22 — `codebases/orbit/crates/orbit-cli/src/command/mod.rs#ROOT_HELP_TEMPLATE` (grouped
  root help), and live `orbit task list --help` (possible values listed, `Examples:` block).
- R23 — `codebases/orbit/AGENTS.md#Code` ("Never expose internal task/friction IDs …"),
  and `codebases/orbit/crates/orbit-cli/src/command/tests/mod.rs#recursive_cli_help_uses_only_placeholder_artifact_ids`.
- R24 — `codebases/orbit/scripts/check-goldens.sh` (run as `make goldens`, regenerated with
  `make goldens UPDATE=1`), `codebases/orbit/crates/orbit-cli/src/command/tests/mod.rs#assert_help_matches_golden`,
  `codebases/orbit/crates/orbit-cli/tests/output_goldens.rs`, and
  `codebases/orbit/crates/orbit-cli/tests/snapshots/mcp_tools_list.json`. Counter-example
  for the "as shipped" clause: `codebases/orbit/crates/orbit-cli/src/command/tests/friction_help/show.txt`
  has no `--format` line, but live `orbit friction show --help` does.
- R25 — `codebases/orbit/crates/orbit-common/src/governance/operation.rs#OperationSpec`,
  `codebases/orbit/crates/orbit-common/src/governance/friction/operations.rs`,
  `codebases/orbit/crates/orbit-cli/src/command/operation_args.rs`,
  `codebases/orbit/crates/orbit-cli/src/command/tests/operation_args.rs` (a synthetic noun
  gets a complete CLI from nothing but a registry entry), and
  `codebases/orbit/docs/design/operations-as-data/1_overview.md`.
- R35 — `codebases/orbit/crates/orbit-config/src/registry/keys.rs#REMOVED_CONFIG_KEYS`
  with `codebases/orbit/crates/orbit-config/src/resolved.rs#warn_removed_key` (removed
  config keys are warned about by name and ignored),
  `codebases/orbit/crates/orbit-config/src/tests/layering.rs#removed_pilot_max_complexity_warns_once_for_the_layer_that_sets_it`,
  `codebases/orbit/crates/orbit-cli/src/command/tests/mod.rs#cli_parses_web_serve_global_as_deprecated_noop`
  (a deprecated flag that still parses), and
  `codebases/orbit/crates/orbit-cli/src/command/run/tests/ship.rs#ship_rejects_removed_local_subcommand_form`
  (a removed form, rejected after its window).
- R36 — `codebases/orbit/crates/orbit-tools/src/builtin/orbit/tests/search.rs#search_schema_advertises_only_lexical_inputs`,
  `codebases/orbit/crates/orbit-cli/tests/mcp_roundtrip/plugins.rs#an_enabled_plugin_tool_is_advertised_and_callable_and_a_disabled_one_is_not`,
  and `codebases/orbit/crates/orbit-mcp/src/federated/tests/capability.rs#the_locked_mapping_covers_exactly_the_advertised_surface`.
