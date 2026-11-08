# ferl CLI: API specification and business-analysis handoff

## 1. Purpose and confirmed product decisions

Prepare a specification and implementation plan for a **separate ferl CLI
project** that consumes the existing ferl server HTTP API.

The owner confirmed these first-release decisions:

| Area | Confirmed direction |
| --- | --- |
| Scope | Minimal first release: configure a server/API key, shorten links, list/search links, and delete links. |
| Configuration | Multiple named server/account profiles, with a default profile and explicit profile selection. |
| Platforms | Linux and macOS, each on ARM64 and x86_64. |
| Distribution | Downloadable native binaries; a Homebrew tap for Mac users. |
| Repository | A separate client project, consuming the server API. |

The existing server business specification anticipates a Rust CLI. Use Rust as
the planning baseline; select and justify the client's own toolchain and MSRV.
The server's pinned compiler is not a requirement for client users.

Link editing, dedicated tag commands, per-link analytics commands, account
administration, server-side credential rotation, and anonymous-token workflows
are **deferred from the first CLI release**. Their API details below are future
reference, not a requirement to implement them now. Replacing an externally
rotated API key in a local profile still needs to be possible.

Custom-code/title/tag options within shortening, advanced listing filters, and
output-column selection are API-supported refinements for the analyst to
propose and have confirmed within the minimal scope. Do not silently expand the
first release to full API coverage.

## 2. Authoritative handover files

Give the analyst all three files, retaining their relative paths:

1. **This document** — confirmed client scope, API summary, client implications,
   and decisions the analyst must resolve.
2. **[spec/openapi.yaml](../spec/openapi.yaml)** — normative OpenAPI 3.1.0
   contract: paths, operation IDs, security, request/response schemas, statuses,
   headers, parameter serialization, and examples.
3. **[spec/server-business-logic.md](../spec/server-business-logic.md)** —
   normative authorization, validation, rate accounting, persistence, concurrency,
   and pagination semantics that schemas cannot fully express.

Both normative documents must agree. This summary does not replace or modify
them. Report an actual conflict with exact references instead of weakening a
contract or changing the backend to fit a proposed client.

Reference snapshot:

| Item | Value |
| --- | --- |
| Server repository revision | `833feedccb3755f9641617673b3471c2fe871f1d` |
| OpenAPI `info.version` | `1.0.0` |
| Business API prefix | `/api/v1` |
| OpenAPI SHA-256 | `566e467b0e8fbc51e4492b276c8b44d2a03cfa1a0d854833b9d3380961a78860` |
| Business specification SHA-256 | `abe90313a6fedd1e8bbd912672c66c9978ed76b38037a638f510d86af591a6cd` |

The server package version, API version, and future CLI version are distinct.
Record the imported contract revision in the client project and review contract
updates deliberately. The normative files are self-contained: OpenAPI uses
local references and does not require a live documentation endpoint.

Optional background in this server repository:
[configuration](configuration.md), [release/operator guide](release.md),
[development and fixtures](development.md), and
[verification evidence](release-verification.md).

## 3. Connection and HTTP conventions

For a deployment with origin `https://s.example.com`:

```text
Account API:    https://s.example.com/api/v1/me
Link API:       https://s.example.com/api/v1/links
Public link:   https://s.example.com/my-docs
Health:        https://s.example.com/api/health/ready
```

- There is no universal hosted server. Each profile selects a self-hosted
  deployment and an account API key.
- The public origin has no path prefix. The analyst should define whether a
  profile stores an origin or an API URL; recommend storing an origin and
  constructing the fixed API paths to avoid doubled `/api/v1` prefixes.
- Return/display the server's `short_url`. It is built from the server's
  configured public origin, which can differ from the address used to reach its
  API. Do not reconstruct it from the profile URL.
- Use JSON for API bodies, `Content-Type: application/json` when sending JSON,
  and `Accept: application/json` for API requests.
- The request body limit is **65,536 bytes**. Unknown request fields, unknown
  query parameters, duplicate JSON fields, and repeated scalar query parameters
  are rejected. Send only operation-specific parameters.
- GET requests and DELETE requests in this contract take no body. Credential
  replacement POSTs also take no body: `{}`, JSON `null`, and whitespace are
  bodies, not an omission.
