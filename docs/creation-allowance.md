# Effective client IP and creation allowance (Part 05)

Production composition is `AppState::from_config`: `creation` is the PostgreSQL
`CreationService`, and `client_ip` is the shared `ClientIpResolver`. The executable
serves `app::transport_service(app::router(state))`, installing Axum's accepted
socket `ConnectInfo<SocketAddr>`. Part 05 exposes no link-creation or charging
route. Health, accounts, and current-stage fallbacks do not consume allowance.

## Effective IP

`CanonicalIp` has private fields, redacted Debug, canonical equality, and explicit
`address()` / host-masked `host_network()` accessors for persistence and authorized
representations. IPv4-mapped IPv6 becomes IPv4, including peers, forwarded hops,
anonymous identities, and the existing visit DTO converter.

`ClientIpResolver::resolve_request(&Parts)` requires transport `ConnectInfo`.
Missing context fails internally; neither Host, forwarding headers, an arbitrary
canonical-IP extension, nor a synthetic loopback address can replace it.
`resolve(peer, headers)` is the same core resolver for future visit ingestion.

- With no trust or an untrusted peer, every forwarding header is ignored.
- A trusted peer requires exactly one UTF-8 X-Forwarded-For field, containing
  1–32 comma-separated bare IPs with surrounding element whitespace permitted.
  The complete chain is validated before selection. Missing/duplicate fields,
  empty elements, trailing commas, ports, brackets, zones, unknown/non-bare values,
  invalid UTF-8 and 33+ hops all fall back to the canonical peer.
- Walk valid hops right-to-left past trusted hops, with the already-trusted peer
  implicitly appended. Select the first untrusted address, or the leftmost
  supplied address when all are trusted. Forwarded and X-Real-IP have no authority.
- Trust comparison keeps the **original configured CIDR**, without rewriting its
  prefix length. Canonical IPv4 is tested against IPv4 ranges and its equivalent
  mapped IPv6 representation against IPv6 ranges. Thus mapped /128 is exact,
  mapped /120 corresponds exactly to IPv4 /24, and /96 covers mapped IPv4.
  Broader IPv6 prefixes retain their original membership: for example `::/80`
  contains the mapped space; `::/0` trusts both IPv6 and mapped IPv4. This is an
  explicit consequence of configuring such broad trust, not prefix truncation.
- Parsing allocates at most 32 IP values and stops before pushing a 33rd value.
  Accepted header bytes are bounded by the HTTP transport. There is no additional
  byte cap that would reject a valid chain solely for allowed whitespace padding.

Diagnostics expose only `peer_only`, `forwarded`, or `fallback`; raw and resolved
IPs and header values never become logging fields or metric dimensions.

## Part 06 consumption contract

After selecting the actual POST route/method, split the request into parts and
body, then call `state.creation.prepare(&parts).await`. The service has no body
argument and cannot consume body frames. It performs:

1. Fresh `auth::authenticate` of any supplied credential.
2. Creator eligibility: missing + disabled anonymous creation is 403
   `anonymous_creation_disabled`; enabled anonymous creation uses effective IP;
   enabled user/admin uses account UUID; valid management tokens are 403
   `forbidden`; malformed/unmatched/revoked/disabled/deleted credentials are 401
   `invalid_credentials`, never anonymous fallback. Storage failure is 503;
   corrupt stored values remain sanitized 500.
3. An independently committed allowance transaction. Account requests use
   `begin_account_mutation`, the **shared** authority guard, with fresh credential,
   enabled-status, existence and role lookup in a separate post-wait statement.

