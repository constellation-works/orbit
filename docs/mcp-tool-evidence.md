# MCP behavioral evidence

This matrix covers the exact 37 modern advertised names in `crates/orbit-cli/tests/snapshots/mcp_tools_list.json` on the consolidation candidate. It is a map of demonstrated behavior, not a claim that every argument, state, provider, or remote deployment was exercised. The five legacy advertisements are available only for the recognized pre-contract federated mux handshake and are covered separately below.

`S` means the production Orbit binary over real stdio and disposable stores. `W` means the MCP transport kernel over an in-memory wire or loopback fixture. `I` means registered runtime tools or application integration with temporary stores. Application-only evidence is labeled explicitly. A dash means this review has no named evidence for that dimension; it does not mean that behavior is unsupported. Persistence means an asserted durable readback or byte-preserving refusal, not merely a successful response.

| Advertised tool | Function | Refusal | Persistence | Workspace / authority | Limit |
| --- | --- | --- | --- | --- | --- |
| orbit_agent_invoke | S-agent | S-agent-denial | — | S-agent-denial | Synthetic CLI response; real providers untested |
| orbit_auto_task_add | S-auto-crud, I-auto-crud | I-auto-policy | S-auto-crud, I-owner | I-owner | Definition creation read back after server restart |
| orbit_auto_task_delete | S-auto-crud, I-auto-delete | S-auto-crud, I-auto-delete | S-auto-crud, I-auto-delete | — | Open-mint refusal preserves bytes; forced deletion retains minted task |
| orbit_auto_task_list | S-auto-crud, I-auto-list | S-replica | S-auto-crud, I-auto-crud | I-owner | Ordinary list read after restart; bounded extension also I-domain |
| orbit_auto_task_mint | S-auto-crud, I-auto-mint | I-auto-policy | S-auto-crud, I-auto-mint | I-domain | Creates a proposed task without dispatch; scheduler cursor proof I-auto-mint |
| orbit_auto_task_toggle | S-auto-crud, I-domain | I-domain | S-auto-crud, I-owner | I-owner | Ordinary toggle persisted across restart; checked stale toggle also I-domain |
| orbit_auto_task_update | S-auto-crud, I-auto-crud | S-auto-crud, I-auto-policy | S-auto-crud, I-owner | S-auto-crud, I-owner | Updated description read after restart; wrong workspace preserves bytes |
| orbit_command_exec | I-command | I-command-denial | I-command-audit | I-command-denial | Local safe argv fixtures; remote command effects untested |
| orbit_crew_list | S-replica | S-selector | — | S-federated | Read-only catalog |
| orbit_drain_claim_bind | I-bind-settle | I-bind-settle | I-bind-settle | I-bind-settle | Recording transport; no live follower |
| orbit_drain_claim_settle | I-bind-settle | I-settle | I-bind-settle | I-bind-settle | Recording transport; no real candidate delivery |
| orbit_drain_probe | I-probe | I-probe | I-probe | I-probe | Asserts observation creates no admission state |
| orbit_drain_receipt_lookup | I-receipt | I-receipt | I-receipt | I-receipt | Upgrade/version namespace fixtures |
| orbit_friction_add | S-records | S-replica | S-records | S-replica | Disposable records |
| orbit_friction_list | I-friction-list | S-replica | S-rehome | S-replica | Structured list and replica refusal |
| orbit_friction_rehome | S-rehome | S-rehome | S-rehome | S-rehome | Two registered disposable checkouts |
| orbit_friction_update | S-records | I-friction-update | S-records | S-selector | Runtime missing-record refusal |
| orbit_pipeline_invoke | I-domain | S-domain | I-domain | S-domain | Durable default-input submission uses synthetic worker; public explicit-input/provider completion untested |
| orbit_routine_control | S-domain | S-domain | S-domain | S-domain | List/toggle/restart; host scheduler not fired |
| orbit_search | S-search | S-search-denial | S-search | S-replica | Lexical consistency; external semantic provider untested |
| orbit_task_add | S-records, S-guarded | S-context | S-guarded | S-replica, S-selector | Guarded proposed create and ordinary authoring |
| orbit_task_artifact_get | S-artifact | I-artifact-denial | I-artifact | S-artifact | Text/raster fixtures; no external download |
| orbit_task_artifact_put | S-artifact-write | S-artifact-write, I-artifact-authority | S-artifact-write | S-artifact-write, I-artifact-authority | Exact UTF-8 bytes after server restart; path and workspace refusals |
| orbit_task_list | S-records | S-selector | S-guarded | S-selector | Field projections, bounded pages |
| orbit_task_pull | I-pull | I-pull-denial | I-pull | I-pull-denial | Recording transport; no live remote owner |
| orbit_task_show | S-records, S-guarded | S-selector | S-guarded | S-selector | Global ID and explicit workspace filter |
| orbit_task_update | S-records, S-guarded | S-context, S-guarded | S-guarded | S-selector | Restart receipts and stale revision refusal |
| orbit_ui_inspect | S-presentation, W-presentation | W-presentation | — | S-presentation, W-presentation | Presentation read, native launcher untested |
| orbit_ui_open | S-presentation, W-presentation | — | — | S-presentation, W-presentation | Same data selection; native launcher untested |
| orbit_workflow_auto | S-domain, I-drain | S-domain, I-drain | I-drain | S-domain | Stdio status; start/stop runtime fixture, no provider delivery |
| orbit_workflow_run_delivery | S-delivery | S-delivery | — | S-delivery | Agent can read bounded delivery evidence without run access |
| orbit_workflow_run_list | S-domain, I-runs | I-runs | S-domain | I-runs | Combined catalog observed over stdio |
| orbit_workflow_run_resume | S-resume | S-resume, I-runs | S-resume | S-resume, I-runs | Deterministic sleep-only job; retained successful checkpoint and retry lineage |
| orbit_workflow_run_show | S-resume, I-runs | I-runs | S-resume, I-runs | I-runs | Retry lineage read after server restart; unavailable evidence fixtures |
| orbit_workflow_run_workers | S-workers | S-workers, I-workers | S-workers | S-workers | Disposable running record; no provider or detached worker dispatched |
| orbit_workflow_ship | I-ship | I-ship, S-governed | I-ship | I-ship | Synthetic dispatch; actual provider/GitHub delivery untested |
| orbit_workspace_list | S-federated | S-selector | — | S-federated | Local machine-qualified routing; live SSH destinations untested |

