# CI log fixture goldens

These are sanitized, captured-shape excerpts reconstructed from the historical
runner samples in the existing regression tests, rather than new live GitHub
reads. Repository names are fixture placeholders such as `acme/orbit` and
`openai/orbit`; credentials are masks or visibly synthetic repeated-character
tokens. No real credential was copied.

JSON fixture names map to the existing unit tests below. A `/suffix` names a
variant of that regression. Streamed `parts` concatenate strings and
`{"text": "...", "repeat": N}` segments, keeping large source/retention-limit
fixtures small on disk. The fixtures vary chunk boundaries, ANSI, Unicode,
checkout attribution, omitted source, command completeness, rerun metadata,
endpoint validation, and synthetic credential boundaries.

`make goldens` verifies these contracts; `make goldens UPDATE=1` regenerates
them. Set `ORBIT_LOG_GOLDEN_CASE` to an exact fixture name to replay one case
without regeneration (used for deliberate-break validation). Review the parsed JSON diff before accepting an update. Outputs snapshot
parser results and retained evidence, not task descriptions or prompt prose.

The tests join `public_tool_surface` and use the production `github_cli`
API consumed by deterministic automation: the streamed collector, request
builders/projections and log recovery. Recovery additionally exercises the
registered tools with a scripted `gh`, recording argv and normalized cwd.
That fixture re-executes in a child with cleared authority, disposable HOME
and a 120-second deadline, with kill/reap on exit. The pure parsers need no
process-global mutations. The retained live-only test stays ignored; its log
shape has an offline counterpart here, not a claim of current GitHub access.
The legacy test-only `bound_log_text` cases exercise the same guarantees
through the production streamed collector rather than reproducing a private
helper. Its streaming omission marker is part of the output contract.

All existing unit tests remain. Retirement tasks can use this mapping when
removing duplicated cases.

## Carrier cost and boundary coverage

Rows write `golden row start: <suite>/<fixture>` to stderr before source
construction or rendering. Completion records include row elapsed time; streamed
rows also record source preparation, source bytes, chunk size, push count, push
time and finish/render time. A timeout can therefore name the last started row
and collector. Nextest's full success output retains these records.

The collector amortizes tiny reads in a bounded 4096-byte input buffer, processes
the final partial buffer in `finish`, and keeps unfinished UTF-8 characters for
the next batch. Head/tail retention, adjacent cut bytes, scan limits, redaction
and source byte accounting still operate on the original bytes. A bounded tail
ring avoids moving retained bytes on each batch. The line collectors reuse their
bounded buffers and avoid impossible command/identity matches on short lines;
short lines inside an active command or failure block still count and survive.
Unstyled lines bypass ANSI character reconstruction.

All original 122 streamed rows, their chunk schedules and caps remain. The
printed-command provenance update below adds a retained display closer to one
existing region and adjusts its byte accounting; other existing streamed
outputs remain unchanged. In particular, the three 8 MiB source-limit negatives remain;
the retention-cut credential rows retain their 19 chunk sizes; checkout/event
and action identity, ambiguous/hidden columns, overlong lines, source refusal,
completeness, omitted-byte accounting, Unicode, multiline panic and secondary
failure controls retain their existing public-boundary carriers. The separate
mid-log nextest test still checks uneven and whole-stream input, source ranges,
oversized first blocks, secondary failures and the block cap. No coverage moved
to another test. The added
`checkout_unicode_crosses_transport_and_collector_batch_boundaries` row puts a
two-byte checkout subject character across byte 4096 and checks single-byte,
uneven, batch-sized and whole-stream input. The original collector fails its
chunk-equivalence assertion; the candidate preserves the character in evidence.

Native Linux measurements against source
`6a74cce2f1cf3305898bd6eb19b3e6b1d2c8e2fe` used the full `orbit-tools`
library/binary/integration selection, the default nextest profile, Rust 1.98.1,
nextest 0.9.136, the same progress instrumentation and host build budget (8
admission slots, 4 Cargo jobs, 4 test threads). Dependencies were warm for the
matched pair; production changes required a rebuild. Each run passed all 100
tests across three binaries, with zero skipped tests.

| Matched sample | Streamed carrier | Target test wall | Summed testcase cost | Streamed rows |
| --- | ---: | ---: | ---: | ---: |
| Baseline | 6.682 s | 6.852 s | 12.534 s | 122 |
| Candidate | 2.862 s | 3.048 s | 6.909 s | 123 |

The carrier reduction is 57.2%, including its final partial-batch work and the
added Unicode row. Summed full-target cost falls 44.9%; it is not elapsed job
time. Raw logs, JUnit, source hashes, build/runner/load context, preparation and
per-collector timings for every exploratory and final sample are retained in
the task's `log-cost` evidence. No sample was rejected as an outlier. The initial
baseline required a cold 45.60-second build; matched test timings exclude build
time. A profiling attempt was denied by the host's perf policy and is retained
as unavailable attribution, rather than a successful profile.

Separately, the source's hosted Check / Clippy / Test job
`115794101765` in run `38546610947` succeeded in 1158 seconds, and its captured
streamed carrier row took 25.486 seconds. No candidate hosted job has run as
part of this native comparison. These measurements establish the native carrier
target, not a hosted speedup or the whole-job 900-second budget.

