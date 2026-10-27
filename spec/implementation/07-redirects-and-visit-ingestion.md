# Part 07 — Public redirects and best-effort visit ingestion

**Dependencies:** part 06.

**Normative references:** business sections 8–10, 13–14; OpenAPI `followShortLink`, `followShortLinkHead`, redirect/HEAD responses and Visit schema.

## Outcome

Exact public GET/HEAD short links issue 302 responses and hand off immutable visit events through a bounded, nonblocking queue. A worker persists visits and counters atomically without compromising redirect availability or resurrecting deleted data.

## Files and responsibilities

- `src/redirects.rs`, raw-path dispatch in `src/http/router.rs`.
- `src/analytics/{mod,event,queue,worker,repository,user_agent}.rs`.
- Worker composition/shutdown hooks in `src/app.rs` and `src/shutdown.rs`.
- `tests/{routing,redirects,visit_ingestion,analytics_failures,visit_concurrency}.rs`.

## Tasks

### 1. Exact public routing

Apply path ownership before lookup: `/api` and `/api/*` only API; root and unsupported multi-segment paths backend 404; `/assets/*` backend 404 when called directly; other single segments can be codes. `/assets`, `/health`, `/api-sale` are valid code paths. Never rewrite `/assets` to `/assets/`.

Inspect original/raw URI path. Reject percent-encoded spellings, uppercase alternatives, trailing slash, extra segments and invalid code syntax with 404 rather than decoding/lowercasing/normalizing to a link. The router and any proxy fixture must preserve this behavior. Code lookup is exact and case-sensitive.

Implement explicit GET and HEAD, with unsupported methods on a short-link route returning 405 and `Allow: GET, HEAD`. HEAD error bodies are stripped. Do not let framework implicit GET-to-HEAD behavior enqueue twice or record method GET for a HEAD request.

### 2. Resolve and issue response

Use one fresh primary-database statement joining live link and owner to obtain link ID, immutable code and serialized destination only when link.enabled and owner absent/enabled. A missing/unavailable link returns the same 404 with no Location and no visit. A query failure is 503. Apply `no-store` to successes, misses, method failures and unavailable-storage responses.

Do not authenticate public requests. Ignore the incoming query entirely, including apparent credentials/tracking parameters. Build a response with exact captured destination in Location, status 302 and empty body. Never append incoming parameters or remove the stored query/fragment.

Define the event-creation point at the final successful redirect-response construction/return boundary. Emit only for the response the server is issuing as 302, never before a potentially failing resolution/header construction and never for a replacement error. This does not prove client receipt. Capture server UTC occurrence time there, not at worker persistence time.

Lookups begun after a committed edit/delete/suspension must see it. Already resolved responses may complete with their captured earlier value. Do not use a cached link or re-resolve after enqueuing.

### 3. Capture bounded raw analytics fields

Build one event with visit UUID, link ID/code, exact resolved redirect URL, occurrence time, original GET/HEAD method, canonical effective IP, optional raw Referer/User-Agent, and safe trace linkage.

For each optional header, accept exactly one complete valid UTF-8 value at most 8,192 bytes. Missing, duplicate, invalid UTF-8 or oversized values become null; do not truncate, concatenate, URL-normalize or reject the redirect. Inspect raw header bytes with UTF-8 validation rather than an ASCII-only convenience conversion that rejects otherwise permitted UTF-8 values.

Use part 05's IP resolver. Preserve complete referrer data without parsing it as a destination URL. The user's agent string is stored intact when valid, even when classification later fails.

### 4. Nonblocking handoff and local parsing

Use a bounded queue and `try_send` or equivalent immediate handoff. A full/disconnected queue drops the event with an observable low-cardinality outcome and leaves 302 unchanged. Do not spawn an unbounded task per redirect, await queue capacity, perform UA parsing/database writes on the redirect path, or add a durable local queue.

Workers use bounded concurrency, local maintained UA rules and bounded retries/backoff. Missing/unusable UA or parser failure yields unknown device/browser/OS; retain a valid raw UA when present. Map device to the exact five values and browser/OS to families only. Parsing failures are observable and cannot kill redirect serving.

### 5. Persist with idempotency and deletion safety

Use architecture.md's live-link lock → event insert → conditional counter increment transaction. Reuse UUID/time/snapshot for every retry, including unknown commit acknowledgement; event persistence is internally idempotent, unlike create requests. Never increment unless the insert happened in that transaction.

If link is deleted before the worker locks it, drop the event. If the worker locks first, deletion waits then removes the newly written visit/counter with the link. The worker must not insert links/reservations or fetch the link's current destination. Do not reject an event merely because its live link/owner was disabled after the redirect: its already-issued visit is legitimate.

Expose bounded enqueue/persist/drop/failure counts and queue depth through the telemetry facade. Prepare a bounded drain hook for part 10. A process crash may lose buffered events without a drop metric; document that limitation accurately.

## Verification gate

- Raw HTTP requests cover all routing boundary cases, including literal percent escapes, repeated/extra slashes and `api` versus `api-sale`. Avoid clients that normalize the URI before sending the test case.
- GET and HEAD each return exact 302/Location/no-store and eventually produce one visit when healthy; HEAD has zero body bytes. Invalid credentials/query strings do not alter behavior.
- Missing/disabled/suspended/deleted codes have identical 404 semantics and no visit; non-GET/HEAD methods have 405/Allow and no visit; resolution storage failure is 503.
- Test allowed internal destinations without network access and preserve complete stored query/fragment.
- Header fixtures cover absent/duplicate/invalid/oversized values and exact byte bounds; accepted full values persist with the resolved effective IP.
- Pause the worker after a redirect to A, edit destination to B, flush the event and verify snapshot A equals Location. A subsequent redirect records B. Visit insertion changes no link metadata timestamp.
- Replay one event concurrently and prove one row/one increment. Inject failure between insert and increment and prove neither commits.
- Saturate/disconnect queue, fail parsing/writes and stop workers; redirect latency is independent of worker completion and response remains 302. Retry/drop behavior is bounded and observable.
- Delete while an event is pending and in both link-lock orderings; no orphan/recreated visits or released reservation. Verify concurrent increments are not lost.

**Handoff:** expose immutable event fixtures and deterministic worker controls for report and process tests. Main scenarios: 7, 13–15, 19, 21–22, 24–28, 37–40 and 50.
