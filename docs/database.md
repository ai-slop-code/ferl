# PostgreSQL schema and operations

## Supported versions and schema identity

The initial supported database is PostgreSQL **17.x** (the integration gate
uses the pinned PostgreSQL 17.6 image). The current ferl schema range is exactly
version **1**. The binary becomes ready only when the configured database and
current `search_path` contain the complete version-1 schema and its SQLx
migration history. A newer/unknown migration, an older or incomplete history,
a dirty row, a checksum mismatch, missing schema objects, or missing seed data
is incompatible. `serve` remains live but unready; `migrate` exits nonzero and
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
`0001_initial.sql`; SQLx 0.9 records its checksum and status in
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

The migration adds baseline unique digest/code and ownership indexes, visit
ordering `(link_id, occurred_at DESC, id DESC)`, tag membership `(name,
link_id)`, typed-bucket uniqueness, and charge `(bucket_id, charged_at)` lookup.
It adds no full-width URL/header index, extension, or search-specific index.
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

## Isolated PostgreSQL tests

`tests/support/database.rs::IsolatedTestDatabase::connect_from_harness()`
validates the label-verified pinned harness, creates a UUID-named schema, and
returns a fixture pool plus `schema_database_url()` for production-pool or
process composition. `pool()` is explicitly fixture-scoped. `close()` closes
pools and drops only that generated schema. The same `flock` file coordinates
all test binaries and `scripts/postgres.sh` operations; the PostgreSQL restart
test holds it through outage and recovery. No test points at arbitrary public
schema data, and there is no silent skip when PostgreSQL is absent.
