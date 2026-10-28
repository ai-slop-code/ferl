# Part 03 domain and wire contracts

Part 03 supplies pure validated values, strict standalone request decoders,
record-to-DTO conversions, and a stateless cursor codec. It does not register
business API routes or implement authentication, bootstrap, account/link
services, reports, allocation retries, or database-backed pagination.

## Validated domain API

- `domain::url::DestinationUrl::parse` validates the raw UTF-8 byte limit
  (8,192), surrounding Unicode whitespace, ASCII whitespace/control bytes,
  explicit HTTP(S) `://` syntax, original authority/user-info/port syntax, and
  parser result before storing one serialized URL. It rechecks serialized size
  and `Location` header conversion. It performs no network access.
- `domain::url::PublicOrigin::parse` shares the authority protections but
  permits only an origin and optional root slash. It rejects user-info, a path
  prefix, query, and fragment. Configuration now delegates to this validator.
- `StoredUrl::from_database` verifies an already serialized stored/snapshot URL
  without rewriting the value. Link snapshots therefore remain historical.
- `Username::parse` and `ShortCode::parse_custom` accept only their exact ASCII
  grammars. Reservation conflicts (including `api`) remain a service/database
  decision. `ShortCode::generate` draws eight characters using OS randomness,
  rejecting byte values 252–255 before modulo 36; each character has a bounded
  128-draw test seam.
- Titles, tags, and search use ICU4X 2.4.0 compiled data, Unicode 18.0.0:
  `icu_casemap` performs full non-Turkic Unicode folding, `icu_normalizer`
  performs NFC, and `icu_properties` supplies White_Space and General_Category
  Control checks. Tag input has a 256-scalar per-value limit and 20-entry
  pre-dedup limit; canonical tags are sorted by UTF-8 bytes. Persisted
  destination/title comparison uses the same fold/NFC transform without the
  search-query trim/length limit. A Unicode data upgrade must not mix search
  keys across instances; it requires a coordinated persisted-value migration
  plan before deployment.
- `Timestamp` accepts RFC 3339 offsets and emits UTC `Z` with six fractional
  digits. Filter, cursor, DTO, and database instants are truncated to PostgreSQL
  `timestamptz` microsecond precision. `TimeInterval` enforces inclusive-lower,
  exclusive-upper range ordering. `Count` is checked against `0..=i64::MAX`;
  `PositiveCount` is used for tag link counts.

The public modules are intentionally split by validation responsibility:
`domain/{ids,count,url,short_code,text,time,secret}.rs`. Username/title/tag/search
currently share `domain/text.rs`; they remain distinct private-field types and
have checked constructors.

## Strict decoding and presence

`http::body::read_json_object` reads at most 65,536 actual streamed body bytes,
not a `Content-Length` or size hint. The read has a 30-second total deadline.
It accepts `application/json` and `application/*+json`, with no media
parameters or one UTF-8 `charset` parameter; other/duplicate parameters and
malformed/repeated media types are 415. Media type is checked
before body consumption, so an unsupported type wins over an oversized body.
With a supported type, the complete bounded read/size check precedes JSON
parsing, so an oversized body is 413 even if its prefix is malformed.
Stream/parser errors are sanitized to 400. Raw JSON objects reject duplicate
root member names, non-object roots, and trailing data before any DTO conversion.

Replacement POST/DELETE implementations use `http::body::require_bodyless`,
which reads actual body frames and rejects `{}`, whitespace, and bytes received
without `Content-Length`. No body extractor or eager middleware is installed in
the create pipeline; future services call these decoders only after eligibility
and durable charge.

`RawCreateLink` preserves raw `short_code` member presence. The anonymous feature
permission check precedes all field validation, so any present `short_code`
(including null or wrong-type) is 403 after charging. For account creators,
normal field type/unknown-field/validation errors are 400. `Presence<T>` carries
omitted/null/value where required: title permits all three on PATCH; tags permit
omitted/value but reject null; destination and enabled permit omitted/value but
reject null; account role defaults to `user` only on creation and rejects null.
Unknown/immutable fields and empty PATCH objects are rejected. Validation
constructs complete request aggregates without mutating caller state.

`http::query` preserves decoded raw pair multiplicity, validates percent
escapes and UTF-8 without lossy repair, rejects unknown names/repeated scalars,
and permits repetition only for link-list `tag`. It exposes concrete parsers
for account/link/tag/visit/stats/breakdown operations. Query syntax does not
authenticate; `authorize_scope` preserves the documented 403 for forbidden
scope/owner filters and 400 for invalid admin combinations. Redirect and health
routing do not apply API query parsing.

`http::test_support::decoder_router` is a test-only composition. It is not
referenced by `http::router` and its routes are absent from `ferl-server`.
`tests/http_decoding.rs` exercises it through the production request-ID,
no-store, safe-error, and HEAD response envelope.

