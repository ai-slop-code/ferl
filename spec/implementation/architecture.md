# Implementation architecture and coordination rules

This is the recommended implementation baseline for all numbered parts. Internal choices may change with a documented equivalent design; the observable guarantees may not.

## 1. Rust baseline and layout

Use one Cargo package with a library and the `ferl-server` executable. Choose a supported stable Rust toolchain and edition, pin the toolchain, set `rust-version`, and commit `Cargo.lock` for this application. Resolve mutually compatible released dependencies during part 01 rather than guessing historical versions.

Recommended families:

- Tokio, Axum and Tower for async HTTP; explicitly manage framework defaults that conflict with the API.
- SQLx with PostgreSQL, migrations, UUID and UTC time support.
- Serde/serde_json for DTOs; `url` for URL parsing and serialization.
- A full Unicode case-folding implementation plus Unicode NFC normalization. Lowercasing is not a substitute for full case folding.
- OS-backed cryptographic randomness, SHA-256, base64url and XChaCha20-Poly1305 for secrets/cursors as specified below.
- `tracing`, compatible OpenTelemetry SDK/OTLP exporters, stdout JSON logging, and an actual OTLP log bridge/provider.
- A maintained, local user-agent parser with bundled/versioned rules. Runtime parsing must not download rules or make enrichment requests.

Suggested layout; numbered parts list the relevant subsets:

```text
Cargo.toml
Cargo.lock
rust-toolchain.toml
src/
  main.rs                 # CLI entry, runtime and exit status
  lib.rs                  # testable composition
  app.rs                  # AppState, router and service construction
  config.rs
  shutdown.rs
  error.rs
  domain/                 # validated values and public-independent types
  http/                   # routing, envelopes, strict body/query parsing, DTOs
  auth/                   # bearer parsing, principal lookup, mutation guards
  accounts/               # bootstrap, services and repositories
  links/                  # allocation, CRUD, scoped listing
  tags/                   # discovery derived from visible associations
  rate_limit/             # PostgreSQL rolling allowance
  client_ip.rs
  redirects.rs
  analytics/              # event, bounded worker, persistence, reports
  pagination/             # cursor codec/context and typed keyset boundaries
  db/                     # pool, migrations, coordination, error mapping
  telemetry/              # allowlisted instrumentation, providers, metrics
migrations/
tests/
  support/                # database, HTTP, process and collector fixtures
  ...                     # integration suites named for invariant/feature
scripts/                  # reproducible checks and local prerequisites
deploy/                   # runtime/proxy examples and packaging support
docs/                     # configuration, operations and architecture notes
spec/openapi.yaml
```

Repository functions accept a connection/transaction explicitly where composition must be atomic. Avoid hidden pool checkouts inside an existing transaction, especially with bounded pool sizes.

## 2. Suggested relational model

Use `uuid` identifiers, `timestamptz` times, `bigint` nonnegative counters, `bytea` digests, PostgreSQL UTF-8 and explicit `COLLATE "C"` for byte-order text comparisons. Store UTC instants; render JSON timestamps with `Z`.

| Table | Required data and constraints |
| --- | --- |
| `system_state` | Exactly one seeded singleton row, `bootstrap_completed` initially false; completion can only advance to true through bootstrap |
| `accounts` | UUID PK, unique immutable username, constrained role, enabled, 32-byte unique key digest, created/updated timestamps |
| `code_reservations` | `short_code` text PK in bytewise collation; seed `api`; no owner, link FK, metadata, destination or secret |
| `links` | UUID PK; unique immutable short code FK to reservation; nullable owner FK to accounts with `ON DELETE CASCADE`; destination, nullable title, enabled, counter, created/updated timestamps; anonymous token digest |
| `link_tags` | Link FK with cascade, canonical name, composite PK `(link_id, name)`; no independent tag lifecycle needed |
| `visits` | UUID PK; link FK with cascade; immutable destination snapshot, occurrence time, GET/HEAD method, effective IP, nullable raw headers, device/browser/OS |
| `rate_buckets` | Stable primary key representing typed account UUID or canonical anonymous IP; lock/coordination row |
| `rate_charges` | Bucket FK, unique charge ID, database charge timestamp; index by bucket and time |

