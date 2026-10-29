# PostgreSQL schema and operations

## Supported versions and schema identity

The supported database is PostgreSQL **17.x** (the integration gate uses the
pinned PostgreSQL 17.6 image). The current ferl schema range is exactly version
**2**. The binary becomes ready only when the configured database and current
`search_path` contain the complete version-1 base schema, version-2 discovery
indexes, and SQLx migration history with both required checksums. A newer/unknown
migration, an older or incomplete history, a dirty row, a checksum mismatch,
missing schema objects, or missing seed data is incompatible. `serve` remains
live but unready; `migrate` exits nonzero and
does not rewrite or repair an incompatible history. Deploy the binary that owns
the required schema version or restore the schema through the normal database
recovery process; never delete or edit `_sqlx_migrations` to force readiness.
Both commands fail closed with category `postgres_version_unsupported` on a
PostgreSQL major version outside 17; migration checks the server version before
any DDL, and serving leaves readiness false.

The configured connection URL determines `search_path`. SQLx's default
`_sqlx_migrations` table, migration DDL, the serving pool, and the verifier all
use the first schema selected by that same URL. For a separately provisioned
schema, create it before migration and use the same libpq/SQLx startup option on
both `migrate` and `serve`, for example:

```text
postgres://ferl:…@db.example/ferl?options[search_path]=ferl_app
```

Do not point serving and migration at different schemas. The isolated test
fixture creates a unique schema and adds `options[search_path]` to the URL it
passes to both the production `AppState` pool and the migration executable.

## Migration command

Migrations are embedded and versioned in `migrations/`. Version 1 is
`0001_initial.sql`; version 2 is `0002_discovery_indexes.sql`. SQLx 0.9 records each checksum and status in
`_sqlx_migrations`. Run `ferl-server migrate` explicitly before rollout. It
does not require or parse serving-only variables such as
`FERL_PUBLIC_BASE_URL`, `FERL_CURSOR_KEY`, listener configuration, proxy trust,
or telemetry settings. It validates `FERL_DATABASE_URL` and applicable
`FERL_MIGRATE_*` values before connecting.

| Setting | Default | Bound / behavior |
| --- | ---: | --- |
| `FERL_MIGRATE_CONNECT_TIMEOUT_SECS` | 10 | 1–3,600 seconds; connection establishment deadline |
| `FERL_MIGRATE_LOCK_TIMEOUT_SECS` | 30 | 1–3,600 seconds; bounded advisory-lock polling deadline |
| `FERL_MIGRATE_STATEMENT_TIMEOUT_SECS` | 120 | 1–3,600 seconds; PostgreSQL statement timeout for migration work |
| `FERL_MIGRATE_TOTAL_TIMEOUT_SECS` | 600 | 1–3,600 seconds; whole-command deadline, at least the connection and lock deadlines |
| `FERL_MIGRATE_LOCK_POLL_MS` | 50 | 10–1,000 ms between nonblocking advisory-lock attempts |

The command uses PostgreSQL session advisory lock key
`0x4645524c4d494752` (`FERLMIGR`, decimal `5063543843715630930`). The lock is
scoped by PostgreSQL to the connected database and serializes migration
commands using this implementation. Each attempt is `pg_try_advisory_lock`;
lock waiting is therefore bounded independently of a blocking advisory-lock
call. The lock is held on one standalone connection, not a pool. SQLx's
internal migration lock is disabled only while this lock is held.

SQLx 0.9 applies each PostgreSQL migration and its successful history row in a
transaction by default. This is why the migration has no non-transactional
operations. On failure, SQL is rolled back; the command drops its dedicated
session, which releases its advisory lock even if cancellation or an error
interrupts cleanup. It does not retry migration execution or reinterpret an
unknown commit outcome. Exit codes are 0 on success, 2 for invalid applicable
configuration, and 1 for connection, lock, timeout, history, compatibility, or
migration failures. Diagnostics contain safe categories only; connection URLs,
driver messages, SQL, and parameters are not printed.