Only acknowledged COMMIT returns `ChargedAttempt`. Its private fields retain a
`Creator` with `RateSubject::{Account(AccountId), Anonymous(CanonicalIp)}` and,
for accounts, the presented authentication evidence as a `Principal`. There is
no ID/boolean constructor, Clone/Copy, serialization, global capability store,
or user-visible charge token. Compile-fail doctests enforce those boundaries.
`creator()` borrows it; `into_creator()` consumes the attempt. Part 06's creation
service consumes it by ownership rather than minting an ID-only "already
authenticated" flag. `links::service::LinkService::create` is the production
consumer; its private `Creator` retains the presented `Principal` for the new
protected creation transaction.

The production Part 06 route now performs strict body/media/feature validation
after `prepare` and opens a **new** creation transaction. For an account, it passes
the retained `creator.principal()` to `begin_account_mutation` again; it never
authorizes from `subject()` or the previously observed role. A rotation between
charge acknowledgement and body completion can reject this later transaction
while retaining the charge. Charges are never refunded for bad input, feature
rejection, conflicts, retries, cancellation, or later creation rollback.
Allocation/CRUD/tokens are Part 06; redirects/visit capture remain Part 07.

`http::rejection::creation_error` maps the reusable failures, including a positive
integer Retry-After, through the production request-ID/no-store/HEAD envelope.
Actual public-route charge/body/creation order is covered by
`tests/creation_order.rs` and the link HTTP/OpenAPI suites; the Part 05 fixture
orchestration remains historical service-level evidence, not endpoint evidence.

## PostgreSQL protocol and precision

At the Part 05 rate-accounting increment, schema version **1**, PostgreSQL
**17.x**, and the published initial migration were unchanged. Part 08 later adds
schema version 2 discovery indexes only; it does not change allowance tables or
the accounting protocol. Subject partial unique indexes establish one stable bucket.
The rate transaction is READ COMMITTED. Lock order is:

```text
shared account-authority gate (account creators only)
→ subject bucket row FOR UPDATE
→ that bucket's charge rows
```

`INSERT ... ON CONFLICT DO NOTHING` is followed by a separate fresh-snapshot
bucket lookup/lock. A conflict winner invisible to the insertion snapshot never
causes an alternate bucket. Buckets have no account FK cascade, so key/role
changes keep identity; account deletion retains old accounting and username
reuse creates a new UUID/subject.

Sample `clock_timestamp()` in a statement **after acquiring the bucket lock**.
Private `DatabaseInstant` decodes/encodes PostgreSQL's exact finite signed binary
microseconds since 2000, checks PostgreSQL's supported range, and does not use
Chrono date addition or request/cursor `Timestamp` normalization. All count,
insert, cleanup and Retry-After SQL receives this one sampled value. The column
default is not used for charging. No production HTTP/environment clock override
exists. Deterministic tests alone bind a database-produced controlled instant.

Count only `(sample - 60 seconds, sample]`; future charges are excluded. If
`n < L`, prune a bounded expired batch, insert exactly one explicitly timestamped
charge and commit. Otherwise roll back without inserting, refreshing, or deleting
any charge. PostgreSQL counts and selects an indexed order statistic; Rust never
materializes the charge history. For sorted active timestamps, Retry-After is:

```text
max(1, ceil(t[n - L] + 60 seconds - sample))
```

PostgreSQL 17 EXTRACT yields exact NUMERIC interval seconds, not floating point;
conversion to a checked 1–60 integer follows CEIL. Seven active charges with a
reduced limit of three require the **fifth oldest** expiration. Tests cover that
case, equal times, exact 60-second age, ±1 microsecond, fractional rounding,
future timestamps, exact binary round trips and timestamps beyond Chrono's range.

## Bounds, failure and lifecycle

- Pool connect/acquire uses the existing minimum of the two configured bounds.
  Authentication retains its own deadline. `FERL_CREATION_TIMEOUT_SECS` defaults
  to 15 (1–300) for the entire authentication/eligibility/charge operation.
- The authority helper temporarily installs its lock bound as local statement
  timeout. Rate composition explicitly restores `FERL_DB_STATEMENT_TIMEOUT_SECS`
  and `FERL_DB_LOCK_TIMEOUT_SECS` before bucket SQL. A database trigger test
  observes the independently configured 3s/1s settings after shared revalidation.