- Every application response includes a fresh server-generated `X-Request-ID`;
  API and redirect responses use `Cache-Control: no-store`. Error-body
  `request_id` matches the response header.
- `204` and successful public `302` responses have no body. All HEAD responses
  have no body, including errors. Only the public short-link route documents
  HEAD; API GET does not imply HEAD support.
- Unsupported methods return `405` with `Allow`; unsupported paths return `404`.
  The backend root `/` is `404`, not a version or connectivity endpoint.
- A proxy can return HTML or an empty failure response. Client error handling
  must also handle non-JSON HTTP failures, malformed JSON, TLS failures, DNS
  failures, connection errors, and timeouts.

**Client design recommendations:** verify TLS certificates and hostnames;
support private-CA deployments through an explicit trust configuration. Define
local HTTP development behavior separately. Do not automatically follow API
redirects or send a profile's credential to a destination URL or another
profile's server. Do not require a health probe before every business request;
handle the actual operation's response.

## 4. Authentication and authority

```http
Authorization: Bearer <account-api-key>
```

Supply exactly one Authorization value. Credentials are not query parameters or
JSON fields. There is no password login, OAuth flow, session cookie, refresh
token, public signup, or API-key lookup endpoint.

The first CLI release uses an **existing account API key**, obtained from a
server administrator. An optional profile-validation step can call
`GET /api/v1/me`; this is a read-only API capability, not a requirement to add a
separate account command family. A successful health request does not validate
credentials because health routes ignore them.

| Credential | Authority |
| --- | --- |
| Enabled user account key | Create account-owned links; list own links; manage own links. |
| Enabled admin account key | Same own-list default; explicit all-scope listing and access to other accounts' and anonymous links; account administration. |
| Anonymous management token (deferred) | Read/manage/report on precisely one anonymous link; cannot list links/tags, create links, or access account operations. |
| No credential (deferred for CLI creation) | Create generated-code anonymous links only if the deployment enables it. |

Important distinctions:

- Missing credentials on protected operations: `401 authentication_required`.
- Malformed, unknown, revoked, or disabled-account credentials:
  `401 invalid_credentials`. A `401` includes `WWW-Authenticate: Bearer`.
- Valid credential with the wrong operation scope: `403 forbidden`.
- Unknown link and a link outside the caller's object authority both return
  `404 not_found`. Do not claim that `404` proves global nonexistence.
- Invalid supplied credentials never fall back to anonymous creation. The CLI
  must not retry without authentication after `401` or `403`.
- Key rotation replaces the one active key immediately. Ownership belongs to
  the account, so an externally rotated replacement key still accesses its links.
- Treat secrets as opaque. Current issued prefixes are `ferl_ak_v1_` for account
  keys and `ferl_mt_v1_` for management tokens; they are not JWTs to decode.
  Profile validation should rely on the server rather than infer validity from
  a prefix alone.

The analyst must specify profile storage, credential entry and replacement,
configuration precedence, redaction, and noninteractive use. Profiles and
credential storage are local client behavior; there are no server profile APIs.

## 5. First-release workflow: shorten

**Operation:** `createLink` — `POST /api/v1/links`.

Request schema: `CreateLink`. Success: **201**, schema `LinkCreation`.
Declared errors: `400`, `401`, `403`, `409`, `413`, `415`, `429`, `503`, `500`.
Global method/path errors also apply.

Minimal request:

```json
{"destination_url":"https://example.com/docs?edition=2#intro"}
```

Available request fields:

| Field | Presence and rules |
| --- | --- |
| `destination_url` | Required non-null string; absolute HTTP(S) URL with host and valid port, no embedded user information or surrounding whitespace. Both input and serialized form must fit 8,192 UTF-8 bytes. |
| `short_code` | Optional non-null string for account callers; exact `[a-z0-9-]{3,64}`. Omit for a server-generated code of eight lowercase letters/digits. |
| `title` | Optional string or `null`; omission/null means no title. Trim Unicode whitespace; a string must then have 1–200 Unicode scalar values and no control characters. Empty/blank is invalid. |
| `tags` | Optional array of strings; omission becomes `[]`; `null` is invalid. At most 20 input values before deduplication. |