`migrate` is safe to repeat on a compatible database. It rejects dirty,
checksum-mismatched, unsupported, or unexpectedly newer history instead of
repairing it. After SQLx reports success, ferl independently verifies migration
history and required tables, named constraints, indexes, immutability triggers,
the singleton state row, and reserved `api` seed. If a table was dropped while
its successful migration row remains, the command fails compatibility and
does not recreate the table.

## Schema and persistence boundaries

Version 1 creates only these authoritative tables:

- `system_state`: one singleton row, initially `bootstrap_completed = false`;
  the state trigger prevents deletion, reversing completion, or completion
  before an enabled admin row exists. Migration and serving never create an
  administrator or set completion.
- `accounts`: immutable ASCII-syntax usernames, constrained `user`/`admin`
  roles, non-null enabled status, 32-byte unique API-key digests, and
  `timestamptz` metadata. Accounts are physically deleted, so a deleted
  username can be reused with a new UUID. Database triggers prevent username
  and account-ID updates.
- `code_reservations`: a minimal one-column `short_code` table. Migration seeds
  only `api`. The table stores no destination, owner, token, metadata, or
  analytics values; reservations cannot be updated/deleted. Links reference
  reservations, never the reverse.
- `links`: immutable unique code and owner; exactly one ownership mode; optional
  32-byte unique management-token digest; destination/search values; paired
  nullable title/title-search values; nonnegative `bigint` visit counter; and
  independent `created_at`/`updated_at` timestamps. Account deletion cascades
  owned links. Analytics increments do not update metadata timestamps.
- `link_tags`: deterministic `COLLATE "C"` name equality/order, a composite
  `(link_id, name)` primary key, and link-delete cascade. Full Unicode
  normalization and input-before-deduplication limits belong to Part 03.
- `visits`: immutable event ID, non-null destination snapshot, UTC-capable
  timestamp, GET/HEAD method, host-masked IPv4/IPv6 `inet`, nullable bounded
  referrer/user-agent, constrained device values, and nonempty browser/OS
  values. A live-link FK cascades deletion to visits. A trigger rejects updates;
  no deleted URL or analytics record is retained.
- `rate_buckets` and `rate_charges`: typed account-UUID or canonical host-IP
  identity independent of credential values, durable charge IDs and database
  timestamps. Bucket identity has no account/link ownership FK, so deleting an
  account/link does not erase the accounting history. Part 05 implements rolling
  accounting and bounded opportunistic pruning under the stable bucket lock;
  see [creation allowance](creation-allowance.md).

Version 1 adds baseline unique digest/code and ownership indexes, visit ordering
`(link_id, occurred_at DESC, id DESC)`, tag membership `(name, link_id)`,
typed-bucket uniqueness, and charge `(bucket_id, charged_at)` lookup. Version 2
adds scoped and global link ordering paths for creation time and persisted visit
count, plus scoped/global title-order B-trees for both directions. It adds no
full-width destination URL/header index, extension, or trigram index.
`destination_search` and `title_search` must be written atomically with source
fields by later domain/repository operations; this migration enforces
non-null/nullable pairing but does not implement Unicode normalization.

Part 03 pins the persisted comparison transform to ICU4X 2.4.0 compiled Unicode
18.0.0 case-folding, NFC, whitespace, and control tables. All instances must run
the same transform when later services write/search `destination_search`,
`title_search`, and canonical tags. A Unicode data-version change requires a
coordinated data migration/compatibility plan; mixed search-key versions are not
supported. Part 03 makes no schema or migration change. Application timestamps
are truncated to PostgreSQL's microsecond precision before DTO/cursor use.

## Readiness and recovery

The serving process starts with schema readiness false and does not run
migrations or bootstrap. A bounded background verifier runs immediately and
rechecks every 5 seconds with a 30-second ceiling. It validates SQLx version,
success and checksum rows against the embedded migration source, all required
tables/constraints/indexes/triggers and bootstrap/reservation seeds. Any schema
or database failure marks the instance unready; liveness does not use
PostgreSQL. A later successful verification makes the schema gate compatible
again, while `/api/health/ready` also requires a fresh bounded database probe
and acceptance of work. Thus PostgreSQL/schema recovery becomes visible without
restart. A failed readiness database probe invalidates the schema gate until
the background verifier revalidates it; a verification generation prevents an
older in-flight schema result from overriding a newer failure. Draining remains
unready even if an earlier database probe finishes. The monitor is cancelled
and joined within a 2-second shutdown bound.

