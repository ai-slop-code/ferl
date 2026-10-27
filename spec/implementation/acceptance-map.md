# Acceptance scenario coverage map

The numbers below refer to [server-business-logic.md, section 15](../server-business-logic.md#15-acceptance-scenarios). Each scenario must have executable evidence before part 11 is complete. The final column is initially a suggested suite name, **not evidence that a test exists or passed**. Replace/add exact file/test names during implementation.

Parts are linked from [the implementation index](README.md). Earlier parts may test pure functions or repository invariants; later parts complete HTTP/process evidence for the same scenario.

| Scenario | Required behavior | Implementation parts | Suggested evidence / replace with actual tests |
| --- | --- | --- | --- |
| 1 | Anonymous creation default/switch; old tokens remain valid | 05, 06, 07 | `link_creation`, restart switch flow |
| 2 | Missing/invalid credentials; token operation limits | 04, 05, 06 | `authentication`, `creation_order` |
| 3 | Cross-account isolation and admin object access | 04, 06, 08, 09 | `link_authorization`, `link_listing`, `analytics_authorization` |
| 4 | Single-link token authority, replacement and secrecy | 04, 06, 09 | `token_rotation`, `analytics_authorization` |
| 5 | One-time concurrent bootstrap and restart | 02, 04, 11 | `bootstrap`, `multi_instance::bootstrap` |
| 6 | Account key one-time output, revocation and persistence | 04, 11 | `accounts`, `multi_instance::credential_visibility` |
| 7 | Account disable/re-enable versus individual link state | 04, 07, 11 | `redirects`, `multi_instance::account_state` |
| 8 | Concurrent last-enabled-admin protection, atomic rejection | 04, 11 | `account_concurrency`, independent-process admin race |
| 9 | Account deletion, key invalidation and retained codes | 02, 04, 06, 11 | `accounts`, cross-process cascade/reservation checks |
| 10 | Destination syntax, bounds, internal hosts, no fetching | 03, 06, 07 | `domain_contracts`, network-isolated create/redirect tests |
| 11 | Independent duplicate URLs, generated/custom codes | 03, 06 | `link_creation`, candidate-generator tests |
| 12 | Permanent/conflicting codes and bounded collision retry | 02, 06, 11 | `link_creation`, independent-process allocation race |
| 13 | Immutable identity, edits/deletion and revoked-token errors | 06, 07 | `link_management`, `redirects`, `token_rotation` |
| 14 | GET/HEAD exact 302, query/fragment, no-store/bodylessness | 07 | `redirects` raw-response assertions |
| 15 | Indistinguishable unavailable codes, exact path, 405 | 07 | `routing`, `redirects` |
| 16 | Default per-IP/account/admin rolling thresholds/Retry-After | 05, 06 | `rate_limits`, HTTP charge-count assertions |
| 17 | Eligible invalid attempts charged; other rejections/rollback | 05, 06 | `creation_order`, durable charge rollback test |
| 18 | Concurrent allowance, exact expiry, rotation/restart | 05, 11 | `rate_limit_concurrency`, process restart/rate checks |
| 19 | Proxy trust, full-chain failure, mapped IPv4 identity | 05, 07, 11 | `client_ip`, proxy/anonymous allowance tests |
| 20 | Restart preserves all authoritative data without local state | 02, 04, 11 | `release_smoke`, populated-database restart |
| 21 | Healthy GET/HEAD/bot/repeat ingestion; nonredirect exclusion | 07, 09 | `visit_ingestion`, eventual report assertions |
| 22 | Full optional headers/raw IP, null handling and authorization | 07, 09 | `visit_ingestion`, `visit_records`, access checks |
| 23 | Deterministic visit pages, dates, totals, null/empty breakdowns | 03, 09 | `visit_records`, `visit_stats`, `visit_breakdowns` |
| 24 | Event UUID idempotency and atomic count/record writes | 02, 07, 11 | `visit_concurrency`, multi-process ingestion |
| 25 | Queue/parser/write/worker failure preserves redirects | 07, 10, 11 | `analytics_failures`, observable bounded drop tests |
| 26 | Delete pending visits without resurrection/released codes | 02, 06, 07, 11 | `visit_concurrency`, A-delete/B-queued race |
| 27 | DB failures yield 503 in each dependent operation | 01, 04–09, 11 | Part 01 health subset: `tests/foundation_process.rs::executable_starts_without_database_and_stdout_excludes_secrets_and_raw_urls`; real PostgreSQL outage/recovery: `tests/foundation_postgres.rs::readiness_tracks_schema_and_recovers_after_postgres_restart` (written/compiled, execution pending Podman registry TLS); feature outages remain in later suites |
| 28 | Stale mutation rejection and allowed resolved-redirect race | 04, 06, 07, 11 | Guard synchronization and resolution/edit race tests |
| 29 | Correlated OTLP traces/metrics/logs; sampling independence | 10 | `telemetry_signals` collector capture |
| 30 | All telemetry excludes sensitive data; bounded dimensions | 10, 11 | `telemetry_privacy`, full error-path canary scan |
| 31 | Collector outage, DB/schema readiness, bounded shutdown | 01, 10, 11 | Part 01 process/signal and readiness evidence: `tests/foundation_process.rs::executable_starts_without_database_and_stdout_excludes_secrets_and_raw_urls`; PostgreSQL recovery: `tests/foundation_postgres.rs::readiness_tracks_schema_and_recovers_after_postgres_restart` (execution pending Podman registry TLS); collector outage/export drain remains Part 10 |
| 32 | Cross-instance CRUD/auth/redirect freshness and cursors | 04, 06–09, 11 | `multi_instance::visibility_and_cursors` |
| 33 | One account/IP allowance shared by independent processes | 05, 11 | `multi_instance::shared_rate_allowance` |
| 34 | Cross-process code/bootstrap/admin/migration invariants | 02, 04, 06, 11 | Independent executable race groups |
| 35 | Concurrent worker counts, dedup and cross-process deletion | 07, 11 | `multi_instance::analytics_and_deletion` |
| 36 | Drain/remove/add instance without state transfer | 10, 11 | `multi_instance::drain_and_replace` |
| 37 | Direct backend root/assets and separate static host boundary | 01, 07, 11 | Part 01 direct root 404: `tests/foundation_http.rs::fallback_and_method_errors_are_json_and_head_is_bodyless`; proxy/static boundary remains `proxy_routing` |
| 38 | API prefix fallback and unauthenticated health isolation | 01, 07, 11 | Part 01 API 404 and invalid-credential health checks: `tests/foundation_http.rs::fallback_and_method_errors_are_json_and_head_is_bodyless`, `tests/foundation_process.rs::executable_starts_without_database_and_stdout_excludes_secrets_and_raw_urls`; proxy verification remains `proxy_routing` |
| 39 | assets/health/api-sale valid, api reserved, asset subpaths | 06, 07, 11 | `link_creation`, `routing`, `proxy_routing` |
| 40 | Missing codes/multi-segment paths never become SPA | 07, 11 | Direct and proxy raw-path fixtures |
| 41 | Title normalization, nullable semantics, preservation, no fetch | 03, 06, 08 | `domain_contracts`, `link_management`, list DTO checks |
| 42 | User/admin own defaults, explicit all, scoped pagination | 08 | `link_listing`, `link_pagination` |
| 43 | Complete display fields; ownership/title persistence | 06, 08, 11 | API key-rotation/list/restart/A-B flow |
| 44 | API-only create/list/delete of independent same-URL links | 06, 08, 11 | `release_smoke`, independent-destination flow |
| 45 | Canonical tags, limits, replace/clear and atomic metadata | 03, 06 | `domain_contracts`, `link_management` |
| 46 | Scope-safe tag names/distinct counts and disappearance | 08 | `tag_discovery` |
| 47 | Combined search/filter/tag logic without duplicates | 03, 08 | `link_filters` |
| 48 | Invalid/forbidden filters, multiplicity, no widened scope | 03, 08 | `http_decoding`, `link_filters` |
| 49 | All sort/direction/null/tie cursors, deletion/context/scaling | 03, 08, 11 | `link_pagination`, cross-instance page traversal |
| 50 | Exact A/B historical snapshots, retries/deletion/privacy | 07, 09, 10 | `visit_ingestion`, `visit_records`, `telemetry_privacy` |
| 51 | OpenAPI 3.1 validation, representative schemas/routes/headers | 01, 03–09, 11 | Contract validator and executable conformance suite |

## Additional derived checks

These exercise requirements already present in the normative prose and prevent common implementation mistakes not spelled out individually in the scenario headings:

- Rate Retry-After when a restarted server has a lower configured limit (parts 05, 11).
- Counter overflow rolls back event insertion (part 07).
- No-op canonical PATCH preserves updated_at; visits never change it (parts 06–07).
- A full-size analytics grouping boundary produces a usable cursor through the supported HTTP/proxy stack (parts 03, 09, 11).
- Account and link-token mutation guards revalidate after waiting for their database locks (parts 04, 06, 11).
- API GET routes do not expose automatic HEAD operations; every HEAD error remains bodyless (parts 01, 11).
- All library/default instrumentation obeys the safe-data policy, including exporter failure diagnostics (part 10).

## Release audit record

Status: **not executed**.

When part 11 completes, record the date, build/toolchain/schema versions, supported PostgreSQL version, exact suite commands/results and the concrete evidence locations for every row. A map of intended tests is not a substitute for executed verification.
