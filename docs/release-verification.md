# Part 11 execution checklist and acceptance gap matrix

This matrix is the final release audit companion to the exact per-scenario test
references in [`spec/implementation/acceptance-map.md`](../spec/implementation/acceptance-map.md).
The business specification and checked-in OpenAPI remain normative. “Existing
proof” names only executable evidence inspected in the baseline; “Part 11
closure” identifies evidence that the earlier gates did not supply. Passing is
recorded only after the commands in the final column have run.

## Execution checklist

1. Verify main/Part 10 ancestry, clean index/tree, identity, branches, remotes,
   worktrees, and pending operations; create/resume the Part 11 branch.
2. Audit production source, schema v2 migrations, locked dependencies, scripts,
   test fixtures, and current operator documentation. Keep PostgreSQL fixture
   operations under the existing process/interprocess harness locks.
3. Run real A/B/C production executables against the same isolated PostgreSQL
   schema for concurrent HTTP allowances, independent analytics, authority,
   queued-event deletion, drain, and replacement. Use observed database locks
   and captured queue metrics, not delays, to establish ordering.
4. Validate the entire checked-in OpenAPI 3.1 file using its pinned upstream
   schema, compile all JSON Schema 2020-12 roots, resolve references, compare
   all 22 operation IDs/methods/paths, and probe actual production HTTP behavior.
5. Run the pinned NGINX proxy over TCP with separate static files; record
   container-to-host and container-to-container peer addresses before setting
   proxy trust; traverse actual maximum header-derived report cursors.
6. Build a locked Linux release package, verify its files/provenance/checksums,
   and use that packaged binary for explicit migrate/bootstrap, HTTP smoke,
   deletion, natural shutdown, and restart without reseeding.
7. Run the full Rust/PostgreSQL/process/Collector gate and the separate release
   gate locally; record platform/image/tool versions, counts, durations, artifact
   hashes, and sanitized captures.
8. Replace historical/generic acceptance pointers with exact executable tests,
   review the full diff, commit the verified part and handoff, then perform the
   explicit no-ff local merge.

## Observed local results — 2026-10-29

| Command | Observed result |
|---|---|
| `sh scripts/check.sh` | Passed on the feature implementation before its one-line NGINX version-comment correction: **224 all-target tests plus 3 doctests**, 0 failures and 0 ignored. It ran formatting, locked check/Clippy/tests, doctests, and the independent-process/proxy/release-smoke suites. |
| `sh scripts/check-openapi.sh` | Passed on verified implementation `95d053d70c47c7199cfbd45545fa8c08b8d945d1`: 3 tests, 0 failures/ignored. |
| `sh scripts/check-proxy.sh` | Passed on clean source at `95d053d70c47c7199cfbd45545fa8c08b8d945d1`: 1 production TCP proxy test, 0 failures/ignored; clean-source package provenance verified. |
| `sh scripts/check-release.sh` | Passed on clean source at the same revision: 5 tests (3 OpenAPI, 1 proxy, 1 packaged release smoke), 0 failures/ignored. |

The host was macOS with Podman 6.1.4, PostgreSQL 17.6, Rust/Cargo 1.99.1,
NGINX 1.30.6, and the digest-pinned Debian Bookworm Rust 1.99.1 builder. The
locally built artifact targets `aarch64-unknown-linux-gnu`; this is not evidence
for other operating systems or architectures. The final ignored local artifact
is `target/release-packages/ferl-server-0.1.0-aarch64-unknown-linux-gnu.tar.gz`,
SHA-256
`07d7641e88e852d6891b459c7dbd18f7d0a1db7f0b23850418d7b22718ab5d17`. Its
provenance records source revision `95d053d70c47c7199cfbd45545fa8c08b8d945d1`,
`source_tree_clean=true`, 17 inventoried files, OpenAPI SHA-256
`45677d0d0c0afda4687fada791ce492682ad546f2fbc612113890cffecbb6b5e`, and
binary SHA-256
`76fb42bffd1e224c616b87ca19b22ec52201e9fe101d4a7954a247bab3c32919`.

The macOS host-gateway probe observed backend peer `127.0.0.1`; it was not added
to the trusted-proxy list. On the isolated Podman network the observed NGINX
peer was `10.89.0.4`, direct untrusted peer `10.89.0.3`, and NGINX-observed
client peer `10.89.0.6`; the effective forwarded IP matched the latter. The
harness removed its owned proxy/server containers and network; the pre-existing
PostgreSQL fixture remains running.