| Existing test file | Regression / fixture prefix | Golden |
| --- | --- | --- |
| `discovery_requests.rs` | `dependabot_alert_request_is_bounded_and_projects_compact_evidence` | `requests.golden.json` |
| `discovery_requests.rs` | `dependabot_pull_request_discovery_requests_and_projects_the_head_branch` | `requests.golden.json` |
| `discovery_requests.rs` | `dependabot_alert_request_rejects_repository_path_injection` | `requests.golden.json` |
| `discovery_requests.rs` | `code_and_secret_scanning_requests_are_bounded_and_validate_repository` | `requests.golden.json` |
| `discovery_requests.rs` | `scanning_projections_retain_evidence_but_structurally_drop_secret` | `requests.golden.json` |
| `discovery_requests.rs` | `run_list_applies_every_filter_and_caps_the_limit` | `requests.golden.json` |
| `discovery_requests.rs` | `run_list_defaults_to_an_unfiltered_bounded_query` | `requests.golden.json` |
| `discovery_requests.rs` | `a_filter_value_cannot_smuggle_another_gh_flag` | `requests.golden.json` |
| `discovery_requests.rs` | `run_view_requires_a_numeric_run_id` | `requests.golden.json` |
| `discovery_requests.rs` | `run_view_projects_the_reported_head_sha_and_collects_failed_jobs` | `requests.golden.json` |
| `discovery_requests.rs` | `a_cancelled_job_counts_as_unsuccessful` | `requests.golden.json` |
| `discovery_requests.rs` | `run_logs_defaults_to_failed_steps_and_accepts_the_full_log` | `requests.golden.json` |
| `discovery_requests.rs` | `run_logs_rejects_an_unknown_scope` | `requests.golden.json` |
| `discovery_requests.rs` | `pr_list_projects_each_head_sha_under_its_own_name` | `requests.golden.json` |
| `discovery_requests.rs` | `pr_list_defaults_to_a_bounded_open_query` | `requests.golden.json` |
| `discovery_requests.rs` | `discovery_tools_run_in_the_selected_workspace_rather_than_the_process_cwd` | `fallback.golden.json` |
| `discovery_requests.rs` | `discovery_tools_without_a_selected_workspace_keep_the_callers_directory` | `fallback.golden.json` |
| `bounded_logs.rs` | `a_short_log_is_returned_whole_and_unmarked` | `streamed.golden.json` |
| `bounded_logs.rs` | `an_oversized_log_is_capped_while_keeping_both_ends` | `streamed.golden.json` |
| `bounded_logs.rs` | `truncation_never_splits_a_multibyte_character` | `streamed.golden.json` |
| `bounded_logs.rs` | `a_credential_in_the_log_is_redacted_before_it_is_returned` | `streamed.golden.json` |
| `bounded_logs.rs` | `checkout_evidence_reports_the_tested_commit_not_the_event_sha` | `streamed.golden.json` |
| `bounded_logs.rs` | `a_pinned_action_sha_is_never_reported_as_the_checked_out_commit` | `streamed.golden.json` |
| `bounded_logs.rs` | `a_bare_sha_outside_a_checkout_step_is_not_treated_as_evidence` | `streamed.golden.json` |
| `bounded_logs.rs` | `git_log_head_command_in_an_unknown_step_names_its_following_sha` | `streamed.golden.json` |
| `bounded_logs.rs` | `only_the_immediate_output_of_the_recognized_command_is_checkout_evidence` | `streamed.golden.json` |
| `bounded_logs.rs` | `multiple_recognized_command_outputs_remain_ambiguous` | `streamed.golden.json` |
| `bounded_logs.rs` | `checkout_evidence_lines_are_capped_and_redacted` | `streamed.golden.json` |
| `bounded_logs.rs` | `a_log_without_checkout_evidence_yields_nothing_rather_than_a_guess` | `streamed.golden.json` |
| `bounded_logs.rs` | `checkout_evidence_caps_and_dedups_commits` | `streamed.golden.json` |
| `bounded_logs.rs` | `an_overlong_line_unrelated_to_checkout_is_dropped_without_marking_identity_incomplete` | `streamed.golden.json` |
| `bounded_logs.rs` | `an_overlong_checkout_line_is_dropped_and_marks_identity_incomplete` | `streamed.golden.json` |
| `bounded_logs.rs` | `an_overlong_line_consumes_pending_checkout_command_context` | `streamed.golden.json` |
| `bounded_logs.rs` | `streaming_log_finds_middle_checkout_without_retaining_it_in_the_excerpt` | `streamed.golden.json` |
| `bounded_logs.rs` | `streaming_log_marks_identity_incomplete_after_the_hard_scan_limit` | `streamed.golden.json` |
| `bounded_logs.rs` | `command_output_past_the_hard_scan_limit_is_not_verified` | `streamed.golden.json` |
| `bounded_logs.rs` | `an_abbreviated_and_a_full_spelling_are_one_commit` | `streamed.golden.json` |
| `bounded_logs.rs` | `merge_parents_quoted_in_a_checkout_subject_are_not_checkout_identity` | `streamed.golden.json` |
| `bounded_logs.rs` | `two_checkout_steps_naming_different_commits_stay_two_commits` | `streamed.golden.json` |
| `bounded_logs.rs` | `an_abbreviation_with_rival_expansions_is_not_resolved_to_a_guess` | `streamed.golden.json` |
| `bounded_logs.rs` | `long_log_retains_complete_middle_command_independently_of_display` | `streamed.golden.json` |
| `bounded_logs.rs` | `incomplete_ambiguous_and_source_limited_commands_are_not_complete_units` | `streamed.golden.json` |
| `bounded_logs.rs` | `selected_command_preserves_unicode_and_redacts_secrets_across_chunk_boundaries` | `streamed.golden.json` |
| `bounded_logs.rs` | `oversized_command_keeps_all_failure_regions_and_counts_assertion_omissions` | `streamed.golden.json` |
| `bounded_logs.rs` | `partial_regions_never_override_missing_source_ambiguous_commands_or_hidden_columns` | `streamed.golden.json` |
| `bounded_logs.rs` | `failure_region_overflow_defers_instead_of_losing_secondary_failures` | `streamed.golden.json` |
| — | `oversized_command_keeps_a_multiline_panic_payload_through_the_failure_block` | `streamed.golden.json` |
| — | `failure_block_over_the_region_budget_is_marked_cut` | `streamed.golden.json` |
| `bounded_logs.rs` | `assertion_prefix_never_leaks_a_secret_cut_at_the_retention_boundary` | `streamed.golden.json` |
| `bounded_logs.rs` | `credential_straddling_a_retained_boundary_is_not_returned` | `streamed.golden.json` |
| `bounded_logs.rs` | `a_whole_credential_inside_a_window_or_across_the_short_join_is_redacted` | `streamed.golden.json` |
| `bounded_logs.rs` | `ordinary_bounded_logs_keep_useful_ends_and_middle_evidence` | `streamed.golden.json` |
| `bounded_logs.rs` | `streamed_excerpt_does_not_grow_with_the_source_log` | `streamed.golden.json` |
| `log_fallback.rs` | `an_empty_failed_step_read_recovers_the_failed_jobs_own_log` | `fallback.golden.json` |
| `log_fallback.rs` | `an_in_progress_parent_readiness_error_recovers_a_completed_failed_job` | `fallback.golden.json` |
| `log_fallback.rs` | `readiness_does_not_read_a_running_failed_job` | `fallback.golden.json` |
| `log_fallback.rs` | `a_successful_job_is_never_read_as_failed_step_evidence` | `fallback.golden.json` |
| `log_fallback.rs` | `job_metadata_naming_another_run_is_refused` | `fallback.golden.json` |
| `log_fallback.rs` | `a_job_url_from_another_run_is_refused` | `fallback.golden.json` |
| `log_fallback.rs` | `an_unavailable_job_log_is_reported_rather_than_fabricated` | `fallback.golden.json` |
| `log_fallback.rs` | `an_empty_job_log_falls_through_to_the_next_failed_job` | `fallback.golden.json` |
| `log_fallback.rs` | `an_oversized_job_log_is_bounded_and_keeps_the_failing_tail` | `fallback.golden.json` |
| `log_fallback.rs` | `whole_run_scope_recovers_checkout_evidence_from_a_job_log` | `fallback.golden.json` |
| `log_fallback.rs` | `a_narrowed_job_must_belong_to_the_run` | `fallback.golden.json` |
| `log_fallback.rs` | `a_job_from_a_previous_rerun_attempt_is_refused` | `fallback.golden.json` |
| `log_fallback.rs` | `a_failing_run_scoped_read_stays_an_error_instead_of_falling_back` | `fallback.golden.json` |
| `log_fallback.rs` | `auth_and_network_failures_do_not_trigger_job_log_recovery` | `fallback.golden.json` |
| `log_fallback.rs` | `the_job_log_endpoint_is_a_read_of_this_repository_only` | `fallback.golden.json` |
| `log_fallback.rs` | `the_registered_tool_reads_every_log_request_in_the_selected_workspace` | `fallback.golden.json` |
| `log_fallback.rs` | `a_repository_that_could_traverse_the_endpoint_is_rejected` | `fallback.golden.json` |
| `log_fallback.rs` | `source_read_limit_defers_without_retrying_or_returning_a_partial_unit` | `fallback.golden.json` |
| `log_fallback.rs` | `live_long_job_logs_retain_complete_command_and_checkout` | `fallback.golden.json` |

Printed-command provenance cases distinguish source inside a runner `Run`
display from emitted diagnostics after `##[endgroup]`. Printed nextest status,
compiler text and process-completion annotations do not create failure blocks
or complete a command. Selected oversized-command regions now retain the
closing display delimiter, with retained/omitted byte counts adjusted for those
source bytes. The fallback and mid-log nextest synthetic commands also include
that real runner delimiter before runtime output. Their mid-log recovery,
source-range, secondary-failure and budget assertions remain in place.