Named proofs are below. Suite totals are supporting execution evidence; the named assertion determines what each row claims. Schema snapshots and parser tests do not count as a successful business operation.

## Production stdio proofs

All `S` proofs are in `crates/orbit-cli/tests/mcp_roundtrip.rs` or its indicated submodule.

- **S-records**: `mcp_serve_round_trips_records_against_a_temp_workspace` creates, updates and reads task/friction records through production serialization and stores.
- **S-guarded**: `desktop::desktop_writes_reconcile_after_restart_and_reject_stale_or_implicit_destinations` proves create/comment receipt replay after process restart, stale revision refusal and explicit destination requirements; it also exercises cached legacy aliases.
- **S-domain**: `desktop::domain_automation_stdio_preserves_observed_routine_state_and_refuses_dispatch_without_authority` reads readiness/catalog, toggles a routine, reloads disabled state after server restart, checks stale toggle refusal preserves bytes, and checks disabled/mixed-input pipeline and missing resume/worker calls leave the run list empty. It checks unprivileged routine, readiness and ordinary pipeline calls are denied.
- **S-agent / S-agent-denial**: `a_remote_operator_session_invokes_an_agent_end_to_end` / `a_remote_agent_session_cannot_invoke_an_agent`; the agent executable is a local response stub.
- **S-replica**: `a_replica_mcp_session_enforces_checkout_capability_classes` checks coordination refusals, permitted crew inspection and absence of refused effects.
- **S-selector**: `every_workspace_scoped_tool_behavior_matches_its_own_selector_wording`, `mcp_task_show_follows_the_global_id_and_explicit_workspace_stays_a_filter`, and `mcp_workspace_argument_must_be_a_string_and_a_blank_one_defers_to_the_session` check dispatch/filter behavior, not just advertised wording.
- **S-federated**: `federated_mcp_serve_lists_and_routes_local_workspaces_without_destinations` and `direct_and_federated_local_calls_record_equivalent_audit_contexts`.
- **S-rehome**: `mcp_friction_rehome_moves_a_record_into_its_registered_owner` asserts target copy/source disposition and repeated refusal.
- **S-context**: `mcp_task_add_and_update_validate_context_selectors`.
- **S-search / S-search-denial**: `task_mutations_are_immediately_searchable_from_the_cli_and_mcp` / `mcp_search_without_query_or_tag_keeps_its_refusal_message`.
- **S-artifact-write**: `transport_operations::stdio_artifact_put_reopens_intact_bytes_and_refuses_workspace_escape` attaches exact bytes, restarts the server, reads them back, and proves source/destination escapes and unknown workspace cannot alter the artifact projection.
- **S-auto-crud**: `transport_operations::stdio_auto_task_crud_mints_without_dispatch_and_reopens_definition_state` adds/updates/toggles/reloads a definition, mints a proposed task, proves open-mint deletion and wrong-workspace update preserve bytes, forces definition deletion while retaining the task, and asserts no run was dispatched.
- **S-workers**: `transport_operations::stdio_workers_changes_one_running_record_and_preserves_it_on_stale_or_unauthorized_calls` seeds a disposable running record, adjusts its ceiling through stdio, reopens the revision, and proves stale, wrong-workspace and unprivileged calls preserve state. No worker is spawned.
- **S-resume**: `transport_operations::stdio_resume_runs_only_deterministic_remaining_steps_and_reopens_checkpoint_lineage` resumes a failed two-step sleep-only job, observes success over stdio, reopens retry lineage, proves the successful checkpoint was retained, and refuses completed-run and unprivileged resume. The worker performs no provider dispatch.
- **S-artifact**: `mcp_task_artifact_get_follows_the_global_id_and_explicit_workspace_stays_a_filter`.
- **S-delivery**: `an_unprivileged_session_reads_bounded_delivery_evidence_but_not_the_run`.
- **S-governed**: `mcp_server_advertises_governed_tools_but_denies_an_unprivileged_session` and `a_remote_originated_agent_session_is_refused_a_governed_tool`. Advertisement alone grants nothing.
- **S-presentation**: `mcp_apps_presentation_reads_explicit_tasks_without_authority_or_workspace_fallback`.

