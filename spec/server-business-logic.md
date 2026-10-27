# ferl: server business logic

## 1. Purpose and decision authority

This is the implementation specification for the first release of **ferl**, a simple, robust URL-shortening server written in Rust and inspired by Shlink.

The deliverable is **only the ferl backend server**: its JSON API, public HTTP redirects, account management, visit analytics, persistence, and OpenTelemetry observability. Account administration, link management, and analytics reports are API capabilities, not screens or browser workflows.

**No UI is in scope.** Do not build or bundle a web frontend, landing page, admin dashboard, analytics charts, HTML templates, static UI assets, or an interactive API-documentation UI. Public short links issue HTTP redirects directly, without preview or interstitial pages. Requests handled by the Rust server use the same HTTP/JSON contracts for browsers and other clients.

A ferl web-client SPA will be a separate project. At the shared public origin, `/` is reserved for that SPA, and `/assets/*` for its static files, served by a reverse proxy/static host. The Rust server serves the API and redirects; it does not serve the SPA files. Section 8 defines this routing boundary without specifying or implementing the UI.

A separate Rust CLI client is planned; its implementation is outside this specification. The server's bootstrap command is an operator function of the server binary, not that client.

**Status:** ready for implementation. The product decisions below were confirmed with the project owner. Routine contract defaults were selected with the owner's explicit permission and are identified in section 3. There are no outstanding product questions for this scope. The implementing agent must not infer additional features from Shlink.

“Must” identifies a requirement. Acceptance scenarios in section 15 describe observable behavior rather than a prescribed code structure.

The companion [OpenAPI 3.1 contract](openapi.yaml) defines the machine-readable HTTP interface. Both documents must agree. This document additionally defines authorization, normalization, concurrency, and operational invariants that schemas alone cannot express. Resolve any discovered conflict before implementation rather than silently choosing one contract.

## 2. Confirmed product decisions

| Area | Decision |
| --- | --- |
| Server | Rust. |
| Deliverable | Backend server only: JSON API and public HTTP redirects. No UI or bundled client application. |
| Public routing | Reverse proxy/static host serves the separate SPA at `/` and its files under `/assets/*`. `/api` and `/api/*` belong to the server API, with business endpoints under `/api/v1` and health under `/api/health/*`. Other single-segment paths go to short-link lookup. |
| Persistence | PostgreSQL. |
| Deployment | Initially one instance. The stateless server must support adding interchangeable instances behind a load balancer, sharing PostgreSQL, without an application redesign. Cross-instance correctness is required from the first release. |
| Actors | Admin accounts, regular user accounts, and anonymous callers. |
| Provisioning | Admins create accounts; no public registration. |
| Authentication | One active API key per account. No password login. |
| Administration | Admins manage every account and link, including anonymous links. Protect the last enabled admin. |
| Ownership | Users manage their own links. Anonymous links have a per-link management token. |
| Listing | All accounts, including admins, list their own links by default. Admins explicitly request `scope=all` to list everyone's links, including anonymous links. Ownership belongs to the account, not a particular API-key value. |
| Search and filtering | Search destination, short code, and title; filter by enabled flag, creation-date range, and tags. Admins can filter all-links listings by owner. Sort by creation time, title, or visit count. |
| Tags | Per-link, case-insensitive labels, created when used. Set/remove tags through link creation/editing and discover tags within authorized scope. Multiple-tag filters support any/all matching, defaulting to any. |
| Anonymous creation | Operator-configurable; off by default. Disabling creation does not invalidate existing management tokens. |
| Link management | Owners, management-token holders, and admins can edit destinations, disable/re-enable links, and delete links within their authority. |
| Link titles | Optional, caller-supplied, editable titles. The server does not fetch destination pages to discover titles. |
| Custom codes | Available to authenticated users and admins; unavailable to anonymous creators. |
| Code policy | One public base URL. Generated codes are 8 random lowercase letters/digits. Custom codes are 3–64 lowercase letters/digits or hyphens. All codes are globally unique, immutable, and never reused after deletion. Reserve the code `api`. |
| Destination policy | Absolute HTTP(S) URLs with any host, including localhost/private hosts. Reject malformed URLs and embedded credentials. Validate without fetching the destination. |
| Duplicate destinations | Always create an independent link; do not deduplicate. |
| Automatic expiry | None: no expiration timestamp or redirect-count cap. |
| Creation limits | Configurable. Defaults: 5 attempts/minute per anonymous client IP and 60 attempts/minute per account, including admins. No total-link quota. |
| Rate accounting | Check authentication and permission to create before charging. Eligible attempts consume an allowance even if validation or a code conflict subsequently rejects them. |
| Account suspension | Disabling an account blocks authentication and suspends its redirects. Re-enabling restores otherwise-enabled links. |
| Account deletion | Delete the account's links and visits; retain their code reservations. |
| Redirects | GET and HEAD return 302 with `Cache-Control: no-store`. Ignore incoming query parameters. Preserve the stored destination's query and fragment. Unknown, disabled, and deleted links return 404. |
| Visit collection | Count GET and HEAD redirects, including bots/previews. A visit means a redirect was issued, not that the destination loaded. |
| Visit fields | UTC timestamp, full referrer URL, raw client IP, raw user agent, parsed device/browser/OS, and the exact destination used for that redirect. |
| Analytics access | Per-link visit records and summaries, accessible to the owner, that link's management-token holder, and admins. |
| Retention | Individual visits retained indefinitely until link deletion. Keep a lifetime visit counter until deletion. |
| Analytics failure | Continue redirecting if visit recording fails; analytics may undercount. |
| Client IP | Peer IP by default. Trust `X-Forwarded-For` only through explicitly configured trusted proxies. |
| Observability | OpenTelemetry traces, metrics, and logs over OTLP, plus structured stdout logs correlated with traces. Collector outages must not fail requests. Exclude credentials, full URLs, IPs, and raw user agents from telemetry. |
| API documentation | A checked-in OpenAPI 3.1 YAML contract for the server, suitable for the separately developed CLI and SPA. |

## 3. Selected contract defaults

These are documented defaults, not additional decisions attributed to the owner. They complete the agreed behavior without requiring further API-design questions.

| Area | Selected default |
| --- | --- |
| HTTP interface | Business JSON API under `/api/v1`; health under `/api/health`; redirects at `/{short_code}`. Root and SPA asset hosting belong to the reverse proxy/static host. |
| Public base URL | A configured HTTP(S) origin without a path prefix, user information, query, or fragment. An optional trailing `/` is normalized away. Generate returned short URLs from configuration, never from request headers. |
| Reserved codes | `api`; matching is exact. `health` and `assets` are available short codes because `/api/health/*` and `/assets/*` are distinct multi-segment routes. |
| Credentials | `Authorization: Bearer <secret>` for both API keys and management tokens. Distinguishable credential formats; at least 256 bits of cryptographically secure random secret material. Store only a SHA-256 digest of the complete secret. |
| Identifiers | Opaque UUIDs for accounts, links, and visits. Public link-management routes identify a link by its immutable short code. |
| Usernames | Immutable, unique among existing accounts, matching `[a-z0-9_-]{3,64}`. Deleted usernames may be reused; a new account always gets a new UUID. |
| URL length | Both the submitted and serialized destination must be at most 8,192 UTF-8 bytes. |
| Title validation | Nullable plain text. Trim surrounding Unicode whitespace; a supplied string must then contain 1–200 Unicode scalar values and no control characters. Empty/blank strings are invalid; `null` means no title. |
| Tag validation | At most 20 input tags per link/request. Each input name is at most 256 Unicode scalar values; its canonical form is 1–64 scalar values with no control characters. Canonicalization is trimming surrounding Unicode whitespace, locale-independent full Unicode case folding, then NFC normalization. Deduplicate and sort canonical names in ascending UTF-8 byte order. |
| Text search | A single literal substring, not a regular expression or tokenized query. Trim input; require 1–256 Unicode scalar values with no control characters. Compare using locale-independent full Unicode case folding and NFC normalization. |
| Link ordering | `sort=created_at`, `title`, or `visit_count`; default `created_at`. `order=asc` or `desc`; default `desc`. Title comparisons use the same case-folded NFC form as search; null titles always sort last. UUID is the final tie-breaker in the selected direction. |
| Body size | At most 64 KiB for API request bodies. |
| Rate window | A rolling 60-second window, backed by PostgreSQL. Positive integer limits configured at server startup. |
| Pagination | Cursor-based collection responses, default limit 50, maximum 200. Live pagination, not a frozen snapshot across requests. |
| Time | Server-generated UTC timestamps. JSON timestamps use RFC 3339 with `Z`; filter timestamps with explicit offsets are accepted and converted to UTC. |
| Analytics grouping | One grouping dimension per request: UTC day, HTTP method, full referrer, client IP, raw user agent, device class, browser family, or OS family. |
| Optional visit headers | For requests accepted by the HTTP transport, store complete valid UTF-8 values up to 8,192 bytes each. Missing, duplicated, invalid, or oversized values become `null`; analytics extraction never rejects the redirect. |
| Proxy parsing | Rules in section 10; no trusted proxies by default. |
| HTTP errors | Structured JSON with stable error codes. Validation is 400; unauthorized object access is concealed with 404. See section 12. |
| Operational endpoints | `/api/health/live` and `/api/health/ready`. |
| OpenAPI artifact | `spec/openapi.yaml`, using OpenAPI 3.1.0. Include it with server releases; no additional HTTP documentation endpoint is required. |
| Telemetry defaults | Service name `ferl`; OTLP over HTTP/protobuf when an endpoint is configured. Structured stdout logs always enabled. Parent-based trace sampling with root ratio `1.0`, configurable. |

