# Archived: Scoped Replication Hooks

**Status:** Removed in TalaDB 0.11.0.

This document previously described the replication-backed React API shipped in
0.9.x (`ReplicationProvider`, `useQueries`, `useCoverage`, and sync-contract
transport). TalaDB 0.11.0 removes the replication engine and those public APIs
as part of the slim-core release. The design below is therefore no longer an
implementation guide or a statement of current behavior.

The replacement is the smaller local-first query layer exported from
`@taladb/react/query`:

- `QueryProvider` owns backend routing and drain policy.
- `useQuery` hydrates canonical server documents into normal local collections.
- `useMutation` supports optimistic, queued, and immediate writes.
- Pending state lives atomically on each document rather than in the removed
  replication log/outbox.
- Client-generated `_id` values are authoritative, and successful write
  responses return canonical documents.

See the current [query guide](../../../../docs/guide/query.md) and the
[0.11.0 changelog](../../../../CHANGELOG.md) for the supported contract and
migration notes. Historical 0.9.x release notes remain in the changelog for
users maintaining older applications.
