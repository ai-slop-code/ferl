# Per-link analytics reports

Part 09 adds exactly these authenticated operations:

| Method and route | Result |
| --- | --- |
| `GET /api/v1/links/{code}/visits` | Keyset page of persisted visit records |
| `GET /api/v1/links/{code}/stats` | Lifetime and interval counts |
| `GET /api/v1/links/{code}/stats/breakdown` | Keyset page of complete grouped counts |

All three accept only their documented query parameters. Reports do not create
visits, spend link-creation allowance, fetch destinations, or cache report data.
Only GET is registered; HEAD and all other methods return bodyless 405 with
`Allow: GET`.

## Authentication and authority

The production API dispatcher recognizes report paths before entering the
account dispatcher’s unrelated fixed 45-second deadline. Report dispatch selects
the exact path and method first, then calls the shared fresh credential
authenticator. A missing credential is 401 `authentication_required`; supplied
malformed, revoked, rotated, or disabled-account credentials are 401
`invalid_credentials` with `WWW-Authenticate: Bearer`. A valid account must be
the enabled owner or a currently enabled admin. A management token must match
the live anonymous link. Unknown and inaccessible codes both return concealed
404 `not_found`. Link `enabled` state and owner suspension do not prevent an
admin from reading retained reports. Disabling anonymous creation does not
invalidate an existing token.

Path syntax is checked without percent-decoding or aliasing a short code. Before
any date, grouping, limit, or cursor value is decoded, the service performs a
read-only object-authority preflight using the authenticated private-field
`Principal`; a caller-supplied UUID is never used as authority. It returns only
the immutable link UUID. The final operation rechecks the credential and object
authority in the report snapshot. Thus malformed input for an inaccessible
object cannot reveal whether its dates, grouping, or cursor were valid.

Visits allow `from`, `to`, `limit`, `cursor`; stats allow only `from`, `to`;
breakdown requires exactly one `group_by` from `day`, `method`, `referrer`,
`client_ip`, `user_agent`, `device`, `browser`, or `os`, and additionally allows
`from`, `to`, `limit`, `cursor`. The shared strict query decoder rejects unknown
names, repeated scalars, malformed percent/UTF-8 values, empty cursors, invalid
limits, and equal/reversed intervals. `from` is inclusive, `to` exclusive; both
are normalized to UTC microseconds. Page size is not part of cursor context.

## Snapshot, transaction and deletion boundary

After the precedence preflight, a report acquires one connection from the
configured primary pool and opens one `REPEATABLE READ READ ONLY` transaction.
Its first SELECT revalidates the authenticated account digest/enabled/current
role or the exact live token digest, then the code/owner link authority. The
report statement runs on that same held connection and snapshot; there are no
hidden checkouts inside the transaction, per-item checkouts, row locks, N+1
queries, or unbounded Rust-side grouping/sorting. `FERL_DB_POOL_MAX=1` is covered
by the production-router/database tests.

If link deletion commits before the final snapshot, authority fails with 404;
the service cannot turn it into a successful zero count. If deletion or token
rotation commits after the final snapshot is established, that already-in-flight
read may finish from its older consistent snapshot. Every subsequent request
reauthenticates and reauthorizes against current PostgreSQL state. No
linearizability is promised beyond this snapshot boundary. Ordinary SELECTs take
no row lock that blocks redirect workers or link deletion. Cancellation drops
the transaction; PostgreSQL/operation/acquire bounds remain active, and a later
request can reuse the bounded pool.

The configured complete report deadline is
`FERL_REPORT_OPERATION_TIMEOUT_SECS` (default 30 seconds; 1–300). It covers fresh
authentication, preflight authority, query parsing/cursor decode, final
authority, and report SQL. It is separate from authentication’s acquire bound,
database statement/lock/acquire bounds, the link-service deadline, analytics
worker persistence deadlines, and the account API wrapper. A report deadline or
storage fault maps to sanitized 503, not an empty page or fabricated zero.
Corrupt persisted values map to sanitized 500.

## Report semantics

### Raw visits

Each item contains exactly `id`, `link_id`, `short_code`, `redirect_url`,
`occurred_at`, `method`, `client_ip`, `referrer`, `user_agent`, `device`,
`browser`, and `os`. `redirect_url` is the immutable persisted `Location`
snapshot, never the current destination; `occurred_at` is redirect occurrence
time, not worker insertion time. Nullable headers preserve their full valid UTF-8
values; null and empty string remain distinct. IPs serialize as canonical bare
addresses, with IPv4-mapped IPv6 normalized to IPv4. Timestamp, destination,
method, device, header size, host-mask IP, and counter conversions are checked.
UTC timestamps retain PostgreSQL microsecond precision and serialize with `Z`.