## Credentials and redaction

Credential formats are intentionally distinct:

| Class | Prefix | Payload |
| --- | --- | --- |
| Account API key | `ferl_ak_v1_` | exactly 32 OS-random bytes, canonical unpadded base64url (43 chars) |
| Anonymous management token | `ferl_mt_v1_` | exactly 32 OS-random bytes, canonical unpadded base64url (43 chars) |

Prefixes/version are excluded from entropy but included in SHA-256. The complete
presented UTF-8 secret is hashed, and only the fixed 32-byte digest is suitable
for persistence. Credential parsing handles exactly one case-insensitive
`Bearer` Authorization value and returns missing, supplied-invalid, or parsed
redacted state. It performs no authentication/authorization lookup and never
falls back across credential classes. Randomness failure is a safe typed error;
there is no weak fallback. `OneTimeSecret` is serializable only for explicit
one-time result wrappers; ordinary DTOs and records contain no plaintext or
digest field.

## Cursor format and capacity

`FERL_CURSOR_KEY` remains the deployment-wide, canonical unpadded base64url
encoding of 32 key bytes. `CursorCodec` uses released `chacha20poly1305` 0.11.0
XChaCha20-Poly1305, a fresh OS-random 24-byte nonce, and a 16-byte Poly1305 tag.
The textual envelope is:

```text
fc1<resource-code>.<base64url-no-pad(nonce || ciphertext || tag)>
```

Resource codes are `a` accounts, `l` links, `t` tag discovery, `v` visit
records, and `b` breakdown. The complete `fc1<code>.` prefix is authenticated
associated data; the decrypted version and resource are checked again. The
encrypted JSON carries version 1, resource, SHA-256 of canonical request
context, and one typed boundary. Context uses deterministic struct-ordered JSON
with explicit null/absent fields and defaults, canonical sorted tags, UTC
microsecond times, actor account/anonymous-link UUID, target link, scope/owner,
filters, sort/order/grouping. Page size and cursor itself are excluded. No API
key, management token, credential digest, or configuration secret enters the
context/payload. Decoding validates the authenticated payload shape and does not
authorize access; services must reauthorize every page.

Typed boundaries are account created-at/UUID; link created-at, normalized
title/null, or visit-count/UUID (matching selected sort); tag name; visit
occurred-at/UUID; and breakdown grouping value/null (matching grouping kind).
Persisted IDs, counts, timestamp range, normalization, group-specific values,
string bounds, and nullability are checked after decryption. Decrypted data is
never exposed as arbitrary SQL input.

The binary nonce+ciphertext+tag envelope is capped at 65,536 bytes; encoded
tokens including prefix are capped at 87,387 ASCII bytes before decoding.
Plaintext is therefore at most 65,496 bytes. A legal 8,192-byte grouping value
may JSON-escape to 49,152 bytes (all control bytes) plus boundary/context
metadata, which fits. `tests/cursor_codec.rs` exercises maximum-size mixed
quote/backslash/control values; codec capacity does not claim later proxy or
request-line deployment verification.

## DTO and database boundaries

`http::dto` has separate validated request aggregates and serialized response
types for accounts, links, owned/anonymous creation, key/token replacement,
visit records, stats, breakdown, tags, and `{items,next_cursor}` collections.
Owned creation omits `management_token`; anonymous creation requires it. Link
`title` and `owner_id`, visit `referrer` and `user_agent`, breakdown `value`,
and collection `next_cursor` always serialize, including nulls. `short_url` is
built only from validated configured origin and immutable code. `active` needs
authoritative owner state for owned links; a missing owner state is a conversion
error.

SQLx `AccountRecord`, `LinkRecord`, and `VisitRecord` remain non-serializable
and without derived Debug. Checked conversions reject malformed stored roles,
codes, digests, search keys, tags, counters, timestamps, visit methods/device
values, optional-header limits, and non-host inet masks. IPv4-mapped IPv6 host
addresses normalize to bare IPv4; other IPs serialize as canonical bare host
addresses. Conversion failures are internal persistence errors, not caller 400s.
Part 05 centralizes IP normalization in `client_ip::CanonicalIp`, also used by
this converter and anonymous accounting. Its private accounting `DatabaseInstant`
is distinct from request/cursor `Timestamp`: exact PostgreSQL binary microseconds
and database interval arithmetic preserve rate-window boundaries without Chrono
range panics or application-clock sampling.
Link metadata uses a separate `DatabaseTimestamp` decoder for finite PostgreSQL
binary microseconds. It rejects infinities/invalid database ranges before DTO
conversion, then checks the narrower RFC 3339 wire range without invoking SQLx's
Chrono binary timestamp decoder. The accounting and link timestamp types have
separate responsibilities.
Historical visit DTOs use the stored destination snapshot. No migration was
needed: schema version 1 and `migrations/0001_initial.sql` are unchanged.
