# CI log fixture goldens

These are sanitized, captured-shape excerpts reconstructed from the historical
runner samples in the former regression unit tests, rather than new live GitHub
reads. Repository names are fixture placeholders such as `acme/orbit` and
`openai/orbit`; credentials are masks or visibly synthetic repeated-character
tokens. No real credential was copied.

JSON fixture names map to the retired unit tests below. A `/suffix` names a
variant of that regression. Streamed `parts` concatenate strings and
`{"text": "...", "repeat": N}` segments, keeping large source/retention-limit
fixtures small on disk. The fixtures vary chunk boundaries, ANSI, Unicode,
checkout attribution, omitted source, command completeness, rerun metadata,
endpoint validation, and synthetic credential boundaries.

`make goldens` verifies these contracts; `make goldens UPDATE=1` regenerates
them. Review the parsed JSON diff before accepting an update. Outputs snapshot
parser results and retained evidence, not task descriptions or prompt prose.

The separate core integration binary drives `OrbitRuntime::run_deterministic`
with `file_ci_failure_tasks`, then reads the filed task. A case with
`"collect": true` instead serves each log, built from string and
`{"text", "repeat"}` parts, through a substitute `gh` as one agent-main job's
`--log-failed` output, runs host collection (`collect_ci_evidence`) over a
local bare `origin`, then files that snapshot; its golden also records the
diagnostic collection bound. `sweep.rs` holds that harness, the multi-sweep
escalation test and the retired-ref investigation budget test. Every scenario
has an isolated store; reruns within a scenario verify persisted deduplication.
`first_batch` submits distinct diagnostics together, as the original tests did,
so pre-existing owner similarity cannot suppress the second diagnostic. It
re-executes in a child with cleared authority, disposable HOME and a 120-second
deadline, with kill/reap on exit. There was no existing CI-filing integration
binary to extend. `parsed.json` records signatures, retained log bodies,
fallback/note flags, filed counts and dedupe keys, omitting allocated task IDs.

The same integration target also checks compiler-owner continuity across
checkout and source-coordinate changes, observation-comment idempotency,
closed owners, and equality of complete code/message/file-path diagnostic
sets. Open-owner identity is retained separately from the bounded description
excerpt; legacy excerpts are accepted only when they reproduce the stored
exact-cause digest. These behavioral cases do not require snapshot updates.

The log-signature unit tests were retired; these goldens are their only
guard. The mapping records which fixture replaced each one.

| Former test file | Regression / fixture prefix | Golden |
| --- | --- | --- |
| `log_signature.rs` | `excerpt_keeps_the_run_command_and_trailing_error_not_the_env_dump` | `parsed.json` |
| `log_signature.rs` | `excerpt_without_an_error_anchor_says_so_and_still_shows_the_command` | `parsed.json` |
| `log_signature.rs` | `error_signature_prefers_an_annotated_error_over_a_checkout_commit_message` | `parsed.json` |
| `log_signature.rs` | `generic_runner_trailer_does_not_collapse_distinct_unannotated_diagnostics` | `parsed.json` |
| `log_signature.rs` | `checkout_commit_message_containing_failure_is_not_the_signature` | `parsed.json` |
| `log_signature.rs` | `same_failure_under_a_different_commit_message_reuses_the_failure_key` | `parsed.json` |
| `log_signature.rs` | `passing_test_names_with_error_words_are_not_the_signature` | `parsed.json` |
| `log_signature.rs` | `distinct_rust_panics_with_the_same_passing_preamble_keep_distinct_keys` | `parsed.json` |
| `log_signature.rs` | `colored_and_uncolored_nextest_cancellation_share_the_fail_identity` | `parsed.json` |
| `log_signature.rs` | `a_truncated_escape_before_a_multibyte_character_still_files` | `parsed.json` |
| `log_signature.rs` | `nextest_fail_durations_do_not_fragment_one_regression` | `parsed.json` |
| `log_signature.rs` | `distinct_nextest_fail_lines_in_the_same_job_keep_distinct_keys` | `parsed.json` |
| `log_signature.rs` | `cargo_test_failed_trailer_does_not_outrank_the_panic` | `parsed.json` |
| `log_signature.rs` | `golden_assertion_help_text_does_not_outrank_the_failing_test_name` | `parsed.json` |
| `log_signature.rs` | `wrangler_error_outranks_generic_npx_process_failed` | `parsed.json` |
| `log_signature.rs` | `generic_only_truncated_excerpt_labels_step_name_fallback` | `parsed.json` |
