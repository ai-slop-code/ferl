# Part 08 — Scoped link listing, search, tags and keyset pagination

**Dependencies:** part 07.

**Normative references:** business sections 3, 6.7, 11.2–11.5, 13; OpenAPI `listLinks`, `listTags` and their parameter components.

## Outcome

Account-authenticated clients can list complete links, combine all agreed filters/sorts, and discover canonical tags with scope-correct counts. Pagination remains stateless and works after boundary deletion and across instances.

## Files and responsibilities

- `src/links/{list_query,list_repository,list_handler}.rs`.
- `src/tags/{mod,query,repository,handler}.rs`.
- `src/pagination/{link_boundary,tag_boundary}.rs` and shared scope/context types.
- A new migration for query-driven scoped ordering/tag/search indexes.
- `tests/{link_listing,link_filters,link_pagination,tag_discovery}.rs`.

## Tasks

### 1. Shared scope and query validation

Require an enabled account key; missing is 401 and management tokens are 403. Default both users/admins to `scope=own`. `scope=all` is admin-only; a regular user receives 403. Unknown scope is 400.

`owner_id` is only permitted for admin all-scope. Non-admin presence is 403; admin own-scope presence is 400. Once permitted, parse an account UUID or exact `anonymous`; malformed values are 400 and a nonexistent/deleted valid UUID yields an empty result. Never drop an invalid owner filter or fall back to broader scope.

Use strict multiplicity-aware query parsing. Link-list allows exactly the documented scope/owner/q/enabled/created dates/tag/tag_mode/sort/order/limit/cursor parameters. Tag discovery allows only scope/owner/q/limit/cursor. Validate and canonicalize before building the cursor context.

### 2. Link-list query semantics

Construct one authorized candidate set, then apply all filters before page selection:

- Own scope requires current account UUID equality. All scope includes anonymous and disabled-owner live links.
- `q` is a literal substring in any of normalized destination, ASCII code or normalized non-null title. Use safely bound escaped LIKE patterns (including literal `%`, `_` and backslash), or another literal-substring implementation with a suitable query plan. Never expose SQL regex/wildcards.
- `enabled` filters the link's own flag, not active status.
- Creation bounds are inclusive lower/exclusive upper; explicit offsets normalize to UTC.
- Repeated tags are canonicalized/deduplicated with the 20-input limit. `any` means at least one association; `all` means every selected tag. Commas are literal. `tag_mode` without tags is 400.
- Combine distinct filter categories with AND; only search fields and any-tag matching use their local OR semantics.

Use EXISTS/subqueries or a distinct candidate-link relation to avoid duplicate items from tag joins. An unknown tag matches no association; an any-filter can still match a known tag, while an all-filter fails if any selected tag is absent.

Return the complete Link representation, including title, tags, origin-derived short URL, stored destination, persisted visit counter, creation/update timestamps and computed active. Preserve sorting while aggregating tags; use one query or bounded batched hydration rather than one query per item.

### 3. Correct sort and keyset boundaries

Supported fields: created_at, normalized title, persisted visit_count. Default created_at descending; each explicit field also defaults descending. UUID is the final tie-breaker in the same direction. Title comparisons use bytewise C collation and nulls last for both directions.

For non-nullable fields `k` and last boundary `(k0, id0)`, use tuple `<` for descending or `>` for ascending, with matching ORDER BY. Store the ordering value in the cursor so the boundary row may be deleted.

For title, let `cmp` be `>` for ascending or `<` for descending:

```text
Last title is non-null s:
  title_search cmp s
  OR (title_search = s AND id cmp last_id)
  OR title_search IS NULL

Last title is null:
  title_search IS NULL AND id cmp last_id
```

Apply the same collation to ordering, equality and boundary comparisons. Encode null explicitly; don't use a fabricated empty string/sentinel. Fetch limit+1 and base continuation on the last returned row. Bind cursor context to resource, caller UUID, scope, owner, normalized q/tags/date/enabled filters, tag mode, sort and direction. Reauthorize each page; role changes can forbid a formerly legal all-scope cursor.

With unchanged data every matching link appears exactly once. Document live pagination for mutable title/count ordering; concurrent movement may repeat/omit items and does not require cross-request snapshot storage.

### 4. Tag discovery

Derive rows from authorized **live links**, including individually disabled and suspended-owner links when in scope. Group canonical names, count distinct visible link IDs, and include only positive counts. Optional q matches canonical names only and never changes which linked accounts are visible.

Order names by ascending UTF-8 byte order. A tag cursor holds the last name and scoped normalized context; apply `name > boundary` after deriving complete per-tag counts, not after paginating links. Shared names do not leak another account's counts. Removal of the final visible association makes the name disappear immediately after commit.

### 5. Indexes and query evidence

Add scoped/global `(created_at, id)` and `(visit_count, id)` indexes as needed, title-sort expression/column indexes with explicit collation/null handling, and tag-membership indexes. Choose whether to enable PostgreSQL `pg_trgm` for escaped substring searches and document extension permissions. If using it, ensure normalized columns and query expressions actually permit index use.

Use representative fixtures and EXPLAIN evidence to identify whole-table scans, tag fan-out and N+1 hydration. Do not mandate an unrequested throughput target; make query/connection costs explicit and avoid loading entire result sets into Rust to sort/page.

## Verification gate

- Test own defaults for admins/users, admin all-scope, owner/anonymous filters, suspended owners, deleted accounts and wrong-operation management tokens.
- Combine all filter categories and check literal wildcard characters, Unicode folding, tag any/all, unknown/mixed tags and distinctness.
- Reject malformed/forbidden owner filters, unknown/repeated scalar parameters, invalid intervals/enums/limits and tag_mode without tags without broadening authority.
- For all six sort/direction combinations, use ties, null titles and small page sizes; traverse unchanged data with no omissions/duplicates. Delete the last row of a page and continue successfully.
- Reject changed context or tampered cursors; preserve valid continuation after key rotation. Cross-instance continuation is finalized in part 11.
- Verify complete requested display fields, empty envelope, independent same-destination links and destination-preserving title edits.
- Discover shared/canonical/anonymous tags with scope-correct distinct counts; clear/delete final visible associations and verify disappearance even if another account still uses the name.
- Storage failures produce 503 rather than an empty page.

**Handoff:** record final query/index choices and pagination fixture helpers. Main scenarios: 3, 42–49; titles/tags/list portions of 32 and 43 are ready for process verification.