Rust framework, PostgreSQL client, migration tooling, internal module layout, and compatible library versions remain implementation choices. They must satisfy this specification; this does not authorize additional product behavior.

## 4. Actors and authorization

An **anonymous caller** has no credentials. A **management-token holder** is still not an account: the credential grants authority over exactly one anonymous link. Admin/user are the only stored account roles.

| Operation | No credentials | Link management token | User API key | Admin API key |
| --- | --- | --- | --- | --- |
| Follow an active short link | Yes | Yes | Yes | Yes |
| Create a generated-code link | If enabled | No | Yes | Yes |
| Create a custom-code link | No | No | Yes | Yes |
| List links | No | No | Own links | Own by default; all with `scope=all` |
| Discover tags | No | No | Tags on own links | Own by default; all with `scope=all` |
| Read/edit/disable/re-enable/delete a link | No | Token's link | Own links | Any link |
| Read per-link visits/reports | No | Token's link | Own links | Any link |
| Replace an anonymous management token | No | Token's link | No | Any anonymous link |
| Read own account / rotate own API key | No | No | Yes | Yes |
| List/create/change/delete accounts or reset another account's key | No | No | No | Yes |

Authorization rules:

1. Public redirect and health routes do not require authentication; supplied credentials do not change their behavior or bypass link suspension.
2. On business API routes, a supplied invalid, malformed, revoked, or disabled-account credential returns 401. Never silently treat it as anonymous, even if anonymous creation is enabled.
3. Multiple or ambiguous `Authorization` values return 401. Credentials in query strings or request bodies are not authentication mechanisms.
4. A valid credential with the wrong operation scope returns 403. A valid account/token attempting an object-level operation on a link it does not control receives the same 404 as an unknown link.
5. An unauthenticated request to a protected API operation returns 401. Anonymous creation while disabled returns 403.
6. Ownership is assigned by the server and immutable. Authenticated creation belongs to the authenticating account; anonymous creation has no account owner. Do not allow caller-supplied ownership or adopting/transferring links.
7. Public redirects never expose management secrets or analytics.

## 5. Accounts and API keys

### 5.1 Account model

An account contains `id`, `username`, `role` (`admin` or `user`), `enabled`, `created_at`, and `updated_at`, plus its current API-key digest. Newly created accounts are enabled; the default creation role is `user`.

- Account creation returns the account and its initial plaintext API key once, after persistence succeeds.
- Ordinary reads/lists never return a plaintext key or its digest.
- Users and admins can rotate their own key. Admins can reset any account's key.
- A replacement atomically invalidates the previous key. Rotation is not an additional concurrently valid key.
- A disabled account cannot authenticate. Disabling preserves its key digest; re-enabling makes the current key usable again. An admin can reset the key while the account remains disabled.
- Account role changes take effect using the same current key. There are no role-bearing session tokens to become stale.
- There are no passwords, sessions, public signup, email verification, or self-service recovery flows in this scope.

### 5.2 Initial admin

Provide a server-operator command equivalent to:

```text
ferl-server bootstrap-admin --username <username>
```

It uses the configured database, creates one enabled admin, and writes its generated API key once to the command's result output. The secret must not also appear in application logs or telemetry.

Bootstrap is a one-time database initialization operation, not a public HTTP endpoint. Concurrent attempts must result in only one successful bootstrap. Persist completion in PostgreSQL and reject later attempts. Starting or restarting the HTTP server must never generate a replacement admin/key or reopen bootstrap automatically.

### 5.3 Administrative changes and deletion

- Admins can promote/demote accounts and disable/re-enable them, including their own account when another enabled admin remains.
- The last enabled admin cannot be disabled, demoted, or deleted. Enforce this atomically even for concurrent requests handled by different server instances.
- Disabling an account suspends every owned link without changing each link's own `enabled` flag.
- Re-enabling an account restores redirects only for links whose own flag is enabled. Previously individually disabled links remain disabled.
- Deleting an account invalidates its key and deletes its links and all associated visits. Their code reservations remain permanently occupied.
- Account deletion must not return success while its links remain publicly usable. The account/link/visit deletion and reservation preservation are one logical atomic operation.

## 6. Links, destinations, and management tokens

### 6.1 Link model and state

A live link contains:

- `id`, `short_code`, `destination_url`;
- `title`: optional caller-supplied metadata, represented as a string or `null`;
- `tags`: a canonical, sorted array of unique strings, initially empty;
- `owner_id`: an account UUID or `null` for an anonymous link;
- `enabled`: initially `true`;
- `created_at`, `updated_at`;
- `visit_count`: initially zero, counting successfully recorded visits;
- a management-token digest only when the link is anonymous.

API representations also include `short_url` and computed `active`:

```text
active = link exists AND link.enabled
         AND (link.owner_id is null OR owner account is enabled)
```

This counter is best-effort product analytics, not a guaranteed count of all responses delivered to clients. There is no automatic time/count expiry.

### 6.2 Destination validation

- Accept only absolute `http` or `https` URLs with explicit `http://` or `https://` authority syntax (scheme matching is case-insensitive), a host, and a valid port if present.
- Reject any embedded username/password/user-info component, including an explicitly empty one.
- Reject leading/trailing whitespace, literal ASCII whitespace/control characters, malformed URLs, and values exceeding the configured contract bound in section 3. Do not silently trim input.
- Use a standards-compliant URL parser and serializer. Store and return the validated serialization, including standard host/IDN and escaping normalization. It must be safe to place in a `Location` header.
- Preserve the destination's query and fragment in that serialization. Do not strip tracking parameters, reorder parameters, or perform application-specific URL rewriting.
- Accept private IPs, localhost, internal hostnames, and destinations on ferl's own origin under the same syntax rules. Do not perform DNS lookup, reachability checks, destination fetching, metadata extraction, or application-level redirect-loop detection.
- Apply the same validation on creation and destination updates.

### 6.3 Short-code allocation