Tag rules: each input has at most 256 Unicode scalar values. The server trims,
full-case-folds, NFC-normalizes, deduplicates, and sorts canonical names by UTF-8
bytes. Each canonical name is 1–64 scalar values without control characters.
`Work`, `work`, and ` work ` become one `work` tag. Commas can be literal data.
Do not substitute ASCII lowercasing for server normalization.

The server accepts private, localhost, IPv6, internal, and internationalized
HTTP(S) destinations under the same syntax rules. It does not fetch them or
discover titles. Send destination query/fragment data intact; do not strip
tracking parameters, trim invalid URL input into validity, or perform a
reachability preflight as a prerequisite for shortening.

Illustrative account-owned response:

```json
{
  "link": {
    "id": "1f1bf5f6-075c-4cf1-a44b-05fca986b100",
    "short_code": "my-docs",
    "short_url": "https://s.example.com/my-docs",
    "destination_url": "https://example.com/docs?edition=2#intro",
    "title": null,
    "tags": [],
    "owner_id": "4a8ffcd5-3066-4519-a68e-060f956ab500",
    "enabled": true,
    "active": true,
    "visit_count": 0,
    "created_at": "2026-10-29T10:00:00.000000Z",
    "updated_at": "2026-10-29T10:00:00.000000Z"
  }
}
```

Account-owned creation **omits** `management_token`; anonymous creation instead
requires a one-time `management_token` alongside `link` and has `owner_id: null`.
It is not represented as `management_token: null` for an account-owned link.

Creation facts affecting the CLI:

- Duplicate destinations create independent links. No deduplication or
  idempotency-key API exists.
- `api` and all previously allocated/deleted codes are unavailable;
  `409 short_code_unavailable` is the authoritative conflict result. Bare
  `assets`, `health`, and `api-sale` can be valid codes.
- Authentication and creator eligibility precede a durable allowance charge;
  body validation and allocation happen afterward. Even invalid eligible
  attempts and code conflicts consume allowance.
- Account creation allowance defaults to **60 attempts per rolling 60 seconds**,
  including admins. Deployment settings can change the limit. It is per account,
  not per key, profile, CLI process, or server instance.
- `429 rate_limited` includes a positive whole-second `Retry-After`. That
  rejected attempt adds no charge. Report the wait; automatic retry policy is a
  client decision and must respect uncertain mutation outcomes (section 9).

## 6. First-release workflow: list and search

**Operation:** `listLinks` — `GET /api/v1/links`.

Success: **200**, schema `LinkCollection`. Requires an enabled account key.
Declared errors: `400`, `401`, `403`, `503`, `500`.

```json
{"items":[],"next_cursor":null}
```

Each nonempty `items` entry is the complete `Link` object shown above. All 12
fields are required, including nullable `title` and `owner_id`. Counts are JSON
integers in `0..=9223372036854775807`; preserve exact integer precision.
Timestamps are RFC 3339 UTC strings with `Z`; the current server emits
microsecond precision. `enabled` is the link's own setting; `active` also
reflects whether its owner is enabled. Do not treat these fields as synonyms.

The API can directly supply list columns for `short_url`, `destination_url`,
`title`, `visit_count`, and `created_at`; no per-link request is needed. Column
selection and human date formatting are client responsibilities. There is no
server `columns` or field-projection query parameter.

### Query parameters

| Parameter | Allowed values and semantics |
| --- | --- |
| `q` | Literal case-insensitive substring of destination, short code, or title. Trim; 1–256 Unicode scalar values, no controls; server uses full case folding and NFC. `%`, `_`, backslashes, and commas are data, not search operators. |
| `limit` | Integer 1–200; default 50. |
| `cursor` | Nonempty opaque server-issued continuation; omit on the first page. |
| `scope` | `own` (default for users **and admins**) or `all` (admins only). |
| `owner_id` | Account UUID or literal `anonymous`; admin with `scope=all` only. A non-admin supplying it gets 403; an admin without all-scope gets 400. |
| `enabled` | `true` or `false`; filters the link flag, not computed active status. |
| `created_from` | Inclusive RFC 3339 creation timestamp; explicit offsets accepted. |
| `created_to` | Exclusive RFC 3339 creation timestamp; if both bounds exist, require `created_from < created_to`. |
| `tag` | Repeat the parameter for 1–20 input tags: `tag=work&tag=rust`. A comma inside one value is literal, not a separator. |
| `tag_mode` | `any` (default when filtering by tags) or `all`; invalid without `tag`. |
| `sort` | `created_at` (default), `title`, or `visit_count`. |
| `order` | `asc` or `desc`; default `desc` for every sort field. |

