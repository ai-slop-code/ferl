# Part 05 — Effective client IP and durable creation allowance

**Dependencies:** part 04.

**Normative references:** business sections 3, 7, 10, 13–13.1; OpenAPI `createLink`, `RetryAfter`, and `RateLimited`.

## Outcome

A shared, thoroughly tested client-IP resolver and PostgreSQL-backed rolling limiter are ready for link creation. The limiter is independently durable and correct across database connections/processes; the creator pipeline is wired into the public creation endpoint in part 06.

## Files and responsibilities

- `src/client_ip.rs`: peer/trusted-chain normalization.
- `src/rate_limit/{mod,bucket,repository,service,maintenance}.rs`.
- Creation eligibility/charge interface in `src/links/` or `src/auth/`.
- `tests/{client_ip,rate_limits,rate_limit_concurrency}.rs`.

## Tasks

### 1. One effective-IP function

Take the real connection peer `IpAddr` from the transport; do not obtain it from a client header. Normalize IPv4-mapped IPv6 to IPv4 for both peer and forwarded elements and perform trust comparisons consistently. Normalize or explicitly handle mapped proxy configuration so an equivalent IP cannot bypass trust/rate identity.

If peer is untrusted, ignore every forwarding header. If trusted, accept exactly one X-Forwarded-For field with at most 32 comma-separated bare IPs and optional surrounding element whitespace. Reject the entire chain on missing/duplicate fields, empty elements, invalid UTF-8, malformed addresses, ports, brackets/non-bare address syntax, `unknown`, or more than 32 hops; fall back to peer.

For a valid chain, append peer and walk right-to-left past trusted hops. Choose the first untrusted IP, or the leftmost supplied IP if all are trusted. `Forwarded`, `X-Real-IP` and unrelated headers are never consulted. Use transport header bounds and the 32-element limit to bound parsing; a partially parsed chain is never used.

Return a canonical IP type used identically for anonymous bucket identity and visit capture. No logging/metric label contains the resulting IP or raw headers.

### 2. Creator eligibility interface

Model only `Account(account_uuid)` and `Anonymous(effective_ip)` as rate subjects. An admin uses the account bucket/limit. API key values are never bucket identifiers.

Authenticate supplied credentials first. Missing credentials require anonymous creation enabled; otherwise 403 `anonymous_creation_disabled`. Management tokens are valid credentials but cannot create, so return 403 `forbidden` before charging. Invalid/disabled credentials produce 401 before charging.

For eligible account requests, take the shared account gate and revalidate before committing a charge. Keep the charge transaction short and finish it before reading the creation body. The create transaction later revalidates again: a rotation between charge and creation can reject creation while preserving its charge.

### 3. Rolling window transaction

Use stable per-subject bucket rows with a primary key distinguishing account and anonymous identities. In one transaction:

1. Ensure the bucket exists (`ON CONFLICT DO NOTHING`) and lock its row `FOR UPDATE`.
2. Sample database `clock_timestamp()` after the lock is obtained. Do not use transaction `now()` sampled before a long wait.
3. Prune/ignore charges at or before `now - interval '60 seconds'`; count only timestamps in `(now - 60s, now]`.
4. If count `n < L`, insert exactly one timestamped charge and commit.
5. If `n >= L`, insert nothing and compute the next availability as below. Release the transaction without resetting or extending any charge.

To compute Retry-After, sort active charge times ascending as `t[0..n-1]`. The needed expiration is `t[n - L] + 60 seconds`: enough charges must expire to leave at most `L - 1`. Return `max(1, ceil(expiration - now))` seconds. This handles a restarted fleet whose configured limit is lower than the number of still-active charges; always selecting the oldest charge is wrong in that case.

If checking, charging or commit acknowledgement fails, return 503 `storage_unavailable`. Do not grant an unaccounted create. A charge whose commit outcome is unknown may have consumed allowance; do not blindly retry it as a new charge.

### 4. Independent durability and maintenance

Represent a successful charge as an internal capability/result tied to this HTTP attempt; it is not a user-visible token. Link creation must run in a new transaction so any later error/rollback leaves the charge committed.

Expired charges may be removed opportunistically and/or by bounded maintenance. Keep stable bucket rows initially to avoid row-deletion/recreation lock races. If bucket cleanup is implemented, prove it shares the same coordination and cannot create two simultaneous lock identities. Multi-instance maintenance must be safe and bounded.

## Verification gate

- Proxy fixtures cover no trust, untrusted spoofing, right-to-left mixed chains, all-trusted chains, 32/33 elements, duplicate headers, malformed/empty/ported addresses and mapped IPv4 equivalence.
- Real-DB tests admit five anonymous or sixty account attempts, reject the next with positive accurate Retry-After, and show independent subjects do not share allowance. Admins use sixty.
- Race at least twice the configured limit through independent connections using a barrier; admitted count cannot exceed the limit.
- Seed charge times with a controlled database-side test clock interface to test exactly 60 seconds, just before/after the boundary, multiple equal timestamps and reduced startup limits. Production time always comes from PostgreSQL; there is no HTTP clock override.
- Rate rejection does not insert a charge or extend the wait. Rotation, reconnection/restart and a creation-transaction rollback preserve live charges.
- Credential/creator permission rejection consumes no allowance. Database failure returns 503.

**Handoff:** expose an explicit authenticate/eligibility/charge result consumed by part 06, plus the IP resolver used by part 07. Main scenarios: 16–19 and the limiter portion of 27; body-validation charging is finalized next and cross-process scenario 33 in part 11.
