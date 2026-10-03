# MCP behavioral evidence

This matrix covers the exact 25 modern advertised names in `crates/orbit-cli/tests/snapshots/mcp_tools_list.json` after removal of the five deterministic drain operations and the consolidation of narrow tools into the tools that now carry their modes (friction listing into `orbit_search`, friction moves into `orbit_friction_update`, auto-task enable/disable into `orbit_auto_task_update`, drain resizing into `orbit_workflow_auto`, delivery evidence into `orbit_task_show`, crew discovery into `orbit_workspace_list`). It is a map of demonstrated behavior, not a claim that every argument, state, provider, or remote deployment was exercised. Retired desktop tool aliases are neither advertised nor callable. Clients and destinations must use the current domain contracts; no older-client translation is performed.

`S` means the production Orbit binary over real stdio and disposable stores. `W` means the MCP transport kernel over an in-memory wire or loopback fixture. `I` means registered runtime tools or application integration with temporary stores. Application-only evidence is labeled explicitly. A dash means this review has no named evidence for that dimension; it does not mean that behavior is unsupported. Persistence means an asserted durable readback or byte-preserving refusal, not merely a successful response.

| Advertised tool | Function | Refusal | Persistence | Workspace / authority | Limit |
| --- | --- | --- | --- | --- | --- |
| orbit_agent_invoke | S-agent | S-agent-denial | — | S-agent-denial | Synthetic CLI response; real providers untested |
| orbit_auto_task_add | S-auto-crud | — | S-auto-crud | — | Definition creation read back after server restart; unknown `required_tools` and owner routing untested |
| orbit_auto_task_list | S-auto-crud | S-replica | S-auto-crud | — | Ordinary list read after restart |
| orbit_auto_task_mint | S-auto-crud | — | S-auto-crud | — | Creates a proposed task without dispatch; scheduler cursor and persisted-requirement recheck untested |
| orbit_auto_task_update | S-auto-crud | S-auto-crud | S-auto-crud | S-auto-crud | Updated description and `enabled` read after restart; wrong workspace preserves bytes; stale checked toggle refused |
| orbit_command_exec | I-command | I-command-denial | I-command-audit | I-command-denial | Local safe argv fixtures; claim-holder refusal and remote command effects untested |
| orbit_friction_add | S-records | S-replica | S-records | S-replica | Disposable records |
| orbit_friction_update | S-records, S-rehome | S-rehome | S-records, S-rehome | S-selector, S-rehome | `rehome_to` move across two registered disposable checkouts; missing-record refusal untested |
| orbit_pipeline_invoke | — | S-domain | — | S-domain | Refusals only; default-input submission, explicit-input and provider completion untested |
| orbit_routine_control | S-domain | S-domain | S-domain | S-domain | List/toggle/restart; host scheduler not fired |
| orbit_search | S-search | S-search-denial | S-search | S-replica | Lexical consistency; no-query friction listing (A-friction-listing) and external semantic provider untested |
| orbit_task_add | S-records, S-guarded | S-context | S-guarded | S-replica, S-selector | Guarded proposed create and ordinary authoring |
| orbit_task_artifact_get | S-artifact | I-artifact-denial | S-artifact-write | S-artifact | Text/raster fixtures; no external download |
| orbit_task_artifact_put | S-artifact-write | S-artifact-write, I-artifact-authority | S-artifact-write | S-artifact-write, I-artifact-authority | Exact UTF-8 bytes after server restart; path and workspace refusals |
| orbit_task_list | S-records | S-selector | S-guarded | S-selector | Field projections, bounded pages |
| orbit_task_show | S-records, S-guarded, S-delivery | S-selector, S-delivery | S-guarded | S-selector, S-delivery | Global ID and explicit workspace filter; `field: "delivery"` reads bounded delivery evidence without run access |
| orbit_task_update | S-records, S-guarded | S-context, S-guarded | S-guarded | S-selector | Restart receipts and stale revision refusal |
| orbit_ui_inspect | S-presentation, W-presentation | W-presentation | — | S-presentation, W-presentation | Presentation read, native launcher untested |
| orbit_ui_open | S-presentation, W-presentation | — | — | S-presentation, W-presentation | Same data selection; native launcher untested |
| orbit_workflow_auto | S-domain, S-workers | S-domain, S-workers, I-workers | S-workers | S-domain, S-workers | Stdio status; `resize` on a disposable running record, no worker dispatched; drain start/stop untested |
| orbit_workflow_run_list | S-domain, I-runs | I-runs | S-domain | I-runs | Combined catalog observed over stdio |
| orbit_workflow_run_resume | S-resume | S-resume, I-runs | S-resume | S-resume, I-runs | Deterministic sleep-only job; retained successful checkpoint and retry lineage |
| orbit_workflow_run_show | S-resume, I-runs | I-runs | S-resume, I-runs | I-runs | Retry lineage read after server restart; unavailable evidence fixtures |
| orbit_workflow_ship | — | S-governed, I-runs | — | S-governed, I-runs | Refusals only; dispatch, in-flight guard and provider/GitHub delivery untested |
| orbit_workspace_list | S-federated, W-crews | S-selector, W-crews | — | S-federated, S-replica | Local machine-qualified routing and `include: ["crews"]` rows; live SSH destinations untested |