Only `tag` may repeat. Use a query encoder, including for `+` in timestamp
offsets, `&` in search text, Unicode, and cursor values.

Separate filter categories combine with AND; q's three fields combine with OR;
tag any/all matching applies within the tag filter. Unknown tags match no
association. Listings include individually disabled links; all-scope can include
links with suspended owners. Deleted links are absent.

### Pagination

1. Request the first page with the selected profile, filters, sort, and limit.
2. Read `items` and `next_cursor`.
3. If the cursor is a string, request the same operation with the same profile,
   filters, scope, and ordering plus that cursor. Page size may change.
4. Terminate on explicit `next_cursor: null`; do not infer completion from a
   short page or merely from an empty `items` array.

There is no offset/page-number API, total-page count, or server-side pagination
session. Never decode, synthesize, truncate, or treat a cursor as authentication.
Cursors are tied to resource, identity, filters, and ordering; authority is
checked on every page. Current server cursor capacity is **87,387 ASCII bytes**;
do not assume a 2–8 KiB URL/argument buffer will suffice for all API families.

Default link order is creation time descending with UUID descending as the final
tie-breaker. Each supported sort uses UUID in the selected direction. Title
ordering is case-folded/NFC byte order, with null titles **last in both directions**.
Preserve server ordering when consuming pages. Pagination is live, not a frozen
snapshot; concurrent changes can move results. The analyst must define default
page display, explicit continuation, optional fetch-all behavior, and reporting
of partial output if a later page fails.

## 7. First-release workflow: delete

**Operation:** `deleteLink` — `DELETE /api/v1/links/{code}`.

Send the account bearer key and **no request body**. Success is **204** with no
JSON to deserialize. Declared errors: `400`, `401`, `403`, `404`, `503`, `500`.

- The path identifies a **short code**, not a link UUID or destination URL.
- A user can delete own links; an admin can delete any link. Inaccessible and
  unknown links return the same `404 not_found`.
- The deletion removes that link, tags, visits, and any anonymous token. It
  permanently retains the code reservation; there is no undelete or code reuse.
- Other links to the same destination remain intact.
- Repeating DELETE with a valid account key returns 404. A repeated request with
  the deleted anonymous link's token instead returns 401 because it was revoked.

The existing business examples suggest `ferl delete <short-url>`. Parsing that
argument into `{code}` is client work. Specify handling of bare codes, origin
mismatches, query/fragment suffixes, encoded spellings, and trailing/extra path
segments. Do not lowercase or percent-decode an invalid alias into a valid code.
Do not follow a public redirect to discover which link to delete. Resolve
credentials from the selected profile, never from an arbitrary input URL.
If public and API origins differ, define an explicit profile association rather
than silently switching authenticated request hosts.

## 8. Errors and request precedence

Application error schema: `Error`.

```json
{
  "error": {
    "code": "short_code_unavailable",
    "message": "The requested short code is unavailable."
  },
  "request_id": "72ad8f48-40e4-4a78-9bac-3cd81aba6000"
}
```

| HTTP status | Stable codes / meaning | Relevant header |
| --- | --- | --- |
| 400 | `invalid_request` | — |
| 401 | `authentication_required`, `invalid_credentials` | `WWW-Authenticate: Bearer` |
| 403 | `forbidden`, `anonymous_creation_disabled` | — |
| 404 | `not_found` (also conceals inaccessible objects) | — |
| 405 | `method_not_allowed` | `Allow` |
| 409 | `short_code_unavailable`, `username_unavailable`, `last_admin_required`, `not_anonymous_link` | — |
| 413 | `request_too_large` | — |
| 415 | `unsupported_media_type` | — |
| 429 | `rate_limited` | Positive whole-second `Retry-After` |
| 503 | `storage_unavailable`, `service_unavailable`, `code_generation_unavailable` | No general Retry-After guarantee |
| 500 | `internal_error` | — |