## Runtime and application proofs

The registered tool-host proofs are in `crates/orbit-core/src/adapter/tool_host/tests/` and passed together in the 227-test tool-host run. They cross runtime authorization/dispatch and real temporary storage; they do not cross production stdio.

- **I-auto-crud** (`auto_task_tools.rs`): `explicit_template_complexity_roundtrips_through_tools_and_minting`.
- **I-auto-list / I-auto-mint** (`auto_task_tools.rs`): `list_returns_every_definition_through_the_tool_surface` / `mint_returns_the_minted_task_with_its_provenance_tag` (also asserts the scheduler cursor is untouched).
- **I-auto-policy** (`auto_task_tools.rs`): `auto_task_add_rejects_unknown_required_tools_with_suggestions`, `auto_task_update_rejects_unknown_required_tools_with_suggestions`, `auto_task_mint_rechecks_persisted_template_requirements`.
- **I-auto-delete** (`auto_task_tools.rs`): `delete_refuses_an_open_mint_then_forces_and_reports_through_the_tool_surface`.
- **I-owner** (`runtime/tests/worker_coordination.rs`): `owner_routing_fences_generic_writes_across_separate_stores` asserts host-brokered add/update/toggle land on the owner and wrong-workspace/shadow definitions cannot change it.
- **I-domain / I-drain** (`desktop_tools.rs`): `domain_automation_scopes_definitions_checks_conflicts_and_mints_without_dispatch` / `desktop_drain_readiness_and_idle_stop_reuse_runtime_without_dispatch`, `desktop_drain_persists_bounded_window_and_explicit_completion_policy`, and `desktop_drain_requires_operator_and_validates_before_dispatch`. Pipeline default submission is a persisted run whose worker is replaced with a bounded shell fixture; it is not provider completion evidence.
- **I-command / I-command-audit / I-command-denial** (`command_tools.rs`): `claim_holder_executes_and_receives_stdout_stderr_and_exit_status`, `audit_record_carries_argv_working_directory_caller_and_workspace`, `operator_without_the_claim_is_refused`, `managed_run_environment_denies_command_exec`, `working_directory_in_a_sibling_checkout_is_refused`, and `working_directory_symlink_that_escapes_is_refused`.
- **I-friction-list / I-friction-update** (`friction_tools.rs`): `list_default_is_always_the_legacy_array`, `list_rejects_unknown_response_modes`, and `update_of_a_missing_record_is_not_found_and_invalid_input_is_preserved`.
- **I-artifact / I-artifact-denial / I-artifact-authority** (`task_tools.rs`): `artifact_get::raster_images_survive_attach_list_and_read_with_intact_bytes`, `artifact_get::text_artifacts_are_returned_as_utf8_rather_than_base64`, `artifact_get::traversal_and_absolute_paths_are_refused_before_any_read`, `artifact_get::an_unknown_task_fails_closed_before_any_artifact_lookup`, and `task_artifacts_retain_trusted_local_provenance_and_reject_ssh_mcp_attribution`.
- **I-runs** (`workflow_tools.rs`): `operator_can_observe_runs_and_agent_denial_is_audited`, `managed_run_environment_denies_ship_and_resume_end_to_end`, `run_list_refuses_a_limit_above_200`, `mcp_run_show_projects_bounded_recovery_evidence_without_replacing_run_error`, and `mcp_run_list_preserves_enriched_default_projection_across_a_mixed_page`.
- **I-ship** (`workflow_tools.rs`): `ship_tool_inherits_the_shared_in_flight_guard`, `ship_tool_records_mcp_provenance_only_for_an_mcp_session`, and `ship_tool_parses_and_rejects_an_unknown_crew_allowlist_before_dispatch`.
- **I-workers** (application-only `application/job/run/tests/worker_limit.rs`): `raising_the_ceiling_preserves_the_run_and_records_what_changed`, `a_stale_revision_is_refused_as_a_conflict_and_writes_nothing`, and `an_over_limit_or_zero_ceiling_is_refused_before_any_write`.
- **I-resume** (application-only `application/job/tests/resume.rs`): `resume_reconciliation_is_idempotent_and_scoped_to_the_retry_lineage` and `resume_submission_rejects_a_non_terminal_run_before_persisting_anything`. Delivery-tail steps and worker processes are deterministic fixtures.

