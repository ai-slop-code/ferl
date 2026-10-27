# Part 04 — Authentication, bootstrap and account administration

**Dependencies:** part 03.

**Normative references:** business sections 4–5, 11.1–11.2, 12, 13; OpenAPI operation IDs below.

## Outcome and endpoint inventory

Implement:

| Operation ID | Route |
| --- | --- |
| `getCurrentAccount` | `GET /api/v1/me` |
| `rotateCurrentApiKey` | `POST /api/v1/me/api-key` |
| `listAccounts`, `createAccount` | `GET`, `POST /api/v1/admin/accounts` |
| `getAccount`, `updateAccount`, `deleteAccount` | `GET`, `PATCH`, `DELETE /api/v1/admin/accounts/{id}` |
| `resetAccountApiKey` | `POST /api/v1/admin/accounts/{id}/api-key` |

Also implement the one-time `ferl-server bootstrap-admin --username <username>` operator command. Link-token authentication is implemented now against fixture links; its public creation/rotation routes arrive in part 06.

## Files and responsibilities

- `src/auth/{mod,bearer,principal,guard}.rs`: parsing, fresh principal lookup and commit-time authority guards.
- `src/accounts/{mod,service,repository,bootstrap,handlers}.rs`.
- Account query/DTO wiring in `src/http/` and account boundaries in `src/pagination/`.
- `tests/{authentication,accounts,bootstrap,account_concurrency}.rs`.

## Tasks

### 1. Authentication and error distinctions

Parse exactly one unambiguous Authorization bearer credential. Treat the HTTP authentication scheme case-insensitively; validate secret syntax exactly. Reject duplicate header fields, comma-combined/ambiguous values, unsupported scheme, empty/malformed credentials and revoked digests with 401 `invalid_credentials` and `WWW-Authenticate: Bearer`.

Represent principals as absent, enabled account (ID and current role), or anonymous-link token (link ID). Query current database state for every business request. Missing credentials on a protected operation are 401 `authentication_required`; valid wrong-operation credentials are 403 `forbidden`; database failure is 503 `storage_unavailable`, not 401.

Do not treat query/body fields as credentials. Invalid credentials on public redirect/health routes are ignored by dispatch rather than run through this extractor. No process-local principal cache is allowed.

### 2. Commit-time authority guard

Implement the shared/exclusive account-authority gate and link-token row protocol from architecture.md. Retain the presented digest securely for transaction revalidation without putting it in DTOs or logs.

All account writes acquire the exclusive gate before re-reading the caller and checking admin/self authority. Ordinary account-authorized link mutations in part 06 use the shared gate. A token-authorized mutation rechecks its digest after locking the corresponding live link. Keep locks until commit.

Add repository/service helpers for object-level link authority without exposing link details outside it. Admin or owner may access a live link; matching token may access its own anonymous link; otherwise return indistinguishable 404. Disabled links remain manageable to otherwise eligible actors.

### 3. Bootstrap transaction and output

Validate username, generate a fresh key, then transactionally lock authority and singleton initialization state. Require `bootstrap_completed=false`, insert one enabled admin and mark completed together. A username/database failure rolls back both. Concurrent invocations have one winner.

After acknowledged commit, write an operator result containing the admin identity and plaintext key exactly once to the command's result stream. Keep diagnostics on the logging stream and exclude the key. Failure/restart does not print a replacement or reopen bootstrap. Document recovery from uncertain/lost output as an operator concern without adding an automatic recovery feature.

### 4. Account operations

- Create: admin only; exact immutable username, optional role default `user`, enabled true. Return 201 with account and initial key after commit. Unique conflict is 409 `username_unavailable`.
- Me/detail/list: complete representation without credentials/digests. Account list includes disabled accounts, ordered `(created_at DESC, id DESC)`. Paginate with limit+1 and a context-bound keyset cursor; default/max limits follow the shared contract.
- Patch: admin only, at least one of role/enabled, strict non-null fields. Revalidate caller's current role before applying changes, including self-targeted changes.
- Prevent removal of the last enabled admin. Under the exclusive gate evaluate the proposed role/status or deletion and return 409 `last_admin_required` without any partial effects when it would leave none.
- Rotate/reset: generate a replacement, atomically replace the digest, and return 200 with only `api_key` after commit. Self-rotation requires an enabled account; admin reset can target a disabled account without enabling it.
- Delete: admin only; protect last admin; delete account plus links/tags/visits in one transaction via the schema's cascades. Preserve reservations. Return bodyless 204 after commit and 404 on repetition with an otherwise-valid credential.

Account suspension preserves the current digest and owned links' individual flags. Role changes apply to the same key immediately. Username reuse after deletion gives a new UUID and does not inherit ownership. Bodyless endpoints reject bodies; validate path UUID and allowed query fields using shared errors.

Generate created/updated timestamps on the server. Keep created_at fixed; update account updated_at when persisted account state actually changes, including replacing its key digest. As an internal timestamp convention, a validated no-op account PATCH preserves updated_at.

## Verification gate

- Execute bootstrap success, repeat failure, concurrent attempts and restart persistence. Inspect both DB rows and output/log capture for one-time key behavior.
- Test missing/invalid/duplicate credentials, disabled/re-enabled account authentication and valid management token on wrong-operation endpoints.
- Cover user/admin permissions for every account route, username conflicts/reuse, disabled-account reset, self-rotation and ordinary representation secrecy.
- Page account fixtures with timestamp ties and deleted page-boundary rows; reject mismatched actor/resource cursors.
- Race two enabled admins being disabled/demoted/deleted; exactly permitted operations commit and at least one enabled admin remains. Test a self-action that waits behind another admin's change and must revalidate afterward.
- Use synchronization hooks to pause a mutation before its guard, commit rotation/suspension first, resume and prove old credentials cannot commit. Also demonstrate the allowed ordering when the mutation holds its guard first and invalidation waits.
- Seed owned links/visits to verify atomic deletion and preserved reservations before the public link API exists.
- Inject database failures into authentication and mutations and verify 503 without raw database messages.

**Handoff:** document principal and transaction-guard APIs for parts 05–06. Main scenarios: 2, 5–9, 20, 27–28; object-level scenarios 3–4 become complete with link routes.