This is the shared error vocabulary, not a claim that every operation declares
every status. Use the per-operation OpenAPI response map for exact coverage.
Retain HTTP status, stable code, safe message, and request ID for useful CLI
errors; design a fallback for new codes and intermediary responses. A database
failure is not an empty list, an invalid credential, or a missing link.

Route/method selection comes first. Authentication and operation/object authority
precede operation-specific validation. Creation then charges allowance before
body/feature validation and commits creation separately. Examples: an invalid
credential with invalid input can return 401 before a field error; malformed
filters do not bypass object concealment; an anonymous `short_code` member,
even null, is forbidden after charging rather than an ordinary field error.

Client validation can improve feedback before a request is sent, but it cannot
establish authorization, availability, or uniqueness. Server responses remain
authoritative.

## 9. Mutation outcomes and retries

Acknowledged successful mutations have committed. A timeout, connection loss, or
some storage failures can occur **after commit**. Without a success response,
the client may not know whether the mutation happened.

Recommendations for the analyst to turn into explicit client requirements:

- Do not transparently replay a create after an uncertain outcome. It can create
  another link and incur another allowance charge. There is no idempotency key
  or request-ID lookup endpoint for reconciliation.
- A custom code can help a user inspect current state, but a conflict on retry
  is not proof the first request succeeded or that the caller owns that link.
- Do not convert a repeated DELETE's 404 into proof that the earlier deletion
  succeeded. Report the observed result and distinguish a prior uncertain request.
- Read-only business GETs can use a bounded retry policy if specified; retain
  the same selected profile/query. Public GET/HEAD redirects produce visits and
  are not side-effect-free metadata probes.
- Report 429 and its wait. If opt-in retry is supported, bound attempts and total
  elapsed time; distinguish a definitive rate rejection from an uncertain write.
- Future key/token rotation needs special handling: successful rotation revokes
  the old credential, and the new plaintext appears once. A lost response or
  failed local save cannot be solved by assuming the old credential still works.

CLI exit codes, stdout/stderr behavior, timeout defaults, and retries are not
specified by the server; they must be designed and tested in the client project.

## 10. Full API inventory (reference, not first-release scope)

There are **22 documented operations**. Schema names below refer to
`components.schemas`; `Page<T>` means `{items: T[], next_cursor: string|null}`.
“Link authority” means owner account, admin, or matching anonymous token.

| Method | Path | operationId | Authority | Request body → success |
| --- | --- | --- | --- | --- |
| GET | `/api/v1/me` | `getCurrentAccount` | Account | None → 200 `Account` |
| POST | `/api/v1/me/api-key` | `rotateCurrentApiKey` | Account | None → 200 `ApiKeyReplacement` |
| GET | `/api/v1/admin/accounts` | `listAccounts` | Admin | None → 200 `Page<Account>` |
| POST | `/api/v1/admin/accounts` | `createAccount` | Admin | `CreateAccount` → 201 `AccountCreation` |
| GET | `/api/v1/admin/accounts/{id}` | `getAccount` | Admin | None → 200 `Account` |
| PATCH | `/api/v1/admin/accounts/{id}` | `updateAccount` | Admin | `UpdateAccount` → 200 `Account` |
| DELETE | `/api/v1/admin/accounts/{id}` | `deleteAccount` | Admin | None → 204, empty |
| POST | `/api/v1/admin/accounts/{id}/api-key` | `resetAccountApiKey` | Admin | None → 200 `ApiKeyReplacement` |
| GET | `/api/v1/links` | `listLinks` | Account | None → 200 `Page<Link>` |
| POST | `/api/v1/links` | `createLink` | Eligible creator | `CreateLink` → 201 `LinkCreation` |
| GET | `/api/v1/links/{code}` | `getLink` | Link authority | None → 200 `Link` |
| PATCH | `/api/v1/links/{code}` | `updateLink` | Link authority | `UpdateLink` → 200 `Link` |
| DELETE | `/api/v1/links/{code}` | `deleteLink` | Link authority | None → 204, empty |
| POST | `/api/v1/links/{code}/management-token` | `rotateManagementToken` | Matching token or admin | None → 200 `ManagementTokenReplacement` |
| GET | `/api/v1/links/{code}/visits` | `listLinkVisits` | Link authority | None → 200 `Page<Visit>` |
| GET | `/api/v1/links/{code}/stats` | `getLinkVisitStats` | Link authority | None → 200 `VisitStats` |
| GET | `/api/v1/links/{code}/stats/breakdown` | `getLinkVisitBreakdown` | Link authority | None → 200 `Page<BreakdownItem>` |
| GET | `/api/v1/tags` | `listTags` | Account | None → 200 `Page<TagInfo>` |
| GET | `/api/health/live` | `getLiveness` | Public; credentials ignored | None → 200 `Healthy` |
| GET | `/api/health/ready` | `getReadiness` | Public; credentials ignored | None → 200 `Healthy`; unready 503 `Unready` |
| GET | `/{short_code}` | `followShortLink` | Public; credentials ignored | None → 302, exact `Location`, empty |
| HEAD | `/{short_code}` | `followShortLinkHead` | Public; credentials ignored | None → 302, exact `Location`, empty |

