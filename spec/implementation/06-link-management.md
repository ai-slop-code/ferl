# Part 06 — Link creation, editing, deletion and management tokens

**Dependencies:** part 05.

**Normative references:** business sections 4, 6–7, 11.1–11.2, 12–13; OpenAPI operations below and complete Link/LinkCreation schemas.

## Outcome and endpoint inventory

| Operation ID | Route |
| --- | --- |
| `createLink` | `POST /api/v1/links` |
| `getLink`, `updateLink`, `deleteLink` | `GET`, `PATCH`, `DELETE /api/v1/links/{code}` |
| `rotateManagementToken` | `POST /api/v1/links/{code}/management-token` |

The server now supports complete API management of owned and anonymous links, including titles/tags. Listing is added in part 08 and public redirects in part 07.

## Files and responsibilities

- `src/links/{mod,service,repository,allocation,handlers,representation}.rs`.
- `src/links/tokens.rs` or shared credential mutation helper.
- Link/tag writes, creation request decoding and route wiring.
- `tests/{link_creation,link_management,link_authorization,token_rotation,creation_order}.rs`.

## Tasks

### 1. Enforce the creation pipeline at the HTTP boundary

Run: supplied-credential authentication → creator eligibility → committed rate charge → bounded body/media-type/JSON handling → feature and domain validation → creation transaction. Do not put an Axum JSON extractor or early Content-Length/body-limit rejection before the charge. Transport-level rejections before application handling remain outside accounting.

After raw object decoding, an anonymous request supplying `short_code` returns 403 `forbidden` after charging; retain field presence even if null or otherwise invalid. Malformed JSON cannot supply a reliable field set and returns 400 after charging. Enforce strict unknown-field and type rules for other inputs.

Charge eligible invalid destinations, invalid titles/tags, conflicts, malformed/oversized bodies, unsupported media types and later storage failures. Rate-rejected requests add no charge. Duplicate destinations and HTTP retries create independent attempts/links.

### 2. Allocate and commit

Validate/normalize destination, title and tags using shared types. Authenticated ownership comes exclusively from the current account UUID; anonymous ownership is null and gets a fresh management token. Defaults are enabled true, zero visits, absent title null, tags empty.

Begin a new transaction after the charge. For account creators, take the shared authority gate and revalidate current credential/status before inserting. Reserve a requested custom code via authoritative insert; a conflict, including `api` or a deleted reservation, returns 409 `short_code_unavailable`.

For generated codes use a secure unbiased candidate source and at most ten reservation attempts with conflict-safe SQL. Generate a fresh candidate after each collision. Exhaustion returns 503 `code_generation_unavailable`. Keep all inserts in one transaction so failed creation does not burn an uncommitted reservation. Separate code-collision handling from unrelated constraints.

Insert link, derived search columns and canonical tag associations atomically. Return 201 only after commit, with full `link` and, only for anonymous creation, `management_token`. Build `short_url` from configured origin and stored code, never request Host/forwarded values.

### 3. Read and object authorization

Resolve fresh credentials and target link authority. Owner account, admin or matching token can read; another valid account/token gets exactly the same 404 body as an unknown link. Missing credentials get 401. Ordinary representation contains all required fields, explicit nullable title/owner and canonical tags, and no digest/token.

Compute `active` from live link.enabled and owner.enabled, without persisting a second authoritative flag. Admins can inspect suspended-owner links. Enabled owners and token holders can inspect individually disabled links. An account-disabled key cannot authenticate.

### 4. Atomic PATCH and DELETE

For PATCH, validate every supplied change before applying any; lock and revalidate authority using the shared guard protocol. Missing means preserve, title null clears, tags `[]` clears, required-value nulls fail. Empty/unknown/immutable-field updates fail with 400.

Update destination and its search form together, title and its search/sort form together, and replace the complete tag set under the link lock. Do not merge concurrent tag replacements. Compare canonical/effective values to detect actual changes; only those advance `updated_at`. Preserve code/owner/creation time/counter and unspecified fields.

DELETE locks/revalidates and removes only the identified link, its tags, token and visits. Preserve minimal reservation. Other links sharing its destination are unaffected. Return bodyless 204 after commit; repeated deletion with an otherwise-valid account/token returns 404. The deleted link's own former token is invalid and therefore returns 401 before object lookup.

### 5. Token replacement

Only a matching token or admin may invoke replacement. A regular account key has wrong operation scope and gets 403; a valid token for another link gets concealed 404. An admin targeting an owned link gets 409 `not_anonymous_link`.

Under the target link lock, revalidate token/admin authority, replace digest, commit, and return the new plaintext once. No body is accepted. Existing anonymous links/tokens keep working when anonymous creation is turned off. Token rotation is atomic with respect to token-authorized mutation; stale tokens cannot commit after revocation.

## Verification gate

- HTTP tests prove creation precedence using both response codes and persisted rate-charge counts, including malformed JSON, wrong media type, >64 KiB body, anonymous custom-code presence, invalid credentials and later rollback.
- Create authenticated/anonymous links with full/minimal metadata and inspect exact complete representations/one-time secret presence. Repeated destinations remain independent.
- Race custom allocation with independent connections: one 201, one 409, no overwrite/orphan reservation. Inject generated candidates to prove retries and ten-candidate exhaustion.
- Cover owner/admin/matching-token/unrelated-account/unrelated-token authority on every operation, including unknown/deleted/disabled links.
- Verify title Unicode boundaries, null clearing, omission preservation, canonical tags and all-or-nothing invalid multi-field updates. Test no-op PATCH timestamps and serialized concurrent tag replacement.
- Verify token reset/rotation revokes the old value, ordinary reads leak none, and disabling anonymous creation leaves old tokens valid.
- Repeat credential/account suspension races against actual link mutations. Verify delete cascades and account deletion with publicly created links.
- Simulate DB failures without changing them into conflicts, missing data or invalid credentials.

**Handoff:** document the link representation builder and a fresh single-statement active-link resolver for part 07. Main scenarios: 1–4, 9–13, 17, 26–28, 41, 44–45; public behavior completes next.