For `links`, enforce exactly one ownership mode:

```text
(owner_id IS NULL AND management_token_digest IS NOT NULL)
OR
(owner_id IS NOT NULL AND management_token_digest IS NULL)
```

Use a unique index for non-null management-token digests and length checks for all digests. Avoid `ON DELETE SET NULL` on owner: deleting an account deletes its links, it does not turn them anonymous. Reservation foreign keys point **from links to reservations**; cascading deletion must never run from a link into its reservation.

Store normalized `destination_search` and nullable `title_search` on links for consistent full-casefold/NFC search and title ordering. Short codes are already ASCII lowercase. Update derived values atomically with source values. Pin/document Unicode data versions; a future version change requires a deliberate persisted-data migration, not mixed normalization among instances.

Visits may obtain `short_code` through their link when rendering, because the code is immutable. A `client_ip` stored as `inet` must always have a host mask and be rendered as a canonical bare IP, never with `/32` or `/128`. Do not derive historical `redirect_url` from a join.

Do not index potentially 8,192-byte values with ordinary B-tree keys that can exceed PostgreSQL index-tuple limits. Use targeted trigram indexes if supported and justified for search; full headers used for reports need not be B-tree-indexed. Account/link/date/counter indexes are added with the relevant query work.

## 3. Credential and account mutation coordination

Initial authentication is a fresh lookup, but is not sufficient to authorize a later mutation. Select one **deployment-wide PostgreSQL transaction advisory read/write gate** for account authority. Document its constant key and lock hierarchy in code.

- Normal account-authenticated link mutations take the shared gate.
- Every account write, including creation, role/status changes, deletion and any API-key replacement, takes the exclusive gate. Bootstrap also takes it.
- With the gate held, re-read the current credential digest, enabled state and role. Reject stale credentials with 401, removed permissions with 403, and inaccessible links with 404 as appropriate.
- Hold the gate through commit. Consequently, if invalidation commits first, a later mutation cannot commit using the old authority. If the mutation acquires/revalidates first, invalidation waits until it commits.
- For enabled-admin removal, count enabled admins and apply the proposed state change while holding the exclusive gate. Protect deletion, demotion and disabling, including self-targeted operations.

This deliberately simple read/write gate serializes comparatively rare account changes and allows ordinary link mutations concurrently. Do not replace it with a process mutex. If it becomes a measured bottleneck, a finer-grained design needs its own proof and race tests.

Use READ COMMITTED for these guarded transactions. Acquire the advisory gate in one SQL command, then re-read authorization in a subsequent command with a fresh statement snapshot. Combining a waiting advisory-lock function and authorization reads in one SELECT/CTE can retain a snapshot taken before invalidation committed; an earlier REPEATABLE READ snapshot has the same hazard. The lock and revalidation must establish current committed authority, not merely execute in apparent SQL expression order.

Anonymous credentials are stored on the link itself. Lock their live link row `FOR UPDATE` before revalidating a token for a mutation. Token rotation, token-authorized edits and deletion use that same row lock. Account-authorized edits, deletion and admin token reset also lock the target live link row. Ordinary analytics inserts need only the link-row protocol below, not the account authority gate.

Global lock order:

1. Account-authority gate, if the operation needs it.
2. Account rows in UUID order, if explicit row locks are needed.
3. Link rows in UUID order, if more than one is needed.
4. Child rows: tags, visits, and other dependent data.

Code creation has no existing link row and uses reservation insertion under the already acquired creator-authority guard. Rate charging has its own short transaction and bucket lock; never hold a rate transaction open during link creation. Maintenance follows the same bucket coordination rules.

Account deletion holds the exclusive authority gate while deleting the account and cascading its links/visits in one transaction. An analytics worker holding a link lock may finish first; then deletion removes its visit. A worker running after deletion finds no link and discards its event. No public destination/status cache is allowed.

Retry only clearly aborted transactions, within a small bound and with the complete authorization check repeated. Do not retry unknown commit outcomes or charge the same HTTP attempt again during an internal create retry. Map exhausted transient database availability failures to a sanitized 503.