### Additional request and response shapes

- `Account`: required `id` (UUID), `username` (string), `role` (`admin|user`),
  `enabled` (boolean), `created_at`, and `updated_at` (UTC timestamps). No key or digest.
- `CreateAccount`: required `username` matching `[a-z0-9_-]{3,64}`; optional
  non-null `role` (`admin|user`, default `user`). New accounts are enabled.
- `UpdateAccount`: at least one of non-null `role` and `enabled`. Username is immutable.
- `AccountCreation`: required `account: Account` and one-time `api_key: string`.
- `ApiKeyReplacement`: required one-time `api_key: string`.
- `ManagementTokenReplacement`: required one-time `management_token: string`.
- `UpdateLink`: at least one of `destination_url`, `title`, `tags`, `enabled`.
  Omitted fields remain unchanged. `title: null` clears the title; `tags: []`
  clears tags; null tags/destination/enabled and an empty PATCH object are invalid.
  Tags replace the complete set. Changes are atomic; no-op metadata preserves
  `updated_at`. No writable owner/code/ID/visit count exists.
- `Visit`: required `id`, `link_id` (UUIDs), `short_code`, `redirect_url`,
  `occurred_at`, `method` (`GET|HEAD`), `client_ip` (canonical bare IPv4/IPv6),
  `referrer` and `user_agent` (each string or null), `device`
  (`desktop|mobile|tablet|other|unknown`), and nonempty `browser` and `os` strings.
  `redirect_url` is the immutable historical destination, not today's link URL.
- `VisitStats`: required integer `lifetime_visits` and `matching_visits`, each
  in `0..=9223372036854775807`.
- `BreakdownItem`: required `value` (string or null) and `visits` (Count).
  Null, empty string, and the string `unknown` are distinct buckets.
- `TagInfo`: required `name` (canonical tag) and `link_count` (positive Count).
- `Healthy`: `{"status":"ok"}`. `Unready`: `{"status":"unavailable"}`.
  Admission overload can instead return a structured `service_unavailable`
  error on readiness; inspect the status/body rather than infer a database
  failure from every readiness 503. Liveness is isolated from business admission
  and retains its normal response or method/query rejection under overload.

Account list accepts only `limit` and `cursor`, ordered creation time/UUID
descending. Tag discovery accepts only `scope`, `owner_id`, `q`, `limit`,
`cursor`, with the same scope/owner rules as link listing; q searches tag names
only. Tags sort ascending by canonical name and count distinct visible live
links across the entire scope, not just one page.

Visits accept `from`, `to`, `limit`, `cursor`; stats accept only `from`, `to`.
Breakdowns require `group_by` and accept `from`, `to`, `limit`, `cursor`.
Grouping is one of `day`, `method`, `referrer`, `client_ip`, `user_agent`,
`device`, `browser`, `os`. Times use inclusive `from`, exclusive `to`; offsets
are accepted, and both bounds require `from < to`. Raw visits sort occurrence
time/UUID descending. Breakdown values sort ascending with null first; UTC day
values use `YYYY-MM-DD`. Counts cover the complete date filter, not just a page;
lifetime count ignores it. Analytics is eventually consistent and best effort.