A plain `SELECT 1`, table count, migration-history row alone, or manually set
Boolean is not schema evidence. Compatibility is determined by the complete
version/checksum and required-object checks. Serving never mutates schema,
creates an administrator, or changes `bootstrap_completed`.

## Locking and transaction contract

The deployment-wide account-authority transaction gate uses the PostgreSQL
two-int key `(0x4645524c, 0x41434354)` (`FERL` / `ACCT`). Account-authenticated
link mutations take the shared transaction lock; every account write takes the
exclusive lock, including bootstrap, key replacement, role/status changes,
creation, and deletion. Part 04 exposes a shared account mutation guard and an
exclusive account-write guard; each owns its transaction and fresh caller state
until explicit commit or rollback. Repository calls use the supplied
transaction/connection and do not check out a hidden pool connection.

Guarded mutations use READ COMMITTED. Acquire the potentially waiting advisory
lock in one SQL statement, then re-read credential and authorization state in
a later statement with a fresh snapshot. Do not combine lock wait and
revalidation into one SELECT/CTE. Configured statement deadlines bound lock
queries; bounded helper variants apply a transaction-local statement deadline
and require rollback/drop on failure. Never continue a mutation after lock
timeout.

Global lock order:

1. Account-authority gate, if required.
2. Account rows in UUID order.
3. Link rows in UUID order.
4. Child rows (tags, visits, and dependent state).

Rate-bucket coordination is in a separate short transaction and is not held
while creating links. Unknown commit acknowledgement remains ambiguous and is
not automatically retried. No domain service, bootstrap command, rate limiter,
link CRUD API, redirect, or analytics worker is implemented in Part 02.

## Part 06 link-management transactions

Part 06 uses the existing version-1 schema without a migration. Eligible create
requests first obtain an independently acknowledged Part 05 allowance charge,
then create in a separate READ COMMITTED transaction. Account creation acquires
the shared account-authority gate and re-reads the retained credential, enabled
state, and current role in a later statement. Anonymous creation stores a null
owner and one freshly generated token digest. Reservations, link metadata/search
values, and tag associations commit or roll back together. Generated allocation
uses conflict-safe reservation insertion and at most ten candidates; collisions
do not abort the transaction. Deleted links cascade their tags and visits but
leave the minimal reservation row permanently occupied.

PATCH and DELETE take the shared account-authority gate for account callers or
the live token-bearing link row for token callers, then lock the target link row
and revalidate current authority before changing any child rows. Token rotation
uses the same row lock; admin token replacement additionally uses the shared
account gate and current admin role. The lock order remains authority gate →
account rows → link rows → child rows. The bounded gate helper initially leaves
its lock deadline in transaction-local `statement_timeout`; link services
explicitly restore the configured statement and lock deadlines before further
SQL. Each mutation retains the lock through acknowledged commit. Unknown commit
acknowledgement is not retried, compensated, or converted into a success.

Detail representations read link metadata, owner state, and canonical tag names
from one SQL statement snapshot. `links::resolve_active_link` is the Part 07
handoff: a fresh, bounded primary-pool single-statement link/owner join that
returns only a typed immutable destination snapshot for an enabled link with an
anonymous owner or an existing enabled account owner. It returns unavailable
links separately from storage/conversion failures, uses no cache, and performs no
redirect or visit work.

## Part 07 visit writes

Part 07 uses this schema unchanged; no migration is required. Each worker attempt
checks out one bounded pool connection and opens one transaction. It locks the
live link row by immutable ID and verifies its immutable code, inserts the
queued UUID/time/destination/method/IP/header snapshot with `ON CONFLICT (id) DO
NOTHING`, and increments `visit_count` only when `RETURNING id` proves that this
transaction inserted a new row. Insert and increment share the transaction; a
counter overflow or failed increment rolls the visit insert back. Analytics does
not update `updated_at` and does not consult the link's current destination.