- Generated codes contain exactly 8 independently, uniformly selected characters from `abcdefghijklmnopqrstuvwxyz0123456789`, using a cryptographically secure generator.
- Custom codes match `[a-z0-9-]{3,64}` exactly. Uppercase letters, underscores, spaces, Unicode, and characters outside that set are invalid. Leading/trailing hyphens are allowed.
- Generated and custom codes share one global namespace, including reservations retained for deleted links.
- A requested occupied/reserved custom code returns 409 `short_code_unavailable`. This includes both previously deleted codes and `api`.
- Concurrent requests cannot allocate the same code. Database uniqueness is authoritative; a prior existence check alone is insufficient.
- A generated collision is retried with a fresh candidate. As a selected operational bound, try at most 10 candidates and then return 503 `code_generation_unavailable`; never overwrite an existing reservation.
- Codes cannot be renamed. Destination edits do not change `short_code` or `short_url`.
- Every successfully created link is independent, including repeated identical destinations from the same caller. Request retries are separate attempts; there is no create-idempotency contract in this release.

### 6.4 Management and deletion

- Authorized callers can replace `destination_url`, set or clear `title`, replace `tags`, and set `enabled` to `true` or `false`. Validate and commit all supplied changes atomically. Actual metadata/state changes update `updated_at`; recording a visit does not.
- Omitted PATCH fields remain unchanged. Unknown fields, immutable fields, `null` for required values, and empty updates return 400.
- Delete removes the live link, its tag associations, its management-token digest if any, and all its visits, including historical destinations. Retain only a minimal permanent code reservation, without the destination, title, tags, owner, credentials, or analytics.
- A deleted link cannot be restored; creating the same destination creates a different code unless another available custom code is supplied.
- With an otherwise-valid credential, GET/manage/delete of a deleted link returns 404, including repeated DELETE requests. The deleted link's former management token is revoked, so requests authenticating with it instead return 401 under section 4.
- Suspended or individually disabled links remain visible/manageable to authorized admins; an individually disabled link also remains manageable by its enabled owner or its token holder.

### 6.5 Anonymous management tokens

- Anonymous creation returns both the link and a newly generated management token, once.
- The token authorizes only that anonymous link's operations from section 4. Knowing the short code, client IP, or destination is not proof of ownership.
- Token holders and admins can replace an anonymous link's token. Return the replacement once and atomically invalidate the previous token.
- Ordinary link reads cannot retrieve the plaintext token. A lost token requires an admin reset; there is no unauthenticated recovery endpoint.
- Turning off anonymous creation does not disable existing anonymous links or their tokens.
- Authenticated account-owned links do not acquire management tokens. An admin request to issue one for such a link returns 409 `not_anonymous_link`.

### 6.6 Caller-supplied titles

- Any eligible creator may supply `title`, including an anonymous creator. Omission or explicit `null` at creation stores no title.
- A supplied string follows section 3's title validation. Store and return the trimmed value as plain text. A value of the wrong type, an empty/blank string, an overlong string, or remaining control characters returns 400 `invalid_request`.
- An authorized owner, matching management-token holder, or admin may edit the title. On PATCH, omission preserves the current title, a valid string replaces it, and explicit `null` clears it. Setting `null` is a valid update, not an empty PATCH.
- Creation, detail, update, and list responses always include `title`, using JSON `null` when absent. Persist it in PostgreSQL with the link.
- Changing a destination preserves its title unless the same update explicitly changes or clears it. Title changes do not change the code, destination, enabled state, or visit count.
- The server does not fetch or generate a title, render it as HTML, or substitute the destination/short code when it is absent. Column headings and the display of absent titles are client presentation decisions.

### 6.7 Per-link tags

- Any eligible creator, including an anonymous creator, may provide `tags` as an array of strings. Omission on creation stores `[]`; explicit `null` is invalid.
- Apply section 3's validation/canonicalization to every input name, then deduplicate. For example, `Work`, `work`, and ` work ` all become `work`. Unicode names, internal spaces, and punctuation are allowed; names are plain text, not paths or identifiers with special routing meaning.
- The maximum of 20 applies to input array elements before deduplication. Empty/blank names, invalid types, and names outside the stated bounds return 400. Return canonical arrays in creation/detail/update/list responses, including `[]` when no tags exist.
- Link owners, matching management-token holders, and admins can replace the link's whole tag set through PATCH. Omission preserves it; `[]` removes all tags. Concurrent replacements have ordinary atomic last-committed-write semantics; do not implicitly merge sets.
- Tags exist through their association with links. The same name on two accounts' links does not give either account authority over the other link or its tags. Internal storage may share canonical name rows, but authorization and discovery must always derive from visible links.
- `GET /api/v1/tags` discovers currently used tags in the permitted scope as defined in section 11.5. Unused tags disappear from discovery automatically; tag names can be reused. Deleting/removing a tag association does not reserve its name.
- There are no standalone tag-creation, bulk tag-rename, or bulk tag-deletion operations. Editing one link's tags never edits other links. No tag-level visit analytics endpoint is required.

## 7. Creation rate limits

Two separate categories apply only to `POST /api/v1/links`:

| Creator | Key | Default |
| --- | --- | --- |
| Anonymous | Effective client IP from section 10 | 5 attempts in a rolling 60 seconds |
| Authenticated user/admin | Account UUID | 60 attempts in a rolling 60 seconds |

Processing order:

1. Authenticate supplied credentials, if any.
2. Check permission to invoke creation: enabled account, or no credentials with anonymous creation enabled. A management token cannot create links.
3. Atomically check and charge that creator's rate allowance.
4. Validate the request body and its feature permissions, validate the destination/custom code, and attempt creation.

Consequences:

- Eligible requests rejected after step 3 still consume their allowance, including malformed bodies, invalid destinations, anonymous custom-code requests, conflicts, and later persistence failures. Transport-level rejection before the application handles a request does not need accounting.
- A rate-limited attempt does not add another charge or extend the wait. Return 429 `rate_limited` and `Retry-After` as a positive integer number of seconds until an allowance can next become available, assuming no other accepted attempts.
- Count charged timestamps in `(now - 60 seconds, now]`; admit only when that count is below the configured limit. A charge expires at exactly 60 seconds of age. Compute `Retry-After` until enough charges expire to admit another attempt, including when a restarted server has a lower configured limit.
- Use database time and atomic database operations. Concurrent requests across all instances cannot collectively exceed the allowance; adding instances must not multiply it. Key rotation, restart, or redeployment must not reset it.
- Charging is durable independently of the subsequent link-creation transaction; a failed creation must not roll back the charge.
- Expired accounting entries can be pruned. No total-link quotas or separate redirect/management/report rate limits are required in this release.
- If allowance checking cannot be performed, return 503; do not silently allow unaccounted creation.

## 8. Public routing and redirects

### 8.1 Shared-origin routing

Apply the following routing rules at path-segment boundaries, in the stated precedence order:

| Request path | Owner | Behavior |
| --- | --- | --- |
| Exactly `/` | Reverse proxy/static host | Serve the separate SPA entry point. |
| `/api` or any path starting with `/api/` | ferl server | Handle only API routes, including `/api/v1/...` and `/api/health/...`. Unrecognized API paths return an API 404 and never fall through to redirect lookup or the SPA. |
| Any path starting with `/assets/` | Reverse proxy/static host | Serve separate SPA assets; missing files return 404, not the SPA entry point. |
| Any other single-segment `/<code>` | ferl server | Perform short-link lookup under the code-validation and redirect rules. Unknown/invalid codes return 404. |
| Other paths | Deployment/server router | Return 404; do not fall back to the SPA or resolve only part of a path as a short code. |

- The deployment reserves `/` for the SPA even before the SPA is deployed. A direct request to the Rust server's `/` returns 404; it is not a shortcode and must not render a UI. The proxy/static host owns the eventual root response.
- `/api` is a reserved whole segment, not a broad text prefix. For example, `/api-sale` can be a short code, while `/api/unknown` cannot.
- `/assets` without a trailing slash is a short-code path. Do not automatically rewrite it to `/assets/`. `/health` is also a short-code path; operational health endpoints live entirely within the API namespace.
- Path ownership does not change the agreed short-code syntax. Routing an arbitrary single segment to the lookup handler does not make it a valid or existing code.
- Do not configure a site-wide SPA history fallback. A missing short code must remain a short-link 404 rather than returning the SPA entry point. The separate SPA must fit this path contract; fragment-based navigation such as `/#/settings` is compatible because fragments are not sent to the server. Its internal navigation design remains part of the separate SPA project.
- Reserve SPA assets under `/assets/` rather than implicitly claiming additional root-level paths. Any future expansion of reserved paths requires an explicit routing-contract change and consideration of existing short codes.

