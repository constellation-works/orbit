# Machine checks — STD-01 CLI surface

The gate that enforces each rule in [STD-01](STD-01.md), or `review-only`. Ship a change
with the gate its rule names, or say it is review-only.

The **Gate** column names what an adopting repo wires up. The **Orbit instance** column
names where Orbit already has that gate. A rule marked `review-only` has no mechanical
gate that is worth its false-positive rate, so reviewers check it against the rule text.
For a new Rust CLI, `assert_cmd` for exit codes and streams, plus `insta` or `trycmd` for
goldens, covers most of this table.

| Rule | Gate | Orbit instance |
|---|---|---|
| R1 | review-only | Root-help grouping test: `command/tests/mod.rs#root_help_groups_every_visible_command_exactly_once` |
| R2 | review-only | — |
| R3 | Whole-tree parser assertion (clap `Command::debug_assert()` in a test) catches colliding long flags; spelling consistency is review-only | `cli_command_tree_debug_assert_rejects_duplicate_long_flags` |
| R4 | Unit test: the global flag parses before and after a subcommand | `src/tests/cli_format.rs` (`format_is_accepted_before_and_after_the_subcommand`, `every_command_declares_exactly_one_format_…`) |
| R26 | Unit test that walks the command tree (`get_subcommands()` / `get_arguments()`) and fails if a subcommand argument's id equals a global argument's id | review-only in Orbit (explicit `id = "workspace_selector"`); behaviour regression `tests/workspace_selector.rs#workspace_remove_deregisters_…` |
| R5 | Test: the destructive verb without `--confirm` fails before any mutation | review-only in Orbit (shared `require_confirmation` helper) |
| R27 | Unit test: the prompt reader given empty input fails once and prompts once; binary test with closed stdin exits non-zero within a deadline | `command/init/tests/command.rs#task_prefix_prompt_rejects_closed_stdin_without_retrying`; `tests/init_interactive_stdin.rs#closed_stdin_keeps_the_existing_message` |
| R28 | Integration test per override: run under a scratch root and HOME, then assert host-global paths are byte-for-byte unchanged; a test that an unsupported override is refused | `tests/ambient_authority_isolation.rs#fixture_lifecycle_leaves_the_ambient_authority_byte_for_byte_unchanged`; `tests/update.rs#update_preserves_explicit_and_environment_roots_…` |
| R29 | Test: an unknown or unsupported field fails, names the field, and writes nothing | `orbit-tools/…/task/tests/strict_input.rs#add_rejects_unknown_key`; `orbit-web/src/api/tests/config.rs#an_unknown_key_is_refused_before_any_write` |
| R30 | Test asserts the effect by reading state back, not the success message; unit test that the success line names the resolved target | `mcp/setup/tests/dispatch.rs#action_summary_names_the_resolved_checkout_and_workspace_id` |
| R31 | Integration test: run every read command against a read-only (or digest-snapshotted) data root and assert nothing changed | `orbit-common/src/storage/tests/sqlite.rs#private_read_only_filesystem_path_has_no_side_effects`; `tests/mcp_roundtrip.rs#read_only_registry_files_keep_…`; partial (run reads reconcile by default) |
| R32 | Round-trip test: every id form the tool emits in `list --json` is fed back to `show` and to each command that takes that selector | `orbit-cmd/…/tests/selection.rs#local_host_qualified_selector_binds_and_foreign_host_fails_closed` |
| R33 | Integration test: more than N newer non-matching records plus one older match, then `--filter --limit N` returns the match; the default list spans every status | `tests/task_list.rs#older_someday_task_discoverable_ahead_of_50_newer_done_tasks`; `tests/task_list.rs#existing_filters_keep_behaviour` |
| R6 | Output goldens: the human and machine forms of the same fixture are both pinned | `tests/output_goldens.rs#plain_and_json_forms_match_their_goldens` |
| R7 | Output goldens include a `--json` form for each list and detail command; a test that `--json` with a different `--format` exits 2 | `tests/output_goldens/*.json`; `tests/json_output_stability.rs`; conflict check absent (see *Deviations* in [`STD-01.md`](STD-01.md#deviations)) |
| R8 | Unit test of the resolver's precedence table | `src/output/tests/sink.rs` (`rung_one_…` through `rung_four_…`, `unrecognized_environment_value_falls_through_to_auto`) |
| R9 | Integration test: piped output contains no `\x1b[` and no box glyphs; N records produce N lines | `tests/output_goldens.rs#no_ansi_escapes_under_any_color_configuration`; `tests/table_rendering.rs` |
| R10 | Byte-exact JSON goldens; a diff must be reviewed as a contract change | `tests/json_output_stability.rs`; `make goldens` |
| R11 | review-only (goldens catch regressions, not first violations) | — |
| R12 | Integration test: stdout is empty or pure payload, and notices land on stderr | `tests/task_list.rs#task_list_truncation_notice_reaches_stderr_…`; `src/output/tests/gating.rs#progress_needs_a_terminal_and_a_human_facing_mode` |
| R13 | Test: a broken-pipe write is treated as a silent exit 0 | `src/output/tests/pipe.rs` |
| R34 | Test with more matches than the limit: `--json` carries `total` and `truncated: true`; the stderr notice appears in every mode | `orbit-core/…/tool_host/tests/task_tools.rs#task_list_tool_is_status_aware_and_bounded`; `orbit-web/src/api/tests/tasks.rs` (`truncated` assertions); `tests/task_list.rs#task_list_truncation_notice_reaches_stderr_in_json_and_ndjson_modes` |
| R14 | Rendering unit tests at a pinned width (never read the width from the environment in tests) | `src/output/tests/table.rs`; `tests/table_rendering.rs` |
| R15 | Rendering unit test for `…`, and one that an unknown width does not truncate; detail-command coverage is review-only (keep a table of truncatable columns and their detail commands) | `src/output/tests/table.rs#overflow_is_truncated_with_a_single_ellipsis`; `src/output/tests/sink.rs#absent_width_disables_truncation_rather_than_defaulting_to_eighty`; `references/detail-commands.md` |
| R16 | Integration test: an empty result gives empty stdout (or exactly `[]` under `--json`), a stderr line, and exit 0 | `tests/table_rendering.rs#a_list_with_no_matches_…`; `tests/json_output_stability.rs#empty_list_json_is_exactly_an_empty_array` |
| R17 | Grep guard in CI: no TTY, width, or color-env query outside the one resolver module; manifest review that clap is built without `color` and `wrap_help` | `scripts/check-terminal-state-guard.sh` (run by `scripts/ci-guardrails.sh`); `src/output/tests/sink.rs#no_color_disables_color_and_outranks_clicolor_force`; `src/output/tests/gating.rs` |
| R18 | review-only | — |
| R19 | Integration test: a failing `--json` call puts `{error, code}` on stderr and nothing on stdout; a usage error's first stderr line starts with `error:` | review-only in Orbit (no CLI test pins `print_error`) |
| R20 | Integration test with explicit `.code(1)` / `.code(2)` assertions | partial: `tests/plugin_cli_group.rs` asserts `.code(1)`; code 2 is clap's default and unpinned |
| R21 | review-only | — |
| R22 | Help goldens make every help change visible in the diff; quality is review-only | `make goldens` |
| R23 | Test that renders the whole help tree recursively and rejects tracker-ID patterns | `command/tests/mod.rs#recursive_cli_help_uses_only_placeholder_artifact_ids` |
| R24 | Help and output golden suite in CI, with an explicit update switch | `make goldens` → `scripts/check-goldens.sh` (`ORBIT_UPDATE_HELP_GOLDENS`, `ORBIT_UPDATE_OUTPUT_GOLDENS`, `ORBIT_MCP_UPDATE_SNAPSHOT`) |
| R25 | review-only for a single-surface CLI; with several surfaces, a test that derives each surface from the declaration and compares it to a snapshot | `command/tests/operation_args.rs`; `tests/snapshots/mcp_tools_list.json` |
| R35 | Test: a deprecated flag or key still parses and warns by name; once its window closes, a test that it is rejected (exit 2 or a load error); help goldens (R24) show the removal | `orbit-config/src/tests/layering.rs#removed_pilot_max_complexity_warns_once_…`; `command/tests/mod.rs#cli_parses_web_serve_global_as_deprecated_noop`; `command/run/tests/ship.rs#ship_rejects_removed_local_subcommand_form` |
| R36 | Parity test: every advertised operation and parameter is invoked with a probe input and must reach its implementation; review-only for a single-surface clap CLI | `orbit-tools/…/tests/search.rs#search_schema_advertises_only_lexical_inputs`; `tests/mcp_roundtrip/plugins.rs#an_enabled_plugin_tool_is_advertised_and_callable_…`; `orbit-mcp/src/federated/tests/capability.rs#the_locked_mapping_covers_exactly_the_advertised_surface` |