The production query filters interval first, then uses
`(occurred_at, id) < (boundary_time, boundary_id)`, order
`occurred_at DESC, id DESC`, and `limit + 1`. The returned boundary is the last
returned row, not the extra row. The existing v1 index
`visits_link_occurred_id_idx (link_id, occurred_at DESC, id DESC)` serves raw
keyset reads. Cursor payloads retain timestamp/UUID values, so continuation does
not look up the previous visit row; deletion of the link still makes subsequent
pages unavailable.

### Summary

The response contains exactly nonnegative int64 `lifetime_visits` and
`matching_visits`. One SQL statement reads the persisted link counter and the
interval `count(*)` in the same repeatable-read snapshot. Lifetime ignores the
filters; matching counts every persisted visit with inclusive `from` and
exclusive `to`, never only a page. A live empty link returns 0/0. A nonmatching
interval returns 0 matching while preserving lifetime. The GET never repairs or
reconciles an inconsistent counter.

Part 07 visit insertion and counter increment remain atomic. A concurrent report
therefore sees either both sides of a committed visit or neither, never one
committed version mixed with another.

### Breakdown

Validated `GroupBy` values select only static SQL expressions; no raw grouping
identifier is interpolated. Day uses UTC explicitly:
`(occurred_at AT TIME ZONE 'UTC')::date`, rendered as `YYYY-MM-DD`; it is not
dependent on the session timezone and no absent dates are synthesized. Method,
referrer, raw user-agent, device, browser, and OS use exact stored values. Unknown
classifications are strings. Null and empty header buckets remain separate.
Client IP is grouped and ordered as its canonical rendered bare string, not by
PostgreSQL’s numeric `inet` order.

The query applies authorization and time filter, computes complete group counts
in a PostgreSQL `MATERIALIZED` CTE, then applies a value boundary, C-collated
ascending order with `NULLS FIRST`, and `limit + 1`. Materialization is deliberate:
the planner cannot push a page boundary onto raw visit rows, so a returned bucket
always retains its full interval count. High-cardinality grouped results can use
PostgreSQL aggregate memory or spill to its temporary-file mechanism; the query
is bounded by the normal statement and report-operation deadlines, not by an
incorrect pre-aggregation page limit. Boundary states are distinct: no cursor means all groups; after
a null value means only non-null groups; after string `s` means non-null values
with `value COLLATE "C" > s COLLATE "C"`. Counts remain complete for each group
regardless of page size/boundary.

## Cursors and live pagination

Reports reuse the sealed XChaCha20-Poly1305 `CursorCodec`, `CursorContext::visits`
and `CursorContext::breakdown`, and typed visit/group boundaries. Context binds
resource, current account UUID or anonymous-link UUID, immutable target link UUID,
normalized microsecond interval, and fixed ordering or selected group. It excludes
page size and contains no credentials or credential digests. Every request
reauthenticates and reauthorizes before cursor decode. Thus account-key and
anonymous-token rotation continue with the replacement credential while the old
credential immediately fails; admin demotion and recreated usernames do not
inherit authority/context.

Pagination is live across requests, not a frozen dataset. Delayed Part 07 worker
ingestion may add earlier occurrence times, and breakdown group counts may change
between pages. No cursor table, sticky session, cache, or cross-request snapshot
is used.

## Maximum values over HTTP/TCP

`FERL_CURSOR_KEY` remains the deployment-wide key. The codec supports an
8,192-byte grouping boundary and caps encoded cursors at 87,387 ASCII bytes.
Locked Axum is 0.8.9 and locked Hyper is 1.12.0. Hyper’s HTTP/1 `DEFAULT_MAX_BUFFER_SIZE`
is `8192 + 4096 * 100 = 417,792` bytes, including the request line and headers;
the default header-count limit is 100 and `max_header_size` is unset, so the
read-buffer bound applies. No application transport setting was changed: actual
production `ferl-server` TCP tests capture 8,192-byte quote-rich and multibyte
UTF-8 user-agent values, obtain non-null breakdown cursors whose returned
boundary is the last item on a page, then continue over A/B/replacement-C and
after the owned PostgreSQL named-volume restart. The quote-rich value exercises
JSON escaping. The tested fields are valid transport values; no NUL/CR/LF header
was sent. Operators using a reverse proxy must set request-target/request-line
limits to pass a cursor of up to the documented codec capacity (at least 88 KiB
plus fixed route/query syntax) and independently permit the ordinary request
headers required by the deployment. The ferl executable’s tested Hyper buffer
is larger. The server does not silently truncate or reduce legal headers/cursors.
No transport buffer was enlarged: Hyper starts small and may grow the HTTP/1
read buffer up to that per-connection cap while an incomplete request is being
parsed. Budget that existing bound against concurrent TCP connections; the
application’s handler admission semaphore begins after request parsing and is
not a substitute for reverse-proxy connection/slow-client limits.
Observed production TCP measurements: both raw fields were exactly 8,192 bytes;
the quote-heavy sealed cursor was 22,191 bytes and its complete request target
was 22,266 bytes; the multibyte UTF-8 cursor was 11,272 bytes with an 11,346-byte
request target. Each cursor was obtained from the endpoint and accepted by a
different production process, then continued by replacement C after the volume
restart.