Public redirects preserve the stored destination's query and fragment and ignore
the incoming short URL's query. They reject percent-encoded aliases, uppercase
spellings, and trailing/extra path segments. Missing, deleted, disabled, and
suspended-owner links are indistinguishable 404s without Location. Both GET and
HEAD can create visits; do not use them to populate a management list.

The last enabled admin cannot be disabled, demoted, or deleted. Account deletion
cascades its links and visits but retains code reservations. Disabling an account
invalidates authentication and suspends its redirects without changing each
link's enabled flag. Token rotation on an account-owned link returns
`409 not_anonymous_link`.

Server migration and initial `bootstrap-admin` are operator commands, **not
HTTP endpoints**. The CLI needs neither PostgreSQL access nor the server cursor
encryption key. There are no bulk, delete-by-destination, destination-preview,
standalone tag-write, global analytics, or remote OpenAPI-serving operations.

## 11. CLI and Homebrew planning decisions

These are client/distribution design questions, not gaps in the completed backend.
The analyst should propose concrete defaults and ask the owner to approve
material choices.

### Client experience

1. **Command grammar:** existing server examples suggest `ferl <url>`,
   `ferl list`, and `ferl delete <short-url>`. Confirm explicit `create` aliases,
   profile/config commands, help/version, and argument disambiguation. Decide
   whether custom code, title, tags, and advanced filters belong in release one.
2. **Profiles:** file location/format, create/select/list/remove behavior, default
   profile, explicit selection, and configuration precedence (flags/environment/
   saved profile). Bind server, account credential, optional public origin, and
   CA settings together; never accidentally mix entries across profiles.
3. **Credentials:** secure interactive entry and a noninteractive automation
   path; macOS Keychain/Linux secret-service versus private-file storage;
   behavior when a key store is unavailable; replacement of externally rotated
   keys; redacted profile output and diagnostics. Do not invent a login endpoint.
4. **Output:** useful terminal tables and a stable machine-readable mode;
   whether shortening prints only `short_url`; null titles and long URL display;
   column selection; timezone; no-color/non-TTY behavior; stdout/stderr and
   exit-code contracts. Render untrusted text as data, not terminal controls.
5. **Pagination:** page vs fetch-all default, explicit continuation, bounds and
   cancellation, and partial-output reporting. Do not silently truncate a list
   while representing it as complete.
6. **Deletion:** accepted code/URL forms, profile/origin checks, confirmation
   behavior in terminals and scripts, and honest handling of 404/uncertain writes.
7. **Transport:** TLS/private roots, local HTTP policy, proxy environment support,
   connection/request timeouts, bounded read retries, and mutation retry policy.

### Native distribution and Homebrew

Plan four native executable targets:

| OS | Architecture | Rust target planning baseline |
| --- | --- | --- |
| macOS | ARM64 / Apple Silicon | `aarch64-apple-darwin` |
| macOS | x86_64 / Intel | `x86_64-apple-darwin` |
| Linux | ARM64 | `aarch64-unknown-linux-gnu` or a deliberately selected musl target |
| Linux | x86_64 | `x86_64-unknown-linux-gnu` or a deliberately selected musl target |

The OS/CPU matrix is confirmed; minimum macOS versions, Linux libc/runtime
baseline, and gnu-versus-musl are still decisions. A compiled cross-target binary
alone is not native runtime verification. Client platform acceptance is separate
from the server's existing verification results.

The Homebrew tap is a separate Git repository containing the **client formula**;
Homebrew installs the `ferl` executable on the user's Mac. A registry-hosted
Docker image for `ferl-server` is a different deliverable.

For the owner's prebuilt-binary direction, propose a formula selecting the
appropriate immutable macOS release archive by architecture, checking its
SHA-256, and installing `ferl` into `bin`. A prebuilt upstream archive is not
automatically a Homebrew-built bottle; choose the packaging approach explicitly.
Plan basic formula installation and `ferl --version`/help tests without requiring
credentials or a running server, plus native end-to-end client tests separately.