Retries are limited to the configured retry budget and bounded exponential
backoff. They reuse the exact same event UUID, timestamp, destination snapshot,
method, address, and optional headers. This UUID idempotency is the one
intentional exception to the general no-retry rule for unknown commit
acknowledgements: a committed first attempt becomes a duplicate insert and does
not increment again; an uncommitted attempt is inserted once. Other write
outcomes are reported as safe categories and dropped after bounded retries.
Each worker attempt, connection acquisition, PostgreSQL statement/lock wait,
explicit rollback, and shutdown drain has its own bound. If a transaction is
cancelled, it is discarded; application code never continues a cancelled
transaction.

Deletion safety follows the existing order: a worker that locks the link first
may commit its visit/counter, after which deletion waits and cascades both; a
deletion that commits first leaves no live row for the worker and the event is
dropped. Workers do not create links, reservations, visits for absent links, or
counters. Abrupt termination may lose the bounded in-memory queue; PostgreSQL
rows/counters already committed remain durable.
An event denotes a ferl-issued 302 only; it is not evidence that the client
received the response or reached the destination.

## Part 08 discovery reads and migration rollout

Version 2 is append-only; `0001_initial.sql` and its SQLx checksum are unchanged.
`0002_discovery_indexes.sql` adds eight ordinary B-tree indexes:

| Access path | Indexes |
| --- | --- |
| `(created_at, id)` order | global and `(owner_id, created_at, id)` |
| `(visit_count, id)` order | global and `(owner_id, visit_count, id)` |
| normalized title order | global/scoped, each with ASC NULLS LAST and DESC NULLS LAST keys |

The two `visit_count` indexes are intentionally maintained on every successful
Part 07 visit increment; this write amplification supports the global and
owner-scoped orders without counting raw visit rows. Title keys index the bounded
normalized title, not destination URLs. Existing `(link_id, name)` and
`(name, link_id)` tag indexes support association predicates and byte-ordered
group scans. PostgreSQL's C collation on `title_search`/`link_tags.name` is
verified, as are all eight key definitions and the indexes' valid/ready state.
Substring `LIKE` patterns use a bound `!` escape policy (`!`, `%`, and `_` are
escaped); backslash is literal. With a leading wildcard and no `pg_trgm`, search
is expected to scan the authorized candidate relation. No extension permissions,
global extension lifecycle, or extension cleanup is required.

The migration uses normal transactional `CREATE INDEX`, not `CREATE INDEX
CONCURRENTLY`: SQLx's PostgreSQL migrator wraps version 2 DDL and its history row
in one transaction. This preserves a clean retry and no partial-index state, but
an index build takes a write-blocking table lock for its duration. Migration
connect, lock, statement, and total deadlines remain bounded. If the lock or
statement deadline expires, the transaction rolls back and the CLI returns
failure; it does not mark the schema upgraded. The verifier then rejects missing
or mismatched discovery indexes. Retry only after resolving the lock/contention
cause and re-run `ferl-server migrate`.

Because both binaries fail closed on migration history they do not recognize,
this additive schema change is not an overlapping rolling upgrade: the v1 binary
does not serve after version 2 is applied, and the v2 binary does not become
ready before version 2 is applied. Required rollout is a maintenance sequence:
drain/stop all v1 instances, apply migration with the v2 binary's `migrate`
command, verify it exits successfully and the full schema verifier passes, then
start v2 instances and wait for readiness. An index-build timeout is not a
successful upgrade. Rollback to v1 requires restoring the pre-migration database
state (or a deliberate forward fix); starting old binaries against v2 is not a
rollback strategy.