- Rejection/error rolls back with a 2s application deadline within the total
  operation bound. A cancelled public future **owns** its transaction and drops
  it; SQLx queues rollback, and the server-side statement/lock deadlines remain
  active while pending SQL finishes. Private repository futures only borrow a
  connection: cancelling one never authorizes continuing that transaction.
  Drop/rollback is obligatory. Tests cancel after an uncommitted insert while a
  controlled PostgreSQL lock is held, then prove rollback and connection reuse.
- No internal retry occurs, including after clearly transient errors. COMMIT
  I/O failure or a deadline during COMMIT is `CommitOutcomeUnknown`, mapped to
  sanitized storage 503. There is no successful capability without acknowledged
  commit, automatic replay, compensating delete, HTTP deduplication, or exactly-once
  promise. Safe cancellation diagnostics explicitly preserve commit uncertainty.
- Cleanup is opportunistic on admission: **at most 128** expired charges at or
  before the same `sample - 60s` cutoff, in the owning bucket transaction.
  Stable buckets remain forever initially; idle subjects may retain expired rows
  until used again. Active-window queries ignore those rows. Cleanup failure
  aborts admission. Concurrent processes use identical bucket locking, with
  expired-history pruning tested across two independent workers. No worker,
  maintenance interval, unbounded DELETE, or extra shutdown task is introduced.
- Existing bounded HTTP drain, pool close and schema monitor cancellation apply.
  The service holds no transaction while a body is read or a link is created.

Privacy evidence captures the **production** JSON logging/filter composition,
including dependency-target suppression, real SQL failures, timeout/cancellation,
cleanup failure and both forms of COMMIT fault. A narrow prerequisite fix moves
the previous nested per-layer logging filters to global composition: the original
composition suppressed required application events after database activity; the
new executable test reproduced the missing diagnostics and passes with the fix.

## Executable evidence

- `tests/client_ip.rs`: full-chain/trust matrix, mapped prefix membership, actual
  TCP ConnectInfo through production transport composition, missing-peer failure.
- `tests/rate_limits.rs`: default 5/60/60, independent subjects, one-connection
  pool, configuration/reduced limits, observed statement bounds, credentials and
  body ordering, independent rollback, post-charge rotation, identity lifecycle,
  unaffected production routes.
- `src/rate_limit/tests.rs`: real PostgreSQL deterministic clock boundaries,
  lower-limit order statistic, binary range/roundtrip checks, bounded cleanup.
- `tests/rate_limit_concurrency.rs`: ten independent one-connection pools versus
  five allowance slots, invisible first-use winner, shared guard coexistence,
  invalidation order, post-lock clock sample, after-insert timeout/cancellation.
- `tests/rate_limit_failures.rs`: actual reusable HTTP errors validated with locked
  YAML/JSON Schema 2020-12, ref-sibling conjunction/formats, invalid controls,
  production privacy capture, aborted COMMIT, and truly committed/lost-ack COMMIT.
  The loopback-only test proxy waits for COMMIT CommandComplete, permits an
  independent durable-row assertion, then drops delivery. The two faults are
  explicitly distinguished in assertions.
- `tests/rate_limit_process.rs`: two independently executed workers against one
  owned schema, shared account/IP allowance and cleanup, process replacement,
  pool recreation, and owned PostgreSQL named-volume restart. These workers are
  test-binary entry points, never production commands/routes.

Run `sh scripts/check.sh`; it includes all suites and compile-fail doctests.
`IsolatedTestDatabase::verify` bounds new database scenarios to 90s and performs
fixture cleanup before propagating assertion failures. All shared-container
operations retain the existing interprocess harness lock. Process output is
captured with bounds and children are terminated/reaped; JoinSet tasks and proxy
connections have cancellation/cleanup owners.