Named proofs are below. Suite totals are supporting execution evidence; the named assertion determines what each row claims. Schema snapshots and parser tests do not count as a successful business operation.

## Production stdio proofs

All `S` proofs are in `crates/orbit-cli/tests/mcp_roundtrip.rs` or its indicated submodule.

- **S-records**: `mcp_serve_round_trips_records_against_a_temp_workspace` creates, updates and reads task/friction records through production serialization and stores.
- **S-guarded**: `desktop::desktop_writes_reconcile_after_restart_and_reject_stale_or_implicit_destinations` proves create/comment receipt replay after process restart, stale revision refusal and explicit destination requirements; it also proves cached advertised and canonical retired aliases return `tool_not_found`.
- **S-domain**: `desktop::domain_automation_stdio_preserves_observed_routine_state_and_refuses_dispatch_without_authority` reads readiness/catalog, toggles a routine, reloads disabled state after server restart, checks stale toggle refusal preserves bytes, and checks disabled/mixed-input pipeline and missing resume/worker calls leave the run list empty. It checks unprivileged routine, readiness and ordinary pipeline calls are denied.
- **S-agent / S-agent-denial**: `a_remote_operator_session_invokes_an_agent_end_to_end` / `a_remote_agent_session_cannot_invoke_an_agent`; the agent executable is a local response stub.
- **S-replica**: `a_replica_mcp_session_enforces_checkout_capability_classes` checks coordination refusals, a permitted workspace listing with crews and absence of refused effects.
- **S-selector**: `every_workspace_scoped_tool_behavior_matches_its_own_selector_wording`, `mcp_task_show_follows_the_global_id_and_explicit_workspace_stays_a_filter`, and `mcp_workspace_argument_must_be_a_string_and_a_blank_one_defers_to_the_session` check dispatch/filter behavior, not just advertised wording.
- **S-federated**: `federated_mcp_serve_lists_and_routes_local_workspaces_without_destinations` (including the local row's own crews under `include: ["crews"]`) and `direct_and_federated_local_calls_record_equivalent_audit_contexts`.
- **S-rehome**: `mcp_friction_rehome_moves_a_record_into_its_registered_owner` asserts that an update with `rehome_to` leaves the target copy and source disposition, and that a repeat is refused.
- **S-context**: `mcp_task_add_and_update_validate_context_selectors`.
- **S-search / S-search-denial**: `task_mutations_are_immediately_searchable_from_the_cli_and_mcp` / `mcp_search_without_query_or_tag_keeps_its_refusal_message`.
- **S-artifact-write**: `transport_operations::stdio_artifact_put_reopens_intact_bytes_and_refuses_workspace_escape` attaches exact bytes, restarts the server, reads them back, and proves source/destination escapes and unknown workspace cannot alter the artifact projection.
- **S-auto-crud**: `transport_operations::stdio_auto_task_crud_mints_without_dispatch_and_reopens_definition_state` adds/updates/disables (with a checked `expected_enabled`, whose stale repeat is refused)/reloads a definition, mints a proposed task, proves a wrong-workspace update preserves bytes, shows MCP has no delete while the CLI refuses an open-mint deletion and forces one that keeps the minted task, and asserts no run was dispatched.
- **S-workers**: `transport_operations::stdio_workers_changes_one_running_record_and_preserves_it_on_stale_or_unauthorized_calls` seeds a disposable running record, adjusts its ceiling through `orbit_workflow_auto` `action: "resize"` over stdio (by `id`, then without one to target the single live drain), reopens the revision, and proves stale, wrong-workspace and unprivileged calls preserve state. No worker is spawned.
- **S-resume**: `transport_operations::stdio_resume_runs_only_deterministic_remaining_steps_and_reopens_checkpoint_lineage` resumes a failed two-step sleep-only job, observes success over stdio, reopens retry lineage, proves the successful checkpoint was retained, and refuses completed-run and unprivileged resume. The worker performs no provider dispatch.
- **S-artifact**: `mcp_task_artifact_get_follows_the_global_id_and_explicit_workspace_stays_a_filter`.
- **S-delivery**: `an_unprivileged_session_reads_bounded_delivery_evidence_but_not_the_run` reads `orbit_task_show` `field: "delivery"` but not the run.
- **S-governed**: `mcp_server_advertises_governed_tools_but_denies_an_unprivileged_session` and `a_remote_originated_agent_session_is_refused_a_governed_tool`. Advertisement alone grants nothing.
- **S-presentation**: `mcp_apps_presentation_reads_explicit_tasks_without_authority_or_workspace_fallback`.

## Internal distributed protocol

The five former public tools (`orbit_drain_probe`, `orbit_drain_receipt_lookup`,
`orbit_drain_claim_bind`, `orbit_drain_claim_settle`, `orbit_task_pull`) are absent
from ordinary local, federated and managed-agent discovery. Public `tools/call`
refuses their canonical and advertised spellings before admission or mutation.
They have no public compatibility aliases.

`internal_drain::public_drain_calls_and_spoofed_initialize_are_refused_and_audited`
uses production stdio and checks the 25-tool count, both retired spellings,
operator and managed-agent discovery, spoofed client/initialize claims, denied
custom RPC and durable refusal audit rows without changing task state.

`internal_drain::follower_internal_transport_reconciles_lost_admission_and_fences_claims`
exercises the production `SshDestinationProbe` and server through a disposable
SSH process substitute. Only the executable and destination filesystem are
substituted; Orbit composes the internal launch argv and sends its actual RPC.
The fixture probes, drops one committed admission reply, restarts the owner
session, looks up and replays the original receipt, binds, settles a failure,
and reads the original receipt and current phase again. It asserts one durable
claim and unchanged request identity, machine/run/stale-claim/workspace refusals,
and successful caller-attributed audit rows. Internal preflight negotiates a
protocol revision without loading general tool schemas. Existing application
proofs below cover capability floors and owner-local execution.

These isolated results do not establish a live SSH login, provider execution,
native-client integration, or successful remote candidate handoff/landing.

## Runtime and application proofs

The registered tool-host proofs are in `crates/orbit-core/src/adapter/tool_host/tests/`. They cross runtime authorization/dispatch and real temporary storage; they do not cross production stdio. Only refusals and secret-handling guards remain at this layer; positive behavior is proved over stdio above.

- **A-friction-listing**: no current proof that a queryless friction search lists every status oldest first, or of its list shape.
- **W-crews** (`orbit-mcp` `federated/tests/host.rs`, `remote/tests/discovery.rs`): `each_row_carries_the_crews_its_own_destination_resolved` and `include_accepts_crews_and_refuses_anything_else`.
- **I-command / I-command-audit / I-command-denial** (`command_tools.rs`): `secret_argv_reaches_child_but_is_redacted_in_the_audit_record` executes as the claim holder, returns stdout and records a redacted argv in the audit row; `auth_family_env_is_excluded_from_child_and_not_leaked_in_persisted_output`; refusals `managed_run_environment_denies_command_exec`, `working_directory_in_a_sibling_checkout_is_refused` and `working_directory_symlink_that_escapes_is_refused`. Refusal of an operator without the claim has no current test.
- **I-artifact-denial / I-artifact-authority** (`task_tools.rs`): `artifact_get::traversal_and_absolute_paths_are_refused_before_any_read` and `task_artifacts_retain_trusted_local_provenance_and_reject_ssh_mcp_attribution`.
- **I-runs** (`workflow_tools.rs`): `managed_run_environment_denies_ship_and_resume_end_to_end` and `mcp_run_show_rejects_a_recycled_pid_and_refuses_to_judge_a_foreign_namespace`. The page limit and the enriched list projection have no current test.
- **I-workers** (`crates/orbit-cli/tests/run_observation.rs`): `run_concurrency_updates_persisted_revision_and_refuses_stale_writes` raises the ceiling of a running run, records actor and reason, and refuses a stale revision without writing; `orbit-types` `workflow/tests/run_state.rs` (`setting_the_ceiling_records_what_it_replaced_and_advances_the_revision`, `a_stale_expected_revision_changes_nothing`) pins the record. The over-limit and zero-ceiling refusals have no current test.
- **I-resume**: `orbit-web` `api/tests/runs.rs::resume_job_run_endpoint_rejects_non_terminal_run_with_guard_reason` refuses a non-terminal source; `orbit-store` `driver/sqlite/job_run_store/tests/backend.rs` (`resume_insert_refuses_a_live_lineage_run_and_reopens_once_it_is_terminal`, `concurrent_resume_inserts_of_one_source_admit_exactly_one_run`) fences the retry lineage. Resume reconciliation idempotency has no current test.

The distributed admission proofs use isolated stores and in-process transports. No SSH or live owner was contacted.

- **I-pull / I-bind-settle / I-settle**: `crates/orbit-core/tests/distributed_drain.rs` drives a follower's drain against a real owner runtime: `lost_pull_and_bind_replies_recover_the_same_claim_and_leaf_exactly_once`, `three_failed_claims_open_the_breaker_and_a_new_drain_resets_it`, and `a_settlement_whose_reply_is_lost_is_redelivered_and_applied_once`.
- **I-receipt / I-pull-denial** (store level, `crates/orbit-store/tests/allocation_admission.rs`): `simultaneous_retries_of_one_request_create_one_claim`, `distinct_concurrent_requests_never_claim_overlapping_tasks`, and `handoff_authorization_follows_the_owner_policy_at_admission`. Session refusal is the `internal_drain` stdio proof above.
- **I-probe** and upgraded or incompatible receipt lookups have no current test.

## Wire and remaining gaps

**W-presentation** comprises `presentation_wire_preserves_dispatch_context_and_refuses_hidden_or_mismatched_reads` and `desktop_wire_preserves_explicit_destination_and_run_inspector_authority` in `crates/orbit-mcp/tests/mcp_wire_roundtrip.rs`. `client_handshake_does_not_advertise_retired_desktop_tools` proves that a client handshake cannot expand discovery with retired desktop tools and that current snapshot calls still dispatch.

`federated::tests::route::retired_desktop_calls_and_alias_only_destinations_are_refused_without_dispatch` checks that retired calls and destinations advertising only retired aliases are refused before delivery. `old_same_name_peer_without_extension_schema_refuses_before_dispatch` retains schema preflight for guarded fields. `guarded_domain_write_keeps_unknown_outcome_without_resubmission` checks that a lost reply is never resubmitted or translated into another operation.

Positive stdio resume, worker-ceiling adjustment, artifact writes and auto-task CRUD now have the isolated proofs above. They cover the stated transitions, not every legal argument or concurrent interleaving. Real SSH deployment, native launcher rendering, real provider invocation/completion, remote candidate settlement and GitHub delivery were not run. Parser/schema-only evidence covers legal field names and discovery shape; it proves neither admission nor a durable effect.
