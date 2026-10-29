# Pinned OpenAPI validation schema

`openapi-3.1.0-2022-10-07.json` is the OpenAPI Initiative's published OpenAPI
3.1 schema from repository commit
`OAI/spec.openapis.org@452b94d1a8a482da66ac6ec0e6b10378000cf361`, path
`oas/3.1/schema/2022-10-07`. Its verified Git blob ID is
`468bc7e5f5ac566a294ad7fd76f9244375690ab3` (33,992 bytes). The file is
vendored so validation does not fetch a schema at runtime or in CI.

That official schema validates the OpenAPI document structure and intentionally
leaves Schema Objects to the selected JSON Schema dialect. Accordingly,
`tests/openapi_release.rs` also compiles every named component schema and each
inline operation/parameter/header schema after resolving local references, with
locked `jsonschema` 0.60.2 Draft 2020-12 and format assertions enabled. It
rejects external/dangling references, checks operation IDs and path/method
inventory, compares the inventory to production TCP behavior, and keeps
negative controls for malformed OpenAPI and JSON Schema documents.