Ask for the CLI repository name/owner, tap owner/name, formula name, release
artifact location, version/tag strategy, license, signing/notarization policy,
and authorized CI/publishing method. GitHub Releases is a possible artifact
store, not an existing approved URL. Do not invent usable `brew install` or
download commands for repositories/artifacts that do not yet exist. Linux
downloadable binaries are required; Linuxbrew support is not yet a commitment.

## 12. Acceptance cases to include in the client plan

Translate these into observable CLI requirements and tests, with output and exit
codes decided by the analyst:

1. Select two named profiles on different servers/accounts; each request uses
   exactly its selected URL/key/CA settings; listing profiles reveals no secrets.
2. A valid existing account key works; invalid/revoked keys surface 401 without
   fallback to anonymous creation or another profile.
3. Shorten a URL containing query and fragment; display the returned short URL
   intact. Shortening the same destination twice creates independent links.
4. Where selected in scope, creation serializes optional fields correctly and
   handles custom-code conflict, title nullability, and tag-array validation.
5. Listing uses `scope=own` by default even with an admin key; q is encoded as
   literal text and results retain server ordering and required null fields.
6. Multi-page listing follows opaque cursors through explicit null termination;
   bad cursors and later-page errors never become silent success. Exercise
   large legal cursor values without truncation and counts beyond 2^53.
7. Delete addresses the selected short code with an empty request body and
   handles empty 204. Another link to the same destination survives; a later
   create with the deleted code gets a conflict.
8. Handle 400/401/403/404/409/413/415/429/500/503, Retry-After and request IDs,
   non-JSON intermediary failures, invalid JSON, TLS failure, and network timeout.
9. A deliberately dropped create response does not cause an automatic second
   create. The CLI reports uncertainty rather than claiming no mutation occurred.
10. Invalid input, destination URLs, and redirects cannot redirect an authenticated
    management request to an arbitrary host; credentials stay out of diagnostics.
11. Scripts get documented stdout/stderr/exit behavior without blocking on an
    unexpected prompt; terminal output handles Unicode, null titles, long URLs,
    and embedded escape/control sequences as data.
12. Real CLI binaries run against an isolated real server on all four native
    OS/CPU targets. Mac users can install the intended formula on both Mac
    architectures and upgrade while preserving their local profiles.

Use HTTP fixtures for deterministic serialization/error tests and a real ferl
server with synthetic accounts/data for integration. The client must not depend
on the server's Rust internals; the pinned HTTP contract is the integration
boundary. Feature-specific future tests belong with their deferred command
families.

## 13. Prompt to give the business analyst AI agent

```text
You are the business analyst for a new, separate ferl CLI client project.
Read docs/cli-api-handoff.md, spec/openapi.yaml, and spec/server-business-logic.md.
The OpenAPI and business documents jointly define the existing server contract.

Confirmed release-one scope: configure named server/account profiles, shorten
links, list/search links, and delete links. Support Linux and macOS on ARM64 and
x86_64. Distribute native binaries and plan a Homebrew tap for Mac users.
Use Rust as the planning baseline identified by the existing product spec.

Produce a client business specification and phased implementation plan with:
- explicit release-one scope and deferred capabilities;
- command/flag grammar and concrete interactive/noninteractive workflows;
- profile/configuration/credential storage and precedence;
- exact mapping from commands to existing API operations and schemas;
- output schemas, stdout/stderr conventions, exit codes, pagination, and errors;
- URL handling, TLS, timeouts, retries, and uncertain mutation outcomes;
- native target/runtime matrix, binary packaging, and Homebrew formula plan;
- measurable acceptance criteria, test strategy, milestones, and prerequisites;
- a short decision log distinguishing owner decisions from your proposals.

Ask focused questions about unresolved client and distribution choices. Include
custom-code/title/tag options and advanced filters only after clarifying their
place in the minimal first release. Do not turn the full API inventory into a
commitment to implement every command family.

Do not modify the server contract, reopen completed backend implementation,
invent endpoints, require direct database access, or design server deployment
inside the CLI. The CLI does not need the server's cursor encryption key.
No remote repository, tap, hosted workflow, or publishing credential is yet
approved by this handoff; list those as release prerequisites, not completed work.
Deliver the specification and implementation plan before writing client code.
```