### 8.2 Redirect behavior

For an exact, active `/{short_code}` path:

1. Resolve the live link and, for owned links, the account's enabled state. Capture the resolved destination as an immutable value for this request.
2. Return `302 Found`, `Location: <resolved destination_url>`, and `Cache-Control: no-store` for GET or HEAD, with an empty response body.
3. Attempt to record one visit for that response, including the same resolved destination, according to section 9.

Additional requirements:

- Match codes exactly without lowercasing, redirecting to a canonical code spelling, or accepting a trailing slash. Percent-encoded spellings of short-code characters are not alternate routes; reject them as 404.
- The incoming short URL's query is completely ignored. It is neither merged into the destination nor used for authorization or analytics fields.
- HEAD has no response body and creates a visit just as GET does. Other methods on a short-link path return 405 with `Allow: GET, HEAD` and create no visit.
- Unknown, individually disabled, account-suspended, and deleted links return indistinguishable 404 responses without a `Location` header and without visit recording.
- Add `Cache-Control: no-store` to unsuccessful short-link responses too, so later creation/re-enabling is not hidden by negative caches.
- A persistence failure that prevents resolution returns 503, not 404. An analytics-only failure after successful resolution does not prevent the redirect.
- Link updates, disabling, deletion, and account suspension apply to lookups beginning after their transaction commits. A redirect already resolved before a concurrent change may complete with that earlier state.
- The Rust server's `/` and unsupported paths return 404. The shared-origin proxy routing in section 8.1 serves the SPA separately and dispatches API paths before short-link lookup.

## 9. Visit analytics

### 9.1 Collection and fields

A visit corresponds to one ferl-issued 302 from GET or HEAD, including requests from bots, preview fetchers, and repeat visitors. There is no unique-visitor estimate or bot exclusion. Do not count management requests, failed redirects, or unsupported methods.

Each successfully recorded visit contains:

| Field | Meaning |
| --- | --- |
| `id` | Unique visit UUID; use it to avoid duplicate persistence if internal processing retries an event. |
| `link_id`, `short_code` | The redirected link. |
| `redirect_url` | Non-null, immutable snapshot of the exact serialized destination used in this response's `Location` header, including its query and fragment. |
| `occurred_at` | UTC time when the redirect response was issued, not background write time. |
| `method` | `GET` or `HEAD`. |
| `client_ip` | Canonical effective client IP, including IPv6 support, determined by section 10. |
| `referrer` | Complete received `Referer` header, including its path/query when present, subject to section 3's bound; otherwise `null`. |
| `user_agent` | Complete received `User-Agent` header subject to the same bound; otherwise `null`. |
| `device` | Parsed `desktop`, `mobile`, `tablet`, `other`, or `unknown`. |
| `browser` | Browser family, or `unknown`. |
| `os` | Operating-system family, or `unknown`. |

Parse device/browser/OS locally with a maintained parser. Parser choice is an implementation detail; parsing failures produce `unknown` rather than failing requests. Browser/OS versions are not separate fields. A missing/unusable raw user agent produces unknown parsed fields. Missing optional headers are still valid visits. Treat header values as data, never executable content. Ordinary HTTP transport size/protocol limits still apply before analytics extraction.

No country/geolocation field or external enrichment service is required. No per-account/global analytics endpoint is included in this release.

### 9.2 Reliability and retention

- Redirect success has priority over analytics completeness. Use a bounded, nonblocking handoff to analytics processing; queue saturation, parsing/write errors, or worker unavailability must not delay or fail the redirect path.
- Transient in-memory analytics queues are permitted under the stateless design: they are best-effort buffers, never authoritative state. Process failure may lose queued visits.
- Capture `redirect_url` from the same resolved value used for the HTTP response and carry it in the queued event. The worker must not look up the link's current destination to populate historical data. A subsequent destination edit never rewrites earlier visit snapshots; retries preserve the original event value.
- For each persisted visit, insert the record and increment the link's lifetime `visit_count` together atomically. A retry of the same visit UUID must not increment it twice.
- Consequently, the count matches persisted visit records; it may undercount actual issued redirects. Report responses are eventually consistent with asynchronous recording and do not promise immediate visibility after following a link.
- Generate events only for responses the server issues as redirects. Do not claim that a record proves client receipt or successful destination navigation.
- Record observable enqueue/write failures and drops. Abrupt process termination may lose buffered data before a drop metric can be emitted; do not claim otherwise.
- Retain visits and their associated counter indefinitely while the link exists, including while it or its owner is disabled.
- Historical destination snapshots have the same ownership, retention, and deletion rules as the rest of each visit. Existing per-link reports include visits to all past destinations; individual records expose which destination was used.
- Deletion removes visits and the counter. An event queued before deletion must not recreate a link, recreate visits for its reservation, or restore a counter afterward.
- A graceful shutdown attempts a bounded queue/exporter flush; redirect availability and shutdown completion must not depend indefinitely on analytics.

### 9.3 Reports

Owners, the matching anonymous management-token holder, and admins can request:

1. Paginated raw visit records for one link, newest `occurred_at` first, with visit UUID descending as the tie-breaker.
2. A count summary containing `lifetime_visits` and `matching_visits` for an optional time interval.
3. A paginated breakdown grouped by one of `day`, `method`, `referrer`, `client_ip`, `user_agent`, `device`, `browser`, or `os`.

All three accept optional `from` (inclusive) and `to` (exclusive). With neither supplied, cover all retained history. A supplied interval with `from >= to` is invalid. Boundaries use `occurred_at`.

- `lifetime_visits` ignores date filters; `matching_visits` counts visits inside them.
- `day` means a UTC calendar date. Other groupings use exact stored field values. A missing value is a distinct `null` bucket; `unknown` classifications are ordinary string buckets.
- Breakdown items are `{ "value": <string-or-null>, "visits": <integer> }`. Return groups in deterministic ascending value order, with `null` first; day values use `YYYY-MM-DD`.
- Totals cover the complete filter interval, not just a page. A valid interval containing no recorded visits returns zero matching visits/an empty collection; its lifetime total still covers all history.
- Report endpoints return 503 if PostgreSQL cannot answer. Analytics read failures are not fabricated empty results.
- Apply link-level authorization before any records or aggregate information are returned. Suspension does not prevent an admin from investigating a link's analytics.

## 10. Effective client IP and proxy trust

Use the same resolved IP for anonymous rate accounting and visit records.

1. Parse and canonicalize the connection's peer IP. Normalize IPv4-mapped IPv6 to its IPv4 form so it cannot produce an alternate rate-limit identity.
2. If the peer is not in configured trusted proxy IPs/CIDRs, ignore all forwarding headers and use the peer IP.
3. For a trusted peer, accept one `X-Forwarded-For` header containing a comma-separated chain of at most 32 bare IP addresses, with surrounding element whitespace allowed. Do not accept ports, `unknown`, empty elements, or malformed addresses.
4. With a missing, duplicated, malformed, or oversized chain, fall back to the peer IP. Do not reject the redirect or select a partially parsed attacker-controlled element.
5. For a valid chain, append the peer and walk right to left, skipping trusted proxies. Select the first untrusted address. If every address is trusted, select the leftmost supplied address.
6. `Forwarded`, `X-Real-IP`, and other client-IP headers have no authority under this policy.

The deployment must configure trust for proxies that actually establish/append the forwarding chain. Trust configuration is empty by default and validated at startup. Do not automatically trust private networks or every proxy.

## 11. HTTP API contract

