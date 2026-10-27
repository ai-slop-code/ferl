# Part 09 — Authorized visit records, summaries and breakdowns

**Dependencies:** part 08.

**Normative references:** business sections 4, 9, 11.1–11.2, 12–13; OpenAPI analytics operations and schemas.

## Outcome and endpoint inventory

| Operation ID | Route |
| --- | --- |
| `listLinkVisits` | `GET /api/v1/links/{code}/visits` |
| `getLinkVisitStats` | `GET /api/v1/links/{code}/stats` |
| `getLinkVisitBreakdown` | `GET /api/v1/links/{code}/stats/breakdown` |

All business operations are implemented after this part. Reports expose only persisted, authorized data, with no destination enrichment or extra aggregation dimensions.

## Files and responsibilities

- `src/analytics/{report_query,reports,report_repository,handlers}.rs`.
- `src/pagination/{visit_boundary,breakdown_boundary}.rs`.
- `tests/{visit_records,visit_stats,visit_breakdowns,analytics_authorization}.rs`.

## Tasks

### 1. Authorization and shared interval

Use fresh owner/admin/matching-token link authority before returning any rows or totals. An individually disabled link remains reportable by its eligible owner/token; an admin may report a suspended owner's link. Wrong object authority returns concealed 404. Revoked token/disabled-account credentials return 401, even when their old link still exists.

Parse `from` inclusive and `to` exclusive, optional independently, explicit offsets normalized to UTC. Both absent means all retained history; both present require from < to. Reject unknown/repeated query parameters. Stats accepts only from/to; visits additionally limit/cursor; breakdown additionally required one group_by and limit/cursor.

Use primary PostgreSQL and propagate read failures as 503. If authorization and aggregation require several statements, use a consistent per-response snapshot or combine them so concurrent deletion cannot turn a missing resource into a fabricated successful zero report. The contract is eventually consistent across requests, but a single summary should not mix unrelated snapshots of counter and records.

### 2. Raw records

Select the exact Visit representation: ID, link ID/code, immutable redirect_url, occurrence timestamp, GET/HEAD, canonical IP, full nullable referrer/UA and parsed families/device. Use occurred_at descending and UUID descending.

Filter first, then use keyset `(occurred_at, id) < boundary`, fetch limit+1 and emit a cursor bound to principal/link/interval/resource/order. Use stored snapshot/time; never substitute the current destination or worker write time. Serialize an inet address as a canonical bare IP without network mask.

### 3. Summary

Return exactly `{ lifetime_visits, matching_visits }`.

- Lifetime comes from the persisted counter and ignores date filters.
- Matching counts every persisted visit inside the interval, not a page.
- With no interval, both agree for a single consistent database snapshot.
- Empty intervals yield zero matching and preserve lifetime, even if the link is disabled.

Use a single statement with an authorized link relation/correlated count, or equivalent consistent transaction. Distinguish missing link from a valid link with no visits. Counts fit nonnegative int64.

### 4. Breakdown

Map a validated grouping enum to a static SQL expression, never interpolate raw group_by. Supported dimensions only: UTC day, method, referrer, client_ip, user_agent, device, browser, os.

Day uses `(occurred_at AT TIME ZONE 'UTC')::date` and renders YYYY-MM-DD independently of connection/session timezone. Other dimensions group by exact stored values. Do not fold referrers/agents/families or merge unknown with null. Client IP groups use canonical IP identity and emit its bare string form.

Filter interval → group all matching rows → order groups → page. Each item is exactly `{ value: string-or-null, visits: integer }`. Choose explicit C/UTF-8 byte order for string values and null first, consistently across grouping pagination; use rendered canonical IP strings rather than PostgreSQL inet numerical ordering for string boundaries.

Keep first page, boundary-after-null and boundary-after-string distinct. After a returned null bucket, remaining groups are all non-null; after a string value s, remaining groups satisfy value > s and exclude null. A null cursor boundary is not the same as an absent cursor.

Counts cover the whole interval for each group, independent of page. Generate continuation from the last returned value; bind link/principal/interval/grouping to context. Do not put complete raw grouping values in telemetry. Ensure the sealed cursor and documented HTTP/proxy URI bounds can round-trip a grouping value up to the 8,192-byte header limit, including JSON-escape expansion; a server-issued valid cursor must be usable on the next request.

## Verification gate

- Seed deterministic visits across UTC midnight, leap/day boundaries, explicit-offset filters, equal timestamps and all grouping values. Insert through the atomic event persistence path so counters are consistent.
- Check raw ordering/ties/page continuation, every required field and historical destinations A/B after editing.
- Check inclusive/exclusive date edges, single-sided filters, invalid/equal intervals, empty intervals and lifetime independence.
- Check all eight grouping dimensions, null-first boundaries, ordinary unknown buckets and exact distinct strings. Validate counts before/after pagination.
- Page a maximum-size raw-header grouping value through HTTP using its returned cursor; reject tampered/context-changed cursors and repeated group_by.
- Cover owner/admin/token success and all inaccessible/invalid-credential paths. No unauthorized response reveals counts, raw headers, IPs or historical URLs.
- Inject report-query outages; verify 503 rather than empty/zero data. Verify delayed ingestion is eventually reflected without assuming immediate consistency.

**Handoff:** report fixtures and privacy-sensitive response fixtures are inputs to telemetry and OpenAPI verification. Main scenarios: 3–4, 21–23, 27 and 50–51.