All 51 scenario rows in `spec/implementation/acceptance-map.md` point to exact
executable evidence. Hosted CI was not run. Rust 1.94 compilation and an
end-to-end TLS handshake were not run and are not claimed. The full-gate
pre-correction package hash is intentionally not presented as a release
artifact; only the clean-source checksum above is the final local package
evidence.

## Scenario-by-scenario gap matrix

`sh scripts/check.sh` is the consolidated gate for Rust, PostgreSQL, account,
link, discovery, analytics, independent-process, migration, Collector/privacy,
and lifecycle suites. Dedicated release gates are named in the final column.
The acceptance map contains the full names and assertion details for the
pre-existing tests referenced compactly here.

| # | Existing executable evidence at baseline | Part 11 gap and closure | Evidence level; final command |
|---:|---|---|---|
| 1 | `tests/link_creation.rs::production_create_routes_return_complete_owned_and_anonymous_representations`; `tests/token_rotation.rs::token_replacement_is_scoped_bodyless_one_time_and_survives_creation_disable` | Cross-process credentials/state are checked by `tests/link_process.rs::independent_production_servers_share_link_state_allowance_and_restart_persistence`; packaged execution is checked by the release smoke. | PostgreSQL + HTTP + independent processes + package; `sh scripts/check.sh`; `sh scripts/check-release.sh` |
| 2 | `tests/accounts.rs::fresh_authentication_role_matrix_and_secret_rotation_are_exact`; `tests/authentication.rs::failing_credential_and_persistence_paths_emit_only_safe_diagnostics` | A/B public-route authority is also exercised by production-process suites; no anonymous fallback for supplied invalid credentials. | HTTP + PostgreSQL + independent processes; `sh scripts/check.sh` |
| 3 | `tests/link_authorization.rs::public_link_operations_apply_fresh_object_authority_and_conceal_other_objects`; `tests/analytics_authorization.rs::reports_reauthorize_every_actor_conceal_objects_and_check_authority_before_query_work` | Link and report authority freshness is rechecked through A/B/C TCP processes. | PostgreSQL + HTTP + independent processes; `sh scripts/check.sh` |
| 4 | `tests/token_rotation.rs::token_replacement_is_scoped_bodyless_one_time_and_survives_creation_disable`; `tests/analytics_authorization.rs::reports_reauthorize_every_actor_conceal_objects_and_check_authority_before_query_work` | Token replacement and cursor renewal cross A/B/C in `tests/report_process.rs::report_authority_history_and_maximum_breakdown_cursors_cross_tcp_instances_and_volume_restart`. | HTTP + independent processes; `sh scripts/check.sh` |
| 5 | `tests/bootstrap.rs::{concurrent_bootstrap_processes_have_exactly_one_winner,bootstrap_result_is_one_time_private_and_survives_postgres_restart}` | The packaged executable explicitly migrates and bootstraps once, then restarts without reseeding. | PostgreSQL + independent commands + packaged release; `sh scripts/check.sh`; `sh scripts/check-release.sh` |
| 6 | `tests/accounts.rs::fresh_authentication_role_matrix_and_secret_rotation_are_exact`; `tests/account_process.rs::independent_servers_share_fresh_credentials_account_cursors_and_last_admin_gate` | The packaged binary verifies initial key output, rotation, revocation, and persistence after restart. | HTTP + independent processes + packaged release; `sh scripts/check-release.sh` |
| 7 | `tests/accounts.rs::account_admin_changes_preserve_noops_disabled_resets_and_last_admin`; `tests/redirects.rs::enablement_suspension_and_deletion_apply_to_fresh_public_resolutions` | Cross-process suspension and re-enable visibility is exercised by account/link production-process evidence. | PostgreSQL + HTTP + independent processes; `sh scripts/check.sh` |
| 8 | `tests/account_concurrency.rs::controlled_last_admin_removal_races_never_commit_zero_enabled_admins` | Cross-process self-disable race and final admin invariant: `tests/account_process.rs::independent_servers_share_fresh_credentials_account_cursors_and_last_admin_gate`. | PostgreSQL + independent processes; `sh scripts/check.sh` |
| 9 | `tests/accounts.rs::account_deletion_cascades_children_keeps_reservation_and_allows_clean_username_reuse`; `tests/persistence_constraints.rs::link_and_account_deletion_cascade_children_but_rollback_is_atomic` | Release smoke proves link code reservation after delete/restart; database tests cover account username reuse/new UUID/no inheritance. | PostgreSQL + HTTP + packaged release; `sh scripts/check.sh`; `sh scripts/check-release.sh` |
| 10 | `tests/domain_contracts.rs::url_normalization_and_persisted_snapshot_checks_are_precise`; `tests/link_creation.rs::production_create_routes_return_complete_owned_and_anonymous_representations` | Actual HTTP/process flow remains network-isolated and does not fetch destinations; server telemetry contains no URLs. | Pure + PostgreSQL + HTTP + telemetry; `sh scripts/check.sh` |
| 11 | `src/domain/short_code.rs::tests::syntax_is_exact_and_reservation_conflicts_are_out_of_scope`; `tests/link_creation.rs::production_create_routes_return_complete_owned_and_anonymous_representations` | Database allocation race across independent production HTTP processes: `tests/link_process.rs::independent_production_servers_share_link_state_allowance_and_restart_persistence`. | Pure + PostgreSQL + independent processes; `sh scripts/check.sh` |
| 12 | `tests/persistence_constraints.rs::{accounts_reservations_roles_and_digest_constraints_are_authoritative,link_ownership_digest_counter_and_immutable_identity_constraints_hold}` | Actual namespace and deletion behavior also traverses the pinned proxy. | PostgreSQL + HTTP + actual proxy; `sh scripts/check.sh`; `sh scripts/check-proxy.sh` |
| 13 | `tests/persistence_constraints.rs::link_ownership_digest_counter_and_immutable_identity_constraints_hold`; `tests/link_management.rs::patch_detail_noops_and_delete_are_atomic_and_keep_permanent_reservations` | Fresh cross-process reads/redirects and retained reservation are covered by A/B and packaged restart flows. | PostgreSQL + HTTP + independent processes; `sh scripts/check.sh`; `sh scripts/check-release.sh` |
| 14 | `tests/redirects.rs::redirect_and_visit_use_one_immutable_resolution_snapshot`; `tests/routing.rs::raw_path_ownership_methods_and_fallbacks_are_exact` | Actual proxy GET/HEAD preserves status, Location, headers and zero body bytes. | HTTP + actual proxy; `sh scripts/check-proxy.sh` |
| 15 | `tests/routing.rs::{raw_path_ownership_methods_and_fallbacks_are_exact,resolution_storage_failure_is_not_fabricated_as_missing}` | Proxy path-alias and unavailable-code behavior is verified through TCP. | HTTP + actual proxy; `sh scripts/check-proxy.sh` |
| 16 | `tests/creation_order.rs::default_public_create_limits_charge_five_anonymous_and_sixty_account_attempts`; `src/rate_limit/tests.rs::exact_microsecond_window_rounding_equal_times_and_reduced_limits` | Combined real A/B account and effective-IP HTTP allowances, exact durable charges, and Retry-After: `tests/multi_instance_process.rs::production_http_allowances_are_one_durable_account_and_peer_budget_across_a_b_c`. | PostgreSQL + independent-process HTTP; `sh scripts/check.sh` |
| 17 | `tests/creation_order.rs::every_eligible_invalid_attempt_is_charged_after_prepare`; `tests/rate_limits.rs::eligibility_and_body_order_preserve_independently_committed_charges` | Cross-process HTTP malformed/conflict attempts and later rejected attempts are charged exactly once by `multi_instance_process`. | HTTP + PostgreSQL + independent processes; `sh scripts/check.sh` |
| 18 | `tests/rate_limit_concurrency.rs::concurrent_first_use_shared_guards_and_unique_buckets_admit_exact_limit`; `tests/rate_limit_process.rs::independent_processes_share_allowance_and_restarts_preserve_durable_charges` | Actual public createLink HTTP is split across A/B/C; key rotation and process replacement do not reset either allowance. | PostgreSQL + independent-process HTTP; `sh scripts/check.sh` |
| 19 | `tests/client_ip.rs::{trust_walk_validates_the_complete_chain_and_ignores_other_headers,malformed_duplicate_missing_and_oversized_chains_fall_back_atomically,mapped_configuration_preserves_exact_prefix_membership}` | Proxy peer/client identities are read from persisted visits and NGINX's safe log before trust is configured; direct spoof is checked separately. | PostgreSQL + actual proxy + HTTP; `sh scripts/check-proxy.sh` |
| 20 | `tests/discovery_process.rs::discovery_cursors_and_fresh_state_cross_independent_servers_and_volume_restart`; populated-upgrade tests in `tests/migrations.rs` | Packaged release restarts against the same isolated schema without fixture repopulation. | PostgreSQL + independent processes + packaged release; `sh scripts/check-release.sh` |
| 21 | `tests/visit_records.rs::actual_redirect_snapshots_and_visit_pages_are_reported_after_delayed_worker_persistence`; `tests/report_process.rs::report_authority_history_and_maximum_breakdown_cursors_cross_tcp_instances_and_volume_restart` | Independent A/B HTTP redirects concurrently persist unique rows and equal counters. | PostgreSQL + HTTP + independent processes; `sh scripts/check.sh` |
| 22 | `tests/visit_ingestion.rs::optional_headers_obey_complete_utf8_duplicate_and_byte_limit_rules`; `tests/report_process.rs::report_authority_history_and_maximum_breakdown_cursors_cross_tcp_instances_and_volume_restart` | Maximum legal header values and actual report cursors also traverse the configured NGINX TCP path. | HTTP + independent processes + actual proxy; `sh scripts/check-proxy.sh` |
| 23 | `analytics::reports::tests::deterministic_worker_fixtures_cover_raw_keysets_summaries_and_all_breakdowns`; `tests/visit_records.rs::actual_redirect_snapshots_and_visit_pages_are_reported_after_delayed_worker_persistence` | A/B/C report cursor continuation is exercised by the process suite. | PostgreSQL + HTTP + independent processes; `sh scripts/check.sh` |
| 24 | `src/analytics/repository.rs::tests::concurrent_duplicate_event_ids_insert_once_and_overflow_rolls_back`; `tests/visit_concurrency.rs::unknown_visit_commit_acknowledgement_retries_the_same_event_idempotently` | A/B worker commits are concurrently checked for exact row/counter equality. | PostgreSQL + independent processes; `sh scripts/check.sh` |
| 25 | `tests/analytics_failures.rs::queue_worker_parser_and_database_failures_never_fail_a_redirect`; `tests/telemetry_signals.rs::production_otlp_privacy_sampling_outage_drain_and_replacement_are_verified` | Resolution failures remain 503 while post-resolution analytics failures preserve 302; both branches have executable failure controls. | HTTP + PostgreSQL + Collector/process; `sh scripts/check.sh` |
| 26 | `tests/visit_concurrency.rs::concurrent_redirects_count_once_and_both_delete_lock_orders_are_safe` | A deletes while B has a PostgreSQL-blocked event plus an OTLP-observed queued event; work is released and no resurrection occurs. | PostgreSQL + independent processes + Collector metric; `sh scripts/check.sh` |
| 27 | `tests/authentication.rs::failing_credential_and_persistence_paths_emit_only_safe_diagnostics`; `tests/routing.rs::resolution_storage_failure_is_not_fabricated_as_missing`; `tests/report_failures.rs::report_failures_corruption_cancellation_and_diagnostics_are_safe_and_recoverable` | Each database-dependent subsystem's real failure remains 503 and is included in the full PostgreSQL/process gate. | PostgreSQL + HTTP + processes; `sh scripts/check.sh` |
| 28 | `tests/account_concurrency.rs::account_authority_revalidates_after_wait_and_holds_shared_guard_through_commit`; `tests/link_concurrency.rs::guarded_account_mutations_revalidate_after_wait_and_hold_shared_gate_to_commit`; `tests/redirects.rs::redirect_and_visit_use_one_immutable_resolution_snapshot` | Existing guard lock-wait and redirect snapshot evidence is retained; A/B freshness is exercised by process suites. | PostgreSQL + HTTP + independent processes; `sh scripts/check.sh` |
| 29 | `tests/telemetry_signals.rs::production_otlp_privacy_sampling_outage_drain_and_replacement_are_verified` | Production collector capture and sampled/unsampled correlation execute in the full gate. | Packaged-independent production process + Collector; `sh scripts/check.sh` |
| 30 | `tests/telemetry_signals.rs::production_otlp_privacy_sampling_outage_drain_and_replacement_are_verified` | Actual OTLP/stdout/stderr canary privacy and bounded dimensions are scanned in the full gate. | Collector + production processes; `sh scripts/check.sh` |
| 31 | `tests/telemetry_signals.rs::production_otlp_privacy_sampling_outage_drain_and_replacement_are_verified` | Collector outage/stall, PostgreSQL readiness, and measured natural process exit remain required parts of the consolidated gate. | PostgreSQL + Collector + production processes; `sh scripts/check.sh` |
| 32 | `tests/account_process.rs::independent_servers_share_fresh_credentials_account_cursors_and_last_admin_gate`; `tests/link_process.rs::independent_production_servers_share_link_state_allowance_and_restart_persistence`; `tests/discovery_process.rs::discovery_cursors_and_fresh_state_cross_independent_servers_and_volume_restart`; `tests/report_process.rs::report_authority_history_and_maximum_breakdown_cursors_cross_tcp_instances_and_volume_restart` | These execute deliberate A/B/C requests and reauthorize on every continuation. | Independent production processes + PostgreSQL; `sh scripts/check.sh` |
| 33 | `tests/rate_limit_process.rs::independent_processes_share_allowance_and_restarts_preserve_durable_charges` | Production HTTP allowance races are now split across server A/B in `tests/multi_instance_process.rs::production_http_allowances_are_one_durable_account_and_peer_budget_across_a_b_c`. | PostgreSQL + independent processes + HTTP; `sh scripts/check.sh` |
| 34 | `tests/migrations.rs::concurrent_migration_processes_create_one_consistent_history`; `tests/bootstrap.rs::concurrent_bootstrap_processes_have_exactly_one_winner`; `tests/account_process.rs::independent_servers_share_fresh_credentials_account_cursors_and_last_admin_gate`; `tests/link_process.rs::independent_production_servers_share_link_state_allowance_and_restart_persistence`; `tests/legacy_compatibility.rs::v1_server_runs_on_v1_then_fails_closed_after_v2_upgrade_and_v2_runs_ready` | Concurrent CLI/process invariants and retained-history compatibility are run by the complete gate. CI must use full Git history for retained Part 07 commit `9bcf7c0c021c994e76d0696633fe822f0a49a5e1`. | PostgreSQL + independent processes + legacy build; `sh scripts/check.sh` |
| 35 | `tests/visit_concurrency.rs::{concurrent_redirects_count_once_and_both_delete_lock_orders_are_safe,unknown_visit_commit_acknowledgement_retries_the_same_event_idempotently}`; `analytics::reports::tests::worker_counter_snapshots_are_coherent_and_part08_listing_observes_committed_events` | Actual production A/B workers and B's pending queue are synchronized by observed PostgreSQL locks and queue-depth export. | PostgreSQL + independent processes + Collector; `sh scripts/check.sh` |
| 36 | `tests/telemetry_signals.rs::production_otlp_privacy_sampling_outage_drain_and_replacement_are_verified` | `tests/multi_instance_process.rs::independent_a_b_workers_keep_counters_atomic_and_delete_b_queued_events` observes A unready, B ready, and fresh C without state transfer. | PostgreSQL + independent processes + packaged executable; `sh scripts/check.sh` |
| 37 | `tests/routing.rs::raw_path_ownership_methods_and_fallbacks_are_exact` | Separate static fixture, actual NGINX TCP root/assets, and direct backend 404: `tests/proxy_routing.rs::pinned_tcp_proxy_routes_raw_paths_trusts_only_observed_hops_and_carries_real_cursors`. | HTTP + actual proxy + packaged executable; `sh scripts/check-proxy.sh` |
| 38 | `tests/routing.rs::raw_path_ownership_methods_and_fallbacks_are_exact`; `tests/foundation_http.rs::health_ignores_credentials_replaces_request_id_and_sets_common_headers` | API namespace and health precedence are verified through the real proxy in `proxy_routing`. | HTTP + actual proxy; `sh scripts/check-proxy.sh` |
| 39 | `tests/link_creation.rs::permanent_code_namespace_reserves_api_but_not_asset_or_health_root_names` | Static `/assets/*` versus bare `/assets` and valid `health`/`api-sale` codes are tested over proxy TCP in `proxy_routing`. | PostgreSQL + HTTP + actual proxy; `sh scripts/check-proxy.sh` |
| 40 | `tests/routing.rs::raw_path_ownership_methods_and_fallbacks_are_exact` | Percent-encoded alias, slash, multisegment, unknown code, and no-SPA fallback are tested over actual proxy TCP in `proxy_routing`. | HTTP + actual proxy; `sh scripts/check-proxy.sh` |
| 41 | `tests/domain_contracts.rs::validated_domain_values_keep_their_scalar_and_canonical_invariants`; `tests/link_management.rs::patch_detail_noops_and_delete_are_atomic_and_keep_permanent_reservations` | API/list title persistence is in `link_listing`; actual packaged create/list is in release smoke. | Pure + PostgreSQL + HTTP + packaged release; `sh scripts/check.sh`; `sh scripts/check-release.sh` |
| 42 | `tests/link_listing.rs::listing_uses_fresh_account_authority_and_complete_visible_link_snapshots`; `tests/link_pagination.rs::each_page_renews_role_and_account_identity_before_decoding_its_cursor` | Cross-server scope/cursor behavior is exercised by discovery process coverage. | PostgreSQL + HTTP + independent processes; `sh scripts/check.sh` |
| 43 | `tests/link_listing.rs::listing_uses_fresh_account_authority_and_complete_visible_link_snapshots`; `tests/discovery_process.rs::discovery_cursors_and_fresh_state_cross_independent_servers_and_volume_restart` | Packaged smoke lists the created titled/tagged link and checks preserved representation after restart. | PostgreSQL + HTTP + packaged release; `sh scripts/check-release.sh` |
| 44 | `tests/link_creation.rs::production_create_routes_return_complete_owned_and_anonymous_representations`; `tests/link_listing.rs::listing_uses_fresh_account_authority_and_complete_visible_link_snapshots`; `tests/link_management.rs::patch_detail_noops_and_delete_are_atomic_and_keep_permanent_reservations` | Packaged binary create/list/delete/restart flow is `tests/release_smoke.rs::packaged_binary_migrates_bootstraps_serves_rotates_deletes_and_restarts_without_reseeding`. | HTTP + packaged release; `sh scripts/check-release.sh` |
| 45 | `tests/link_filters.rs::filters_are_literal_canonical_and_composed_before_distinct_keyset_selection`; `tests/discovery_values.rs::maximum_legal_destination_title_and_comparison_tags_are_insertable_and_listable` | Complete canonicalization, filters, mutation, and invalid controls are in the full PostgreSQL gate. | Pure + PostgreSQL + HTTP; `sh scripts/check.sh` |
| 46 | `tests/tag_discovery.rs::tag_counts_cover_visible_live_links_and_cursor_pages_groups_not_links`; `tests/discovery_process.rs::discovery_cursors_and_fresh_state_cross_independent_servers_and_volume_restart` | Cross-instance tag visibility/continuation executes on A/B/C in `discovery_process`. | PostgreSQL + HTTP + independent processes; `sh scripts/check.sh` |
| 47 | `tests/link_filters.rs::filters_are_literal_canonical_and_composed_before_distinct_keyset_selection` | Production listing query and tags are exercised against PostgreSQL; route-level filters and cursors run in full gate. | Pure + PostgreSQL + HTTP; `sh scripts/check.sh` |
| 48 | `tests/link_listing.rs::listing_uses_fresh_account_authority_and_complete_visible_link_snapshots`; `tests/link_filters.rs::filters_are_literal_canonical_and_composed_before_distinct_keyset_selection`; `tests/tag_discovery.rs::tag_counts_cover_visible_live_links_and_cursor_pages_groups_not_links` | Strict invalid/multiplicity and authority-precedence controls remain in the full gate. | HTTP + PostgreSQL; `sh scripts/check.sh` |
| 49 | `tests/link_pagination.rs::{all_sorts_directions_have_golden_keyset_order_and_stable_stateless_continuations,each_page_renews_role_and_account_identity_before_decoding_its_cursor}` | A/B/C continuation and boundary-deletion behavior is in `tests/discovery_process.rs::discovery_cursors_and_fresh_state_cross_independent_servers_and_volume_restart`. | PostgreSQL + HTTP + independent processes; `sh scripts/check.sh` |
| 50 | `tests/visit_records.rs::actual_redirect_snapshots_and_visit_pages_are_reported_after_delayed_worker_persistence`; `tests/report_process.rs::report_authority_history_and_maximum_breakdown_cursors_cross_tcp_instances_and_volume_restart`; `tests/telemetry_signals.rs::production_otlp_privacy_sampling_outage_drain_and_replacement_are_verified` | A/B snapshots, retry, deletion, and privacy evidence remain in the full gate. | PostgreSQL + independent processes + Collector; `sh scripts/check.sh` |
| 51 | `tests/openapi_contracts.rs` family response tests and invalid controls | `tests/openapi_release.rs::{whole_openapi_document_and_every_json_schema_root_are_valid,documented_method_path_operation_ids_match_the_reviewed_inventory,production_executable_routes_and_global_method_errors_match_openapi}` adds pinned whole-document validation, all schema roots, refs, complete 22-entry method/path/operationId inventory, real executable TCP probes and globally specified unsupported-method/HEAD behavior. | OpenAPI 3.1 + JSON Schema 2020-12 + production HTTP; `sh scripts/check-openapi.sh` |