This section contains selected API defaults implementing the preceding business rules. All API responses, including errors and secret-bearing creation responses, use `Cache-Control: no-store`. Accept and return JSON with the appropriate content type. Admin and analytics endpoints return data only; authentication failures return the specified errors rather than redirecting to a login page. Do not add HTML rendering or frontend routes.

### 11.1 Endpoint inventory

| Method and route | Authorization | Request / result |
| --- | --- | --- |
| `GET /api/v1/me` | Account | Own account representation. |
| `POST /api/v1/me/api-key` | Account | Replace own key; return `{ "api_key": "..." }`. |
| `GET /api/v1/admin/accounts` | Admin | Paginated accounts. |
| `POST /api/v1/admin/accounts` | Admin | `{ "username": "...", "role"?: "user" or "admin" }`; return `{ "account": ..., "api_key": "..." }`. |
| `GET /api/v1/admin/accounts/{id}` | Admin | Account representation. |
| `PATCH /api/v1/admin/accounts/{id}` | Admin | At least one of `role`, `enabled`; return updated account. |
| `DELETE /api/v1/admin/accounts/{id}` | Admin | Delete account and its links/visits. |
| `POST /api/v1/admin/accounts/{id}/api-key` | Admin | Reset key; return `{ "api_key": "..." }`. |
| `POST /api/v1/links` | Eligible creator | Required `destination_url`; optional `short_code`, nullable `title`, and `tags`. Return `{ "link": ... }`, also including `management_token` for anonymous creation. |
| `GET /api/v1/links` | Account | Paginated links with scope, search, filters, and ordering from section 11.3. Defaults to own links, including for admins. |
| `GET /api/v1/links/{code}` | Link authority | Link representation. |
| `PATCH /api/v1/links/{code}` | Link authority | At least one of `destination_url`, `title`, `tags`, `enabled`; return updated link. |
| `DELETE /api/v1/links/{code}` | Link authority | Delete link and its visits. |
| `POST /api/v1/links/{code}/management-token` | Matching token or admin | Replace anonymous token; return `{ "management_token": "..." }`. |
| `GET /api/v1/links/{code}/visits` | Link authority | Visit collection; optional `from`, `to`, `limit`, `cursor`. |
| `GET /api/v1/links/{code}/stats` | Link authority | `{ "lifetime_visits": 0, "matching_visits": 0 }`; optional `from`, `to`. |
| `GET /api/v1/links/{code}/stats/breakdown` | Link authority | Required `group_by`; optional `from`, `to`, `limit`, `cursor`; grouped collection. |
| `GET /api/v1/tags` | Account | Paginated tag discovery with optional `scope`, `owner_id`, `q`, `limit`, `cursor`; see section 11.5. |

“Link authority” means the permitted actors in section 4, not merely a valid API key. Routes shown with `{code}` use the same immutable short code as the public URL. Request sketches use `?` to mark optional fields; it is not literal JSON syntax.

### 11.2 Representations and request rules

- Account representation: `id`, `username`, `role`, `enabled`, `created_at`, `updated_at`.
- Link representation: `id`, `short_code`, `short_url`, `destination_url`, `title`, `tags`, `owner_id`, `enabled`, `active`, `visit_count`, `created_at`, `updated_at`.
- Visit representation: the fields in section 9.1, including the immutable `redirect_url` snapshot.
- Collection envelope: `{ "items": [...], "next_cursor": <string-or-null> }`. Accounts sort by `created_at` descending, then UUID descending. Link ordering is specified in section 11.3, tag ordering in section 11.5, and visit/breakdown ordering in section 9.
- Cursors are opaque, validated, and tied to their resource/filter/order context. They must not confer access, contain secrets, require process-local session state, or bypass normal authorization. Reapply authorization on every page.
- `limit` must be an integer from 1 through 200. Invalid cursors or incompatible filters return 400. Concurrent writes/deletes may change subsequent pages; no cross-request snapshot is promised.
- Reject unknown body/query fields and invalid types with 400. Scalar query parameters cannot be repeated; only explicitly array-valued parameters such as `tag` allow repetition. Optional means omitted, not explicitly `null`, unless the representation explicitly permits null. An anonymous request supplying `short_code` returns 403 after its rate allowance is charged.
- Key/token replacement POSTs take no body. Do not accept caller-chosen keys or tokens.
- Creation returns 201 after commit. Reads/updates/key replacements return 200. Successful deletions return 204 without a body.
- API errors and bodies are valid JSON. HEAD responses never include a body, including error responses. Unsupported API methods return 405 with the appropriate `Allow` header; only documented operations are exposed.
- A server-generated request ID is returned as `X-Request-ID` for every response and is available for log/trace correlation. Do not trust an incoming value as this identifier.

### 11.3 Link listing, search, filters, and ordering

- `GET /api/v1/links` accepts `scope=own` or `scope=all`; omission means `own` for both users and admins. Invalid scope values return 400.
- `scope=own` returns only links whose `owner_id` equals the authenticated account UUID. Rotating its API key does not change ownership or remove links from that account's listing.
- Only admins may request `scope=all`; other accounts receive 403. This scope includes all account-owned and anonymous links, including links suspended by account disabling. It changes visibility, not ownership or mutation authorization.
- Both scopes include individually enabled and disabled live links unless filtered; deleted links are absent. An empty result returns `{ "items": [], "next_cursor": null }`.
- Each item is the complete link representation, including `short_url`, `destination_url`, `title`, `visit_count`, and `created_at`. A client needs no per-link detail or analytics request just to display those columns.
- Return complete JSON records independently of display preferences. Choosing/reordering/hiding columns and formatting dates belongs to clients; this release has no `columns` or field-projection parameter.

Supported query parameters, in addition to `scope`, `limit`, and `cursor`:

| Parameter | Meaning |
| --- | --- |
| `q` | Case-insensitive literal substring search across stored `destination_url`, `short_code`, and `title`; a match in any one field is sufficient. Null titles do not match. Use section 3's text-search normalization and bounds. |
| `enabled` | `true` or `false`, matching the link's own enabled flag. This is distinct from computed `active`: an enabled link on a suspended account still matches `enabled=true` but has `active=false`. |
| `created_from` | Inclusive RFC 3339 lower bound on `created_at`. |
| `created_to` | Exclusive RFC 3339 upper bound on `created_at`. If both bounds exist, require `created_from < created_to`. |
| `owner_id` | An account UUID or literal `anonymous`. Permitted only for an admin using `scope=all`; limits results to that owner or to links with no account owner. |
| `tag` | One or more tag names, encoded as repeated query parameters, for example `tag=work&tag=rust`. Canonicalize and deduplicate using the same rules as stored tags; allow 1–20 input values. Commas inside a value are part of a tag, not separators. |
| `tag_mode` | `any` or `all`, default `any` when filtering by tags. `any` requires at least one selected tag; `all` requires every selected tag. Supplying this parameter without `tag` is invalid. |
| `sort` | `created_at`, `title`, or `visit_count`; default `created_at`. |
| `order` | `asc` or `desc`; default `desc` for every sort field. |

- Apply ownership/authorization and all filters before pagination. Distinct filter categories combine with AND; the OR inside `q` and the chosen tag mode apply only within those filters.
- Tag filtering must return each matching link once even if several selected tags match it. An unknown tag simply matches no association; `any` may still match other selected tags, while `all` fails if any selected tag is absent.
- `owner_id` supplied by a non-admin returns 403. An admin supplying it without `scope=all` gets 400. A well-formed nonexistent/deleted account UUID gives an empty result, not a broader scope or a fallback to own links.
- Treat search text literally, including `%`, `_`, and backslashes. Do not expose SQL wildcards, regular expressions, fuzzy matching, stemming, or full-text query syntax through `q`. Bind query values safely and normalize compared fields consistently with the query.
- Order by the requested field, then link UUID in the same direction. For titles, compare case-folded NFC strings in UTF-8 byte order and always put null titles last. Sort by the persisted visit counter rather than scanning raw visit records per link.
- Cursors carry the last ordering values and are bound to the resource, account/scope, normalized filters, and sort/direction. Recheck authorization on every page. A changed filter/sort context returns 400 rather than silently continuing another result set. Cursor continuation must work after deletion of the previous page's last link.
- With unchanged data, pagination must return every matching link exactly once. This remains live pagination: concurrent edits and visit increments can move links when sorting by title or visits, so pages may repeat or omit moved links. There is no cross-request snapshot promise.
- Choose database indexes/query plans suitable for scoped pagination, tag membership, search, and supported ordering. Pagination must operate on distinct matching links, not joined tag rows.

