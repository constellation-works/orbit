# orbit-config unit-test audit delivery

The pinned 2026-10-12 audit owned 27 identities. This delivery removes all 27
from `src` and its baseline: 23 cases move unchanged to existing integration
targets, one redundant symlink case retires against existing boundary coverage,
and the three designated retirements are removed. No production visibility or
behavior changes. The credential environment case runs in an isolated child.

The three audited admissions remain: the malformed-pool table, exhaustive
family-subset renderer table, and missing/disabled pool-member fault injection.
The later host-shutdown horizon admission also remains unchanged. Only the
orbit-config baseline section is regenerated; other crates retain their entries.

| Audited identity | Result / boundary |
| --- | --- |
| `resolved/tests/execution_env.rs::config_admission_pins_inherit_off` | Moved to `default_crew::execution_env::config_admission_pins_inherit_off` through the existing public API. |
| `resolved/tests/execution_env.rs::default_policy_excludes_benignly_named_ambient_credentials` | Moved to `default_crew::execution_env::default_policy_excludes_benignly_named_ambient_credentials` through the existing public API. |
| `tests/crew_pools.rs::final_recovery_pool_admits_weighted_pools_and_empty_disables_it` | Moved to `default_crew::crew_pools::final_recovery_pool_admits_weighted_pools_and_empty_disables_it` through the existing public API. |
| `tests/crew_pools.rs::final_recovery_pool_default_is_valid_and_shows_in_config` | Retired: built-in policy/getter pin; the moved pool-load cases cover admission. |
| `tests/crew_pools.rs::unset_final_recovery_pool_keeps_only_crews_a_custom_registry_defines` | Moved to `default_crew::crew_pools::unset_final_recovery_pool_keeps_only_crews_a_custom_registry_defines` through the existing public API. |
| `tests/crew_pools.rs::user_authored_terra_crew_still_loads_and_resolves_in_complexity_pool` | Moved to `default_crew::crew_pools::user_authored_terra_crew_still_loads_and_resolves_in_complexity_pool` through the existing public API. |
| `tests/layering.rs::workspace_crew_field_override_keeps_global_crew_fields_and_other_crews` | Moved to `default_crew::layering::workspace_crew_field_override_keeps_global_crew_fields_and_other_crews` through the existing public API. |
| `tests/layering.rs::workspace_file_does_not_inherit_security_relevant_global_keys` | Moved to `default_crew::layering::workspace_file_does_not_inherit_security_relevant_global_keys` through the existing public API. |
| `tests/machine.rs::a_workspace_machine_table_is_refused_at_load_naming_the_file` | Moved to `review_layers::machine::a_workspace_machine_table_is_refused_at_load_naming_the_file` through the existing public API. |
| `tests/operation.rs::box_ws_orbit_review_config_migrates_with_warnings` | Moved to `review_layers::operation::box_ws_orbit_review_config_migrates_with_warnings` through the existing public API. |
| `tests/operation.rs::host_evidence_rules_load_and_a_workspace_list_replaces_the_global_one` | Moved to `review_layers::operation::host_evidence_rules_load_and_a_workspace_list_replaces_the_global_one` through the existing public API. |
| `tests/operation.rs::host_evidence_rules_orbit_could_never_fulfil_fail_the_load` | Moved to `review_layers::operation::host_evidence_rules_orbit_could_never_fulfil_fail_the_load` through the existing public API. |
| `tests/operation.rs::invalid_legacy_values_still_fail_closed` | Moved to `review_layers::operation::invalid_legacy_values_still_fail_closed` through the existing public API. |
| `tests/operation.rs::legacy_after_landing_policy_asks_for_the_auto_task_not_before_pr` | Moved to `review_layers::operation::legacy_after_landing_policy_asks_for_the_auto_task_not_before_pr` through the existing public API. |
| `tests/operation.rs::legacy_before_pr_policy_turns_before_pr_on_with_a_deprecation_warning` | Moved to `review_layers::operation::legacy_before_pr_policy_turns_before_pr_on_with_a_deprecation_warning` through the existing public API. |
| `tests/operation.rs::legacy_none_policy_turns_neither_on` | Moved to `review_layers::operation::legacy_none_policy_turns_neither_on` through the existing public API. |
| `tests/operation.rs::omitted_review_table_is_off_with_the_default_minutes` | Retired: built-in review policy/getter pin. |
| `tests/operation.rs::retired_review_keys_are_not_settable` | Moved to `review_layers::operation::retired_review_keys_are_not_settable` through the existing public API. |
| `tests/operation.rs::review_table_sets_before_pr_and_minutes_with_their_layer` | Moved to `review_layers::operation::review_table_sets_before_pr_and_minutes_with_their_layer` through the existing public API. |
| `tests/operation.rs::review_table_wins_over_its_legacy_spelling_in_the_same_file` | Moved to `review_layers::operation::review_table_wins_over_its_legacy_spelling_in_the_same_file` through the existing public API. |
| `tests/plugin_enablement.rs::a_symlinked_workspace_config_fails_closed_as_a_policy_layer` | Retired duplicate: `config_paths::symlink_leaves_cannot_supply_external_config_or_toggles` calls `assert_loaders_refuse`, which checks `workspace_config_sets_policy` fails closed for the same external toggle symlink. |
| `tests/plugin_enablement.rs::a_toggle_only_workspace_file_keeps_the_security_keys_inherited` | Moved to `config_paths::plugin_enablement::a_toggle_only_workspace_file_keeps_the_security_keys_inherited` through the existing public API. |
| `tests/plugin_enablement.rs::global_plugin_enablement_is_refused_at_load` | Moved to `config_paths::plugin_enablement::global_plugin_enablement_is_refused_at_load` through the existing public API. |
| `tests/seed.rs::host_with_no_cli_seeds_all_crews_disabled_and_no_lane_keys` | Retired: zero-family row is already covered by the admitted exhaustive renderer table. |
| `tests/seed.rs::seeded_and_builtin_configs_load_valid_crew_registries` | Moved to `default_crew::seed::seeded_and_builtin_configs_load_valid_crew_registries` through the existing public API. |
| `tests/store.rs::open_refuses_a_symlinked_config_file` | Moved to `config_store::admission::open_refuses_a_symlinked_config_file` through the existing public API. |
| `tests/store.rs::set_rejects_invalid_value_and_leaves_file_byte_identical` | Moved to `config_store::admission::set_rejects_invalid_value_and_leaves_file_byte_identical` through the existing public API. |

Source drift: the two audited `tests/layering.rs` identities were already in
`src/layering/tests/load.rs` on the delivery base. Both move to the public
loader boundary with their original inputs and assertions. Empty resolved and
layering test modules, retired store/operation/plugin modules, and unused seed
filesystem support are removed. Private renderer seams and fixtures remain.

Boundary modules are `include!`d into the single existing `config` test binary
(`crates/orbit-config/tests/config/main.rs`); no new integration binary is
introduced. Public helpers prove these cases without CLI/provider setup.