Real-PostgreSQL evidence is in `tests/migrations.rs` and
`src/links/list_repository.rs::tests::production_keyset_query_plans_cover_scope_sort_filter_and_tag_paths`.
It starts from fresh migration and from populated authentic v1 history, checks
preservation of credentials, links, tags, visits/counters, reservations,
bootstrap and rate charges, repeat/concurrent migration, missing/wrong index
detection, and lock-timeout rollback/recovery. The query-plan fixture analyzes
20,000 links and 22,000 associations using the production SQL builders and
`EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)`: selective owner and global first and
continuation pages across all sort directions, literal search, tag any/all, and
tag discovery scope/search/group continuation. It records actual scans, indexes,
sorts, rows, loops and buffer counts rather than requiring an index for broad
scans. Maximum legal 8,192-byte destinations, 200-scalar titles, 64-scalar tags,
and 256-scalar trimmed tag input remain insertable in
`tests/discovery_values.rs`.

`tests/legacy_compatibility.rs::v1_server_runs_on_v1_then_fails_closed_after_v2_upgrade_and_v2_runs_ready`
builds the retained Part 07 v1 source in a temporary detached worktree and
separate temporary Cargo target. Against the test-owned schema, it verifies the
old server is ready on v1, runs the v1 migration command, upgrades with v2,
observes the old binary's sanitized newer-schema incompatibility/unready state,
and then verifies a new v2 server is ready and serves the list endpoint. The
temporary worktree and target are removed by bounded cleanup. This is executable
evidence for the stop/migrate/deploy order above, not a claim of mixed-version
rolling compatibility. The isolated cold build deadline is 600 seconds; CI
must fetch complete repository history because the retained commit is not
available from a shallow checkout.

The independent-process Part 08 test also stops/restarts only the owned PostgreSQL
container while `IsolatedTestDatabase` holds the shared harness lock; v2 schema,
links, tags and visit counters remain available through a replacement process.
No generic database is reset or repopulated.

## Part 09 analytics reports (schema v2 retained)

Part 09 adds no schema change. The existing
`visits_link_occurred_id_idx (link_id, occurred_at DESC, id DESC)` supports
inclusive/exclusive per-link visit filtering and `(occurred_at, id)` continuation.
Summary reads the persisted `links.visit_count` and a matching-visit `count(*)`
in one statement. Breakdown SQL uses static UTC-day, method, referrer, canonical
rendered-IP, raw-user-agent, device, browser, and OS expressions, materializes
complete filtered group counts, then applies explicit C-collated value pagination.
No 8,192-byte referrer/user-agent B-tree is added. Existing schema-v2 integrity
checks, index definitions, and the Part 08 migration/legacy-binary compatibility
tests remain unchanged.

Each report preflights authority for error precedence, then uses one held primary
pool connection for a `REPEATABLE READ READ ONLY` transaction that revalidates
the credential and link authority before reading report data. It takes no row
lock. Deletion before this final snapshot is 404; deletion after it may allow
the in-flight response to finish from the prior consistent snapshot. Subsequent
requests perform fresh authentication and authorization. The one-connection
composition is part of report tests. Operation, acquire, and statement deadlines
remain independently bounded; cancellation drops the transaction and the
failure/reuse tests prove the bounded pool is usable afterward.

Production `EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)` evidence for raw first and
continuation pages, narrow/broad summaries, and all eight breakdown first and
continuation pages is recorded by
`src/analytics/reports/tests.rs::production_report_query_builders_emit_analyze_buffer_plans_for_representative_data`.
The plans include multiple links/owners and high-cardinality/null-heavy headers;
they do not force sequential-scan settings. Small/broad aggregates can correctly
use sequential scans. See [analytics report operations](analytics-reports.md)
for the measured plan observations, cursor transport, and deployment behavior.

## Isolated PostgreSQL tests

`tests/support/database.rs::IsolatedTestDatabase::connect_from_harness()`
validates the label-verified pinned harness, creates a UUID-named schema, and
returns a fixture pool plus `schema_database_url()` for production-pool or
process composition. `pool()` is explicitly fixture-scoped. `close()` closes
pools and drops only that generated schema. A process-local async mutex serializes
sibling Tokio test tasks because PostgreSQL advisory keys are database-scoped;
the shared `flock` coordinates independent test binaries and
`scripts/postgres.sh` operations. Restart tests hold both through outage and
recovery. No test points at arbitrary public schema data, and there is no silent
skip when PostgreSQL is absent.