### 11.4 Server support for future CLI operations

The following examples identify required server capabilities. They do not specify or implement the separate CLI's argument parsing, configuration, or terminal UI. Authenticated calls use the existing bearer API-key contract.

| Intended client operation | Server request | Relevant result |
| --- | --- | --- |
| `ferl list` | `GET /api/v1/links`, optionally using search/filter/sort parameters and following pagination | The account's own links; complete records let the client select display columns, including tags. |
| `ferl <url>` | `POST /api/v1/links` with `{ "destination_url": "<url>" }`, optionally also `title`, `tags`, or an allowed custom code | 201 and a link containing its `short_code` and `short_url`. |
| `ferl delete <short-url>` | `DELETE /api/v1/links/{code}`, identifying the code from the short URL | 204 after deleting that one authorized link and its visits. |

The requested display columns map directly to `short_url` (shortened URL), `destination_url` (stored original/destination URL), `title`, `visit_count` (best-effort recorded visits), and `created_at` (creation timestamp). The account's current API key accesses links created using its earlier keys too.

Deletion identifies a unique short link by its code. Because multiple independent links may share a destination, deleting one code must not delete other links with that destination. No deletion-by-destination or display-specific endpoint is required.

### 11.5 Tag discovery

- `GET /api/v1/tags` is account-authenticated. Management tokens cannot enumerate tags across links; they can read tags in their own link representation.
- Use `scope=own` by default, including for admins. `scope=all` and optional `owner_id` follow exactly the authorization and owner-filter rules in section 11.3.
- Optional `q` performs literal, case-insensitive substring search on canonical tag names only, with the same text-search normalization and bounds. It does not search destinations or titles. Other link-list filters are not accepted by this endpoint.
- Each item is `{ "name": "work", "link_count": 3 }`: a canonical name and the number of distinct, visible live links carrying it in the selected scope/owner filter. Include disabled links; exclude deleted links. Counts cover the entire scope, not just one page of links or tags.
- Return only names used on at least one visible link, ordered by canonical name in ascending UTF-8 byte order. Apply existing collection limits and cursor conventions, binding cursors to scope, owner, and normalized search. A regular user must never learn another account's tag names or counts through discovery.
- Removing the last visible association makes a tag disappear from that scope immediately after commit, even if another account still uses the same name. An empty result uses the ordinary empty collection envelope.

### 11.6 OpenAPI contract and compatibility

- Maintain `spec/openapi.yaml` as a valid OpenAPI 3.1 document covering every business API operation, tag discovery, health endpoints, and public GET/HEAD redirects. Include request/response schemas, nullability, bearer authentication, parameter serialization, pagination, errors, and relevant headers.
- This is a checked-in/release documentation artifact, not a frontend or interactive documentation application. Root/SPA asset hosting and the server bootstrap command are described in prose, not exposed as API operations.
- Keep operation IDs stable within API v1. Additive changes must be compatible with this contract; incompatible wire changes require an explicit API-version decision.
- Validate the document and its references in required checks. Validate representative request/response examples against its schemas and verify implemented routes/statuses against it. Authorization, canonicalization, byte limits, cross-field rules, and concurrency invariants also require the business-level acceptance scenarios.
- Health bodies use `{ "status": "ok" }` on success and `{ "status": "unavailable" }` for an unready instance. All health responses include `X-Request-ID` and `Cache-Control: no-store`.

## 12. Error semantics

Error response shape:

```json
{
  "error": {
    "code": "short_code_unavailable",
    "message": "The requested short code is unavailable."
  },
  "request_id": "..."
}
```

| Status | Stable code / use |
| --- | --- |
| 400 | `invalid_request`: malformed JSON, field validation, unsupported fields, bad filters/cursors. |
| 401 | `invalid_credentials` or `authentication_required`; include `WWW-Authenticate: Bearer`. |
| 403 | `forbidden`, or `anonymous_creation_disabled`. |
| 404 | `not_found`: unknown/inaccessible API object; unknown/unavailable public short link. |
| 405 | `method_not_allowed`; include `Allow`. |
| 409 | `short_code_unavailable`, `username_unavailable`, `last_admin_required`, or `not_anonymous_link`. |
| 413 | `request_too_large`. |
| 415 | `unsupported_media_type` for a body requiring JSON. |
| 429 | `rate_limited`; include `Retry-After`. |
| 503 | `service_unavailable` when bounded server request capacity is exhausted; `storage_unavailable`, `code_generation_unavailable`, or another documented dependency-unavailable code. |
| 500 | `internal_error` for unexpected failures. |

Messages must not expose SQL, database connection strings, stack traces, credentials, destination/referrer URLs, or other accounts' information. Unknown and unauthorized objects use indistinguishable responses. Unexpected errors must be observable internally using sanitized error categories and request/trace IDs.

## 13. Persistence, statelessness, and concurrency

- PostgreSQL is authoritative for accounts, key/token digests, links, code reservations, visits/counters, bootstrap completion, and live rate-limit accounting.
- The server requires no durable local filesystem state, sticky sessions, or process-memory session store. Configuration is external; restarting preserves all authoritative behavior.
- One active server instance is the initial deployment, not a correctness constraint. Additional instances of the same compatible server version must be able to serve the same service behind a load balancer using the same PostgreSQL database. Basic multi-instance correctness must be verified in the first release.
- Request-local data, database pools, telemetry buffers, and the explicitly best-effort analytics queue may exist in memory. They must not become required durable state.
- Do not cache destinations, authorization decisions, or account status across requests in this release. Fresh lookups ensure committed edits/revocations/suspensions affect subsequent requests.
- Use database constraints, transactions, and database-level coordination for unique usernames/codes, exclusive ownership, account/link relationships, secret replacement, visit/count consistency, deletion, bootstrap, rate accounting, and last-enabled-admin protection. A process-local mutex or singleton task cannot be the sole protection for a shared invariant.
- Revalidate the current credential and relevant authorization state when committing protected mutations, with appropriate locking/conditional writes. Concurrent rotation or account disabling must not let a stale key complete a new mutation after invalidation has already committed.
- Account-role/status changes that could remove an enabled admin must serialize their invariant check with the update. Two concurrent requests cannot each rely on the other admin remaining enabled.
- Returned success means the mutation committed. A connection failure after commit can leave a client uncertain; do not promise exactly-once request execution or automatic create deduplication.
- A database outage is not equivalent to an empty result, invalid credential, missing link, or available rate allowance. Fail dependent requests with 503. Analytics-only failures follow section 9 instead.
- Database migrations must preserve existing links, credentials, visits, reservations, and bootstrap state. Startup must not reset existing data. Schema must be compatible before readiness succeeds.
- Validate static configuration before serving traffic. Required configuration includes the PostgreSQL connection and public base URL. Other configuration includes the anonymous-creation flag, both creation limits, trusted proxy CIDRs, HTTP listener, and telemetry settings.
- Configuration is read at startup; changing it requires a restart for this release. Invalid values fail startup with a useful, sanitized message. Secret configuration values must not be echoed.

### 13.1 Horizontal scaling contract

The supported application topology is:

```text
Clients -> Reverse proxy / load balancer
             |-- / and /assets/* -> Separate SPA static host
             `-- API and short links -> ferl instance A --+
                                     -> ferl instance B --+-> Shared PostgreSQL
                                     -> ferl instance N --+
