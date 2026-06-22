# 0002 — Postgres for state (not SQLite) (decided)

## Context

The orchestrator + workers + CLI + web API + IMAP poller all need
to talk to the same state. SQLite is single-file and wonderful for
single-process apps; Postgres is heavier but supports concurrent
writers. Salesman is multi-process by design (orchestrator + CLI +
api + multiple background workers).

A Postgres dependency means VPS provisioning + backup + upgrades —
real ops cost. SQLite would have zero ops cost.

## Decision

We use Postgres 16+ (CI exercises migrations against postgres:17) as the
single source of truth for state.
Schema lives in `crates/salesman-state/migrations/*.sql` and is
applied via `salesman migrate` (sqlx::migrate!).

## Consequences

- ✅ Concurrent writers work without lock-thrashing
- ✅ Standard tooling (pg_dump, psql, EXPLAIN ANALYZE) for ops
- ✅ Real types (CITEXT, JSONB, UUID, BYTEA, timestamptz, INTERVAL)
- ⚠️  Ops cost: a Postgres install on the VPS that we have to
   monitor + back up. Daily pg_dump cron in place.
- ⚠️  Schema migrations are now a real concern; sqlx migrations
   table tracks state.
- ❌ We do not ship a "embedded mode" for tiny deployments. There
   is one mode: `salesman migrate` against a real Postgres.

## Alternatives considered

- **SQLite** — zero ops, but the IMAP poller + send-pending +
  classify-replies + summary all want concurrent access. SQLite
  WAL helps but doesn't eliminate the contention.
- **CockroachDB** — overkill for one-VPS deployment.
- **DynamoDB** — wrong shape for our queries (joins, GROUP BY,
  PERCENTILE_DISC). And cloud-coupled.

## Status

`decided 2026-04-26 by claude-code session`

## References

- `crates/salesman-state/migrations/0001_init.sql`
- `crates/salesman-state/migrations/0002_sequences.sql`
- `crates/salesman-state/src/lib.rs`