## Schema and query-plan decision

Part 09 reuses schema v2 and the existing visit ordering index. No migration,
header-value B-tree, counter-order index, dependency, or compatibility change was
needed. Part 08’s v2 schema verifier and migration/legacy-binary regressions
remain in the full gate. Part 08 binaries remain schema-compatible but do not
serve report routes; route traffic must reach Part 09 binaries to use reports.
Rollback to Part 08 needs no database downgrade, but removes the report routes.

The executable report-plan test runs production query builders with
`EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)` against real worker-persisted records,
multiple links/owners, high-cardinality raw agents, null-heavy referrers, broad
and narrow intervals, raw first/continuation pages, summary queries, and all
eight breakdowns on first/continuation paths. It records actual nodes, rows,
loops, sorts, buffers, index names, aggregation and execution/planning times.
The raw broad first page uses the existing visit-order index; a narrow
continuation fixture uses that index through a bitmap scan followed by a
top-N heapsort. Summary uses the link key and aggregates visits; small/broad
fixtures may use sequential scans. All breakdowns aggregate the complete
per-link interval and sort grouped results; no full-width header index is
appropriate. The test does not disable sequential scans or require index use for
small/broad aggregation. In the 1,320-visit fixture (1,200 target-link and 120
other-owner visits), raw broad first-page used `visits_link_occurred_id_idx`
(51 rows, 1 loop, 53 shared hits, 0 reads, about 0.07 ms); narrow continuation
used the same index via a bitmap scan (129 matching rows) and top-N heapsort
(51 output rows, 37 shared hits, 0 reads, about 0.20 ms). Broad summary used a
small-fixture sequential scan for the 1,200 target rows, removed 120 other-link
rows, and had 37 shared hits (about 0.66 ms); the narrow summary used the visit
index for its 129 rows (about 0.13 ms). Breakdown plans materialized full
per-link groups before CTE boundary filtering: UTC day produced 28 groups;
method 2; referrer 12; rendered
IP 5; raw agent 404; and each classification dimension 3. The high-cardinality
agent HashAggregate used about 97 KiB peak memory and a top-N heapsort. Plans
used one loop per node and warm-buffer shared reads were zero; breakdown
execution ranged about 0.7–1.4 ms in this fixture. These observations are
representative, not throughput targets; they show why broad aggregates and
small fixtures need not select an index.

## Verification

The report-specific executable evidence is in:

- `src/analytics/reports/tests.rs` — real PostgreSQL, actual production router,
  private deterministic event-time seam into the ordinary bounded worker,
  golden intervals/pages/groups, UTC/session-timezone and concurrency checks,
  query plans, and preflight/final-snapshot deletion/rotation ordering.
- `tests/visit_records.rs` — delayed worker persistence, actual redirect A → edit
  B → later redirect B, complete raw DTOs and historical snapshots.
- `tests/analytics_authorization.rs` — owner/admin/token authority matrix,
  concealment, fresh rotation/suspension/deletion/demotion and query precedence.
- `tests/report_failures.rs` — actual production logger/filter, storage lock,
  cancellation/reuse, report deadline, and explicitly fault-injected persisted
  timestamp/count/method/device/IP/header values.
- `tests/report_process.rs` — independent ferl-server TCP processes A/B/C,
  credentials, historical records, maximum legal grouping cursors, and the owned
  persistent-volume restart.
- `tests/openapi_contracts.rs::actual_analytics_report_responses_and_errors_match_openapi_with_invalid_controls`
  — actual success/error/HEAD responses against checked-in OpenAPI components
  and Draft 2020-12 invalid controls.

Run the complete PostgreSQL-backed verification with `sh scripts/check.sh`.
It also runs the retained v2 migration/compatibility, Part 08 discovery, Part 07
worker, OpenAPI, and documentation checks. Part 10 OTLP export and Part 11
release/deployment readiness are not asserted by Part 09.