```

- Any ready instance can serve any request. Credentials and pagination cursors issued through one instance must work through another. No sticky sessions, instance-owned links, or durable local files are required.
- Instances use the same public base URL, business configuration, and any deployment-wide cryptographic configuration. Trusted-proxy configuration must identify the actual forwarding path and yield the same effective client IP regardless of which instance handles a request. Configuration changes affecting business rules must be coordinated across the fleet.
- All authoritative reads and writes in this release use PostgreSQL's primary/read-write endpoint. A mutation committed through instance A must affect subsequent authorization checks, redirect lookups, and management requests on instance B under the existing concurrency rules. Asynchronously replicated reads must not weaken these guarantees.
- Creation rate allowances are service-wide per account/IP. Bootstrap, code allocation, token/key rotation, deletion, and last-admin protection retain their guarantees across processes.
- Each instance may run its own bounded analytics worker/queue. All workers write into the shared visit store and update counters atomically; concurrent writes cannot lose increments. Deletion on one instance must also prevent late queued events on another from recreating data. Buffered-event loss during instance failure follows the existing best-effort policy.
- Maintenance that can run on several instances, such as pruning expired rate charges, must be safe under concurrent execution. Apply migrations once per deployment or serialize them with database-level coordination; simultaneous startup must not race schema changes. Instances become ready only with a compatible schema.
- Database connection pools, queue sizes, and timeouts must be configurable and bounded per instance. Deployment sizing must budget total PostgreSQL connections across all instances; adding servers does not create database capacity automatically.
- Scaling out/in requires deploying/removing instances and updating load-balancer membership, with adequate PostgreSQL capacity. It must not require moving link ownership, resetting credentials/rate limits, copying local state, or changing the API. Mark an instance unready before graceful drain/removal.
- This is an application-correctness and deployment requirement, not a promise of linear throughput growth. PostgreSQL query/write capacity, connection limits, and contention on popular-link analytics counters can still bound throughput. Use the specified telemetry to measure these limits as load grows; a particular replica count or requests-per-second target has not been specified.

## 14. OpenTelemetry and operational health

### 14.1 Signals and export

Provide all three signals using OpenTelemetry SDK/exporters:

- **Traces:** incoming HTTP requests, database operations, link creation/management, credential operations, and analytics processing. Propagate valid W3C trace context; do not propagate arbitrary baggage. Asynchronous analytics spans may link to the originating request context.
- **Metrics:** request count/latency/in-flight count; redirect outcomes; link creation outcomes and rate rejections; authentication/authorization failure categories; database errors/latency; analytics enqueue/persist/drop/failure counts and queue depth.
- **Logs:** structured lifecycle, request-completion, and error logs on stdout and via OTLP when configured. Include request ID and trace/span IDs where a context exists.

Use stable low-cardinality attributes such as route template, method, response status, operation, and bounded outcome/error category. Use `service.name=ferl`, build-derived `service.version`, a distinct opaque `service.instance.id` resource attribute per running instance, and configurable environment/resource attributes. Trace sampling is configurable; metrics and error logs must not depend on trace sampling.

OTLP over HTTP/protobuf is the selected initial transport. Support a configured endpoint, export headers, timeouts, and standard service/resource/sampling configuration. Export is enabled when an endpoint is configured; without one the server runs with structured stdout logs and no attempted OTLP export. Validate malformed exporter configuration at startup, but do not require collector reachability for startup/readiness.

Exporters must have bounded queues, bounded timeouts/retries, and bounded shutdown flushes. Slow/unreachable collectors must not fail HTTP requests, block redirects, grow memory without bound, or create recursive log-export failures. Surface exporter trouble through rate-limited sanitized stdout diagnostics.

### 14.2 Telemetry data boundaries

Detailed visit data belongs only in PostgreSQL and authorized analytics responses. Application/HTTP/database instrumentation must use an explicit safe attribute policy:

- Never emit API keys, management tokens, authorization headers, secret configuration, request/response bodies, full destination/referrer/request URLs (including visit `redirect_url` snapshots), query strings/search text, titles, tag names, client IPs, raw user agents, or captured forwarded headers.
- Use route templates such as `/{short_code}` and `/api/v1/links/{code}`, not resolved paths containing actual codes. Do not use account/link/visit IDs, codes, URLs, IPs, or request IDs as metric labels.
- Disable or sanitize default HTTP URL/address/header capture and SQL parameter/statement capture that could leak the excluded data. Sanitized database operation names and error classes are sufficient.
- Error messages from URL parsing, database drivers, and other libraries must be sanitized before logging/export.
- Request IDs and trace/span IDs may appear in logs/traces for correlation. Product-analytics header capture must not be reused automatically as telemetry attributes.

### 14.3 Health and shutdown

- `GET /api/health/live`: 200 while the HTTP process can respond; no database or collector dependency. Return a minimal status object without configuration details.
- `GET /api/health/ready`: 200 when initialization/schema checks have completed, a bounded PostgreSQL probe succeeds, and the instance is accepting work; otherwise 503. A draining instance is unready. Collector health and analytics backlog do not determine readiness.
- When bounded global HTTP admission is exhausted, either health route may return the structured 503 `service_unavailable` error instead of invoking its health handler. This capacity response does not add a database/collector dependency to liveness or change the exact `{ "status": "unavailable" }` body for a completed readiness check that fails its conditions.
- Health endpoints require no credentials and do not create visits or consume creation allowance.
- Graceful shutdown stops accepting new work, allows bounded completion of in-flight work, and attempts bounded analytics/telemetry flushes before closing resources.

## 15. Acceptance scenarios

The implementing agent must verify these behaviors with appropriate unit/integration/API tests. Persistence and concurrency guarantees require real PostgreSQL integration coverage, not only mocked repositories. Horizontal-scaling scenarios must run against at least two independent server processes sharing that database, so process-local coordination cannot accidentally satisfy them. These scenarios are requirements for that implementation, not a claim that server code/tests already exist.

### Access and accounts

1. Fresh configuration rejects anonymous creation with 403; enabling it permits generated-code creation and returns one management token. Existing tokens and redirects work after creation is switched off again.
2. Missing credentials cannot manage links. Invalid supplied credentials cannot fall back to anonymous creation. A valid link token cannot create links or access account operations.
3. User A cannot list, read, mutate, or inspect analytics for user B's links. Object requests return the same 404 as nonexistent links. Admin access succeeds.
4. A management token can operate on precisely its link. Another anonymous link's code returns 404. Token replacement invalidates the old token; normal reads never reveal either token/digest.
5. Bootstrap succeeds once, including under concurrent attempts. Restart preserves its result and cannot issue a new initial key.
6. Account creation/reset/self-rotation reveal a key only in the success response. Rotation immediately invalidates the previous key for subsequent checks; persisted credentials survive restart.
7. Disabling an account invalidates authentication and makes otherwise-active owned links return 404. Re-enabling restores them while individually disabled links remain disabled.
8. Concurrent demotion/disable/delete requests cannot remove all enabled admins. Rejection leaves the accounts and their links unchanged.
9. Deleting an account removes its live links and visits, invalidates its key, and prevents reuse of every former code.

### Shortening and redirects

10. Valid public, localhost, private-IP, IPv6, and internal-host HTTP(S) destinations can be shortened without network lookup/fetching. Malformed URLs, user-info, non-HTTP schemes, whitespace/control characters, and overlong input/serialization are rejected.
11. Repeated identical destinations create independent links. Generated codes match the exact alphabet/length; valid authenticated custom codes work; anonymous custom-code attempts return 403.
12. A custom code conflict, including a reservation left by deletion, returns 409. Concurrent allocation produces one winner and no overwrite. Injected generated collisions retry within the defined bound.
13. Edits preserve the code and use the updated destination on subsequent lookups. Code/owner edits are rejected. Deletion makes redirects and management with otherwise-valid credentials return 404 and permanently reserves the code. The deleted link's revoked management token returns 401.
14. GET and HEAD each issue 302 with the exact stored `Location` and `no-store`; HEAD has no body. Destination query/fragment survive; incoming query parameters do not alter them.
15. Unknown, disabled, account-suspended, and deleted links produce indistinguishable 404 responses with no location/visit. Trailing slashes, uppercase alternatives, and percent-encoded code spellings do not alias a link. Unsupported methods return 405.

### Limits, IP handling, and durable state

16. The default sixth anonymous attempt for one IP and sixty-first account attempt inside 60 seconds return 429 with an accurate positive `Retry-After`. Admins receive the same account limit. Unrelated account/IP buckets remain independent.
17. Invalid destinations, anonymous custom-code requests, and occupied custom codes consume eligible attempts. Requests rejected during authentication or the initial permission-to-create check, and rate-rejected attempts, do not. A subsequent creation rollback does not erase an already committed charge.
18. Concurrent requests cannot overspend an allowance. Expiry at the rolling boundary permits the next attempt. Restart and key rotation preserve the current allowance.
19. An untrusted peer's spoofed forwarding header has no effect. Trusted chains resolve from the right, mixed trusted/untrusted hops resolve correctly, malformed/duplicated chains fall back to the peer, and IPv4-mapped addresses share their IPv4 bucket.
20. Restart preserves links, accounts, secrets, visits/counters, and deleted-code reservations without any durable server-local state.

### Analytics and failures

21. GET and HEAD redirects, including bots/repeats, each eventually create one visit when the analytics queue, worker, and storage are healthy and within capacity. Failed redirects and management requests create none.
22. Full valid referrer/user-agent values and the effective raw IP are stored and visible only to authorized link readers. Missing/duplicated/invalid/oversized optional headers on transport-accepted requests produce null/unknown fields without affecting redirects.
23. Raw visits paginate deterministically; UTC day/group breakdowns and inclusive/exclusive date boundaries produce correct counts. Lifetime totals ignore filters. Null buckets and empty results follow the contract.
24. Internal retries of the same event UUID do not double count. Visit insertion and counter increment commit together or neither commits.
25. Saturate the analytics queue and inject analytics parsing/write/worker failures after successful link resolution: the redirect still succeeds, observable failures are recorded where possible, and counts never claim unpersisted records.
26. Delete a link with visits still queued: existing records disappear, late events cannot recreate them, and the code remains unavailable.
27. Database failure during lookup, authentication, mutation, reports, or rate checking produces 503 rather than fabricated 404/401/empty results or unaccounted creation.
28. Concurrent credential rotation/account suspension cannot authorize a later mutation with stale state; already-resolved redirects obey the defined concurrency boundary.

### Observability

29. With a collector configured, emit correlated traces/logs and operational metrics for successful and failed requests and analytics processing. Root sampling configuration changes traces without suppressing metrics/error logs.
30. Inspect exported spans/logs/metrics, including automatic HTTP/SQL instrumentation and error paths: no credentials, full URLs, IPs, raw user agents, query strings, or excluded payloads appear. Metric labels remain bounded.
31. Collector outage/backpressure leaves API calls and redirects functional with bounded buffers/timeouts. Readiness depends on PostgreSQL/schema state, not the collector. Shutdown completes within configured bounds even if analytics/exporters cannot flush.

### Horizontal scaling

32. Create a link through instance A and read/redirect/manage it through B. Rotate credentials, edit destinations, suspend accounts, and delete links through one instance; subsequent checks through the other observe the committed changes. Paginate through A and continue its cursor through B with the same authorization.
33. Split concurrent creation attempts for the same account and effective anonymous IP across A/B. The combined traffic receives one shared allowance, with consistent 429 behavior; adding/restarting an instance does not increase or reset it.
34. Race code allocation, bootstrap, and last-enabled-admin changes across independent processes. Uniqueness and admin invariants hold globally. Concurrent startup/migration attempts cannot race incompatible schema changes or expose an uninitialized instance as ready.
35. Persist visits concurrently from A/B and verify no lost increments or duplicate event counting. Delete a link through A while B has its visits queued; B cannot recreate the deleted data or release its code reservation.
36. Drain/remove one instance and add a fresh instance against the same database. Remaining/new instances can serve existing links, keys, tokens, and reports without state transfer or session affinity. The draining instance reports unready; any lost buffered visits remain within the documented best-effort behavior.

### Routing boundary

37. The Rust server itself serves no SPA or asset files and returns 404 at `/`. With the deployment proxy and a separate static fixture, `/` and `/assets/example.js` are handled by the static host while API calls and short-link requests reach ferl. Verifying this contract does not require building the separate SPA.
38. `/api`, `/api/`, and unknown API subpaths return API 404 responses without redirect lookup, visit recording, or SPA fallback. `/api/v1` remains the business API prefix. `/api/health/live` and `/api/health/ready` follow their unauthenticated health contract even if an invalid credential is supplied.
39. Available custom codes `assets`, `health`, and `api-sale` can be created and followed, while `api` is rejected as reserved. `/assets` is not rewritten to `/assets/`; `/assets/missing.js` is a static-file 404 and cannot resolve the `assets` short link.
40. Unknown short codes and unsupported multi-segment paths return 404 rather than the SPA entry point. A known code followed by an extra segment is not a redirect alias. Client-side fragments do not participate in server routing.

### Client-operation support and titles

41. Create links with a supplied title, omitted title, and explicit null title; creation/detail/list responses return the stored string or null consistently. Verify Unicode length bounds, whitespace trimming, blank/control-character rejection, authorized edits, null clearing, and omission preserving the existing title. Destination changes alone preserve the title, and no destination fetching occurs.
42. Users and admins receive only their own links from the default list and `scope=own`. Admin `scope=all` includes other accounts' and anonymous links; a user requesting it receives 403. Unknown scope values return 400. Pagination retains and authorizes the selected scope on every page.
43. List items contain all five requested display fields without additional detail/report calls. Rotating an API key preserves the account's listing; restart and requests through another instance preserve titles and listing scope behavior. Deleted links disappear from listings, and empty results use the collection envelope.
44. Create two independent short links with the same destination. Delete one using its returned short code; it and its visits disappear while the other link remains usable and listed. Verify this create/list/delete flow using only server API requests, without implementing the future CLI.

### Search, tags, historical destinations, and OpenAPI

45. Create/edit tagged account-owned and anonymous links. Verify case folding, Unicode normalization, whitespace trimming, canonical order, deduplication, input/name limits, omitted tags preserving state, `[]` clearing, and rejection of null/invalid values. A metadata update with any invalid field changes nothing.
46. Discover tags under own/admin-all/owner-filtered scopes. Verify distinct-link counts, account isolation even for shared names, anonymous-link visibility for admins, and immediate disappearance after the last visible association is removed. A management token cannot list tags globally.
47. Combine literal search, enabled flag, creation-date bounds, owner, and any/all tag filters. Verify AND/OR rules, mixed known/unknown tags, canonical matching, literal wildcard characters, and no duplicate links from multiple matching tags. Suspended-owner links retain the documented enabled/active distinction.
48. Validate forbidden owner filters, malformed/nonexistent owner IDs, invalid time ranges, unknown sort/mode values, repeated scalar parameters, and tag mode without tags. No error or invalid filter may broaden the caller's authorized scope.
49. With unchanged data, paginate every supported sort/direction through ties and null titles without omissions/duplicates, including across server instances and after deleting the last item of the previous page. Changed filter/sort context rejects the cursor; mutable ordering follows the documented live-pagination behavior.
50. Resolve a redirect to destination A, edit the link to B before its analytics event is written, and verify the recorded `redirect_url` is A and exactly matches the issued `Location`. A later redirect records B. Retries preserve each snapshot; deletion removes both records, and telemetry never exports either URL.
51. Validate `spec/openapi.yaml`, resolve all references, and check representative valid/invalid requests and responses. Verify endpoint coverage, optional anonymous creation versus required bearer credentials elsewhere, nullable titles, tag arrays, visit snapshots, pagination/filter parameters, errors/headers, health bodies, and bodyless HEAD responses without implementing a UI or CLI.
