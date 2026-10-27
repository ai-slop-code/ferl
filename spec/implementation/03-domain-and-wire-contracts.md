# Part 03 — Validated domain values, strict HTTP input and cursor codec

**Dependencies:** part 02.

**Normative references:** business sections 3, 4, 6.2–6.3, 6.6–6.7, 10, 11.2–11.5 and 12; OpenAPI parameter and schema components.

## Outcome

Reusable, testable domain and wire types encode the contract's normalization and missing/null distinctions. Later handlers cannot accidentally accept unknown fields, widen query semantics, or expose secrets through a shared record serializer.

## Files and responsibilities

- `src/domain/{ids,url,short_code,username,title,tags,search,time,secret}.rs`.
- `src/http/{body,query,dto,rejection}.rs`: strict raw-to-typed parsing and error mapping.
- `src/pagination/{mod,codec,context}.rs`: versioned sealed cursor envelopes and shared page DTO.
- `tests/domain_contracts.rs`, `tests/http_decoding.rs`, `tests/cursor_codec.rs` or equivalent pure-module tests.

## Tasks

### 1. Destination and public-origin validation

Check the original destination before parsing: UTF-8 byte length at most 8,192; no surrounding Unicode whitespace or literal ASCII whitespace/control; explicit case-insensitive `http://` or `https://` authority syntax. Parse using a standards-compliant URL library, require HTTP(S), nonempty host and a valid port, and reject all user-info.

Inspect the original authority as necessary to detect explicitly empty user-info (`http://@host`), which a parser may erase. Do not rely only on nonempty `username()` or `password()`. Ensure malformed/absent authority is not accidentally admitted by parser repair. Do not reject valid private/localhost/internal/IPv6/IDN hosts through an extra network policy.

Serialize using the URL library, enforce the 8,192-byte bound again, and validate conversion to a Location header. Persist/return that serialization. Preserve query order and fragment. Make no DNS, HTTP or metadata requests. The public-origin variant also rejects path prefixes, user-info, query and fragment and normalizes the root slash away.

### 2. Text and code types

- Username: exact `[a-z0-9_-]{3,64}`.
- Custom code: exact `[a-z0-9-]{3,64}`; do not lowercase/trim. Syntax validation is separate from `api`/occupied reservation conflicts.
- Generated code: eight independently uniform selections from the 36-character alphabet using OS-backed secure randomness. Use rejection sampling or a verified unbiased API, not random-byte modulo 36.
- Title: trim Unicode whitespace, then 1–200 Unicode scalar values without controls. Preserve caller casing/plain text. Nullable semantics belong in DTOs.
- Tag: check at most 256 input scalar values; trim, full locale-independent casefold, NFC; require 1–64 canonical scalars and no controls. Check at most 20 array inputs before deduplicating and sort unique results by UTF-8 bytes.
- Search: trim and require 1–256 scalar values without controls, then use the same full casefold/NFC comparison transform. Folding may expand text; do not mistakenly apply an input bound to the expanded result. Store compared link fields using that transform without trimming their actual stored contents further.
- Times: accept RFC 3339 with explicit offsets, normalize to UTC, reject missing offsets; output `Z`. Use inclusive lower/exclusive upper intervals and reject equal/reversed supplied bounds.

Document the Unicode implementation/data version. Test `Straße`/`STRASSE`, sigma forms, composed/decomposed accented names, whitespace, control characters, expansion boundaries and non-ASCII scalar-vs-byte distinctions.

### 3. Secret material

Choose and document distinguishable versioned opaque formats, for example `ferl_ak_` and `ferl_mt_` followed by canonical unpadded base64url of 32 random bytes. The prefixes do not count toward entropy. Digest the **complete presented secret**, including prefix, with SHA-256 and store exactly the 32 digest bytes.

Create types for one-time plaintext output, redacted internal credentials and digests. Ordinary account/link DTOs cannot serialize either secret or digest. Validate supported format, exact encoding/length and bearer syntax without falling back to another credential class. Do not persist plaintext or generate credentials from UUIDs.

### 4. Strict body/query decoding

Implement a bounded 65,536-byte body reader; map oversized bodies to 413, required JSON media-type failures to 415, and malformed/invalid JSON to 400. Apply the creation reader only **after** part 05's eligibility/charge stage. Enforce bodylessness for key/token replacement and DELETE requests; caller-chosen secrets or even `{}` are not accepted replacements.

Use explicit field-presence types. A plain `Option<T>` often conflates omitted and null and is insufficient here:

| Field | Create omission | PATCH omission | Explicit null |
| --- | --- | --- | --- |
| `title` | No title | Preserve | Allowed; no title/clear |
| `tags` | Empty array | Preserve | 400 |
| `short_code` | Generate | Immutable/unknown field | Invalid; anonymous presence has its feature-permission rule |
| `destination_url` | Required | Preserve | 400 |
| `enabled` | Not a create field | Preserve | 400 |

Reject unknown fields, wrong scalar types, non-object request roots, and empty PATCH objects. Preserve field presence long enough for anonymous `short_code` permission handling; successful raw object parsing must not discard that fact before its 403 check.

Parse query pairs while retaining multiplicity. Reject unknown names and repeated scalars instead of accepting first/last values. Only repeated `tag` is an array; commas are literal. Enforce exact booleans/enums, integer limits 1–200, canonical times and per-operation allowed names. Make query parsing contextual so redirects ignore all queries while API operations validate theirs.

### 5. Cursor codec and representations

Implement architecture.md's sealed cursor format with configured shared key, random nonce, versioning, associated data and strict payload validation. Use typed boundary variants rather than arbitrary JSON passed into SQL. Include enough space for the largest permitted grouped analytics value. Treat cursor data as sensitive request data for logs.

Build shared collection DTO `{ items, next_cursor }`, full account/link/visit DTOs and secret-bearing creation/replacement wrappers matching OpenAPI exactly. Ensure absent optional representation values serialize as explicit null where required. Count types fit signed int64 and never emit negative values.

## Verification gate

- Pure boundary suites cover the rules above, including URL serialization expansion past the byte limit, empty user-info, allowed internal hosts and query/fragment preservation.
- A failed normalization/validation produces no partially validated mutable aggregate.
- HTTP decoder checks exercise exact 64 KiB boundary, malformed bodies, media type, unknown fields, repeated scalar parameters and presence/null distinctions.
- Secret formats are distinguishable, entropy comes from the OS, complete-secret hashing is verified, and redacted debug/serialization paths cannot expose them.
- Cursor tests cover tampering, truncation, wrong key/version/resource/filter/actor, oversized input, explicit null boundaries, equivalent normalized contexts and round trips with maximum legal grouping values.
- JSON fixtures match required field names/nullability and UTC formatting.

**Handoff:** document validator APIs, presence type, cursor context format and chosen libraries. These support 10–12, 19, 23, 41, 45, 47–49 and 51; end-to-end proofs remain with the relevant services.
