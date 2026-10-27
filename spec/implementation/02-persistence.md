# Part 02 — PostgreSQL schema, migrations and transaction primitives

**Dependencies:** part 01.

**Normative references:** business sections 5–7, 9.1–9.2, 13–13.1; OpenAPI account, link, visit and count schemas.

## Outcome

Real PostgreSQL storage supports every durable invariant needed by later parts. Migrations preserve data, coordinate across processes and gate readiness. Tests demonstrate deletion direction, uniqueness and ownership constraints before services rely on them.

## Files and responsibilities

- `migrations/0001_initial.sql`: initial schema from architecture.md, including visits and rate accounting.
- `src/db/{migrate,coordination,error}.rs`: migration locking, compatibility checking, named advisory gates and sanitized database errors.
- Domain-specific record modules or `src/db/records.rs`: internal row types, with no public serialization of digests.
- `tests/{migrations,persistence_constraints}.rs` and reusable seeded-fixture helpers.
- `docs/database.md`: schema, lock hierarchy, version/extension requirements and migration procedure.

## Tasks

### 1. Create the authoritative schema

Implement all tables in architecture.md. Use explicit named constraints so SQLSTATE plus a recognized constraint name can map username/code conflicts safely without parsing a raw server error string.

- Account usernames are unique among live accounts and may be reused after deletion; new accounts always have new UUIDs.
- Seed the singleton bootstrap row as incomplete and reserve `api` in the same migration.
- Reserve code namespace in a separate minimal table. `assets`, `health` and `api-sale` are not seeded/reserved.
- Enforce ownership/token exclusivity, role/device/method enums or equivalent CHECKs, code and username syntax, digest sizes, unique digests and nonnegative counters.
- Store link destination and derived normalized search values together; nullable title implies nullable title-search value. Link timestamps and counter are independent.
- Store canonical tag associations with a composite primary key. Use deterministic collation for name equality/order and ensure no orphan associations.
- Visits reference live links, contain immutable non-null redirect snapshots, have a unique UUID, and cascade on deletion. Optional raw header nullability must match OpenAPI.
- Rate rows are distinct from account/link ownership cascades. Rotation/restart does not change bucket identities or remove live charges. Expiry, not a key value, controls allowance lifetime.

Apply database length/type constraints where expressible; full Unicode normalization and input-before-deduplication limits remain domain validations. Protect immutable link code/owner and username through restricted update statements and, if needed, triggers. Do not add a soft-delete model retaining deleted URLs or analytics.

### 2. Establish indexes

Create baseline indexes for credential digest lookup, ownership, unique codes, `(link_id, occurred_at DESC, id DESC)` visits, tag membership `(name, link_id)`, and bucket/time charge queries. Add scoped ordering/search indexes in part 08 based on actual queries. Avoid full-width B-tree indexes on long URLs/raw headers.

### 3. Coordinate migrations and readiness

Implement `ferl-server migrate` using embedded/versioned migrations and database-level migration locking. Choose SQLx's verified migration coordination or an explicit advisory lock on one held connection. Concurrent commands must not apply a migration twice or leak locks into the pool.

Use an explicit migration command before rollout as the deployment model. `serve` verifies compatibility and does not reset/recreate data or bootstrap an account. Detect missing, dirty, incompatible or unexpectedly newer schema versions and remain unready/fail initialization with a sanitized reason. Document the supported schema-version range and exact behavior.

### 4. Provide transaction helpers

Centralize the account-authority shared/exclusive transaction gates and their constant advisory key. Use PostgreSQL transaction-scoped locks. Provide bounded acquisition/query behavior and a database-error classifier separating expected constraint conflicts, absence, transient availability and unexpected internal failures.

Document the read/write semantics from architecture.md next to these helpers. Use ordinary primary-database reads with fresh statement snapshots for redirect/auth checks; use explicit locks and revalidation for protected writes. Avoid broad SERIALIZABLE retries as a substitute for understanding commit outcome.

## Verification gate

Run tests against real PostgreSQL:

- Apply migrations to empty storage, verify tables/constraints/seeded state, and verify repeated migration commands are harmless.
- Race two migration connections/commands and verify a single consistent migration history. Independent-process coverage is finalized in part 11.
- Duplicate usernames and code reservations fail authoritatively; deleted usernames can be recreated with new IDs.
- Inserting ownerless links without a token, owned links with a token, orphan rows, invalid digest lengths or negative counts fails.
- Deleting one link removes its tags/visits while preserving its reservation. Deleting an account cascades all its links/visits but preserves every code. A rollback restores the entire logical deletion.
- The reservation table contains no deleted link metadata, credential, owner or analytics data.
- Reconnecting/restarting preserves populated accounts, digests, links, visits, counters, reservations and bootstrap flag.
- Incompatible/missing schema never reports ready.

**Handoff:** record migration version, supported PostgreSQL version, lock keys/hierarchy, and fixture APIs. This supplies persistence evidence for 5, 9, 12–13, 20, 24, 26 and 34; service/API behavior is verified in subsequent parts.