The distributed proofs are in `crates/orbit-core/src/application/distributed/tests/`, use recording transports and passed together as 34 tests. No SSH or live owner was contacted.

- **I-probe**: `probe::probe_reports_owner_facts_and_creates_no_admission_state`, `probe::probe_reports_the_first_refusal_the_admission_ladder_would_raise`, and `probe::the_read_only_surface_serves_an_owner_local_session_and_refuses_a_replica`.
- **I-receipt**: `probe::an_upgraded_lookup_finds_the_original_receipt_without_rewriting_it`, `probe::an_incompatible_lookup_protocol_refuses_instead_of_answering`, and `probe::a_worker_reads_its_own_namespace_and_only_an_operator_reads_across_attempts`.
- **I-pull / I-pull-denial**: `serve::a_follower_pull_claims_one_task_for_its_own_machine_and_replays_the_receipt`, `serve::a_new_request_must_carry_the_ship_contract_the_owner_resolves_now`, `serve::a_replica_serves_no_mutating_entry_point`, and `serve::a_pull_requires_agent_capability_on_the_session`.
- **I-bind-settle / I-settle**: `serve::bind_and_failure_settlement_are_fenced_to_the_admitted_machine_and_run` and `serve::settlement_accepts_only_a_handoff_or_a_failure`.

## Wire, compatibility and remaining gaps

**W-presentation** comprises `presentation_wire_preserves_dispatch_context_and_refuses_hidden_or_mismatched_reads` and `desktop_wire_preserves_explicit_destination_and_run_inspector_authority` in `crates/orbit-mcp/tests/mcp_wire_roundtrip.rs`. That file also contains `old_mux_handshake_preserves_discovery_and_domain_dispatch_without_modern_advertisement_growth`: old mux discovery gets the five needed aliases, while modern discovery remains at the reduced surface. This is a real protocol handshake with a synthetic host, not execution of an installed historical binary.

`federated::tests::route::old_same_name_peer_without_extension_schema_refuses_before_dispatch` checks that snapshot/view/default-input and every guarded task field are refused on old same-name schemas before any destination call. `domain_clients_negotiate_legacy_peers_before_dispatch_and_keep_unknown_outcomes` checks a post-dispatch lost reply remains unknown and is never resubmitted.

Positive stdio resume, worker-ceiling adjustment, artifact writes and auto-task CRUD now have the isolated proofs above. They cover the stated transitions, not every legal argument or concurrent interleaving. Real remote deployment, an installed historical mux binary, native launcher rendering, real provider invocation/completion, remote candidate settlement and GitHub delivery were not run. Parser/schema-only evidence covers legal field names and discovery shape; it proves neither admission nor a durable effect.