## 4. Other authoritative transactions

### Bootstrap

Migration coordination runs separately. In bootstrap's transaction, take the exclusive account gate, lock the singleton state row, require incomplete initialization, create the enabled admin and persist completion together. Print the key only after successful commit. Server startup does not execute bootstrap.

### Code allocation

Insert a code reservation and its live link in one creation transaction. Custom conflicts are 409. Generated candidates use `INSERT ... ON CONFLICT DO NOTHING RETURNING ...`, allowing retry without aborting the transaction. Try at most ten candidates. Rollback of unsuccessful creation must not leave an unused permanent reservation. A committed creation followed by deletion retains only the reservation row.

### Rate charge

Use a transaction distinct from creation. Ensure the stable bucket row exists, lock it, sample `clock_timestamp()` **after waiting for that lock**, and count charges in `(now - 60s, now]`. If below the configured limit, insert one charge and commit. Otherwise return 429 without inserting. See part 05 for the lower-limit `Retry-After` algorithm. If credential revalidation is needed for an account charge, take the shared account gate before the bucket lock; creation revalidates again later.

### Visit and counter

In a worker transaction, lock the live link `FOR UPDATE`; if missing, drop the event. Insert using the event UUID with `ON CONFLICT (id) DO NOTHING RETURNING id`. Increment `visit_count` only if an insert actually happened, in that same transaction. Hold the link lock until commit. Retries reuse the same event UUID, timestamp and destination snapshot. Counter overflow must roll back the insert and be observable as analytics failure, not corrupt the count.

## 5. Stateless cursor design

Use a versioned authenticated-encryption envelope with XChaCha20-Poly1305, a random 24-byte nonce and a deployment-wide 32-byte key from `FERL_CURSOR_KEY` (base64url). The prefix/version and resource type are authenticated. Never generate an instance-local fallback key. This is an implementation configuration choice allowed by the scaling contract; document provisioning and keep it stable across restarts/instances.

Encrypted payload contains only:

- Cursor format version and typed resource discriminator.
- A digest of canonical request context: authenticated account UUID or anonymous link UUID, resource/link, scope/owner, normalized filters and ordering.
- Last returned ordering values, with explicit null/present tags and UUID tie-breakers where applicable.

It never contains an API key, management token, digest of either credential, or database/configuration secret. Binding to an account UUID rather than its current key permits key rotation without changing ownership. Reauthorize every request before reading any page. Anonymous-token rotation may continue a link-bound cursor with the new valid token; the old token fails authentication.

Set a documented defensive cursor encoded-size bound large enough for a maximum 8,192-byte breakdown grouping value plus JSON escaping/envelope overhead; do not choose a small limit that makes valid raw-header buckets unpageable. Validate decoded types, version, authentication tag, context and boundary lengths. Reject invalid/incompatible cursors with 400. Do not log cursor text.

Use canonical serialization for the context digest; sorted normalized tags, UTC times and explicit defaults make equivalent requests equivalent. Page size is not part of the filter/order identity; allow `limit` to change. Fetch `limit + 1`, return at most `limit`, and generate a cursor from the last **returned** item only if the extra item exists. A cursor is a keyset boundary, never a lookup of the last row and never a stored server-side session.

## 6. HTTP pipeline and telemetry interfaces

The outer HTTP layer assigns the request ID, tracks a safe route template, applies response headers and strips HEAD bodies even for rejection/fallback responses. Dispatch `/api` at a whole-segment boundary before public-code matching. Preserve access to the raw URI path so framework percent-decoding/normalization cannot alias codes.

Business operation flow: route/method selection → credential parsing/lookup → operation authority → strict operation-specific input processing → service. Creation inserts its durable allowance step before body extraction and feature validation. Use explicit custom rejection mapping, not framework-default plaintext/422 errors or automatic API HEAD support.

Part 01 establishes structured stdout and low-cardinality instrumentation interfaces. Subsequent parts emit typed operation/outcome events without sensitive fields. Part 10 wires the full OTLP providers and verifies every signal and automatic instrument. Do not postpone privacy discipline until exporter setup.
