# Collaborating on Salesman

How multiple people (and agents) work on PlausiDen-Salesman together without
breaking `main`, leaking PII, or sending email no human approved. If you're
about to touch the code, read this first.

## Read-first

1. [CLAUDE.md](../CLAUDE.md) — doctrine + hard rules (the non-negotiables).
2. [HANDOFF.md](../HANDOFF.md) — current runtime state / where we left off.
3. [SCOPE.md](../SCOPE.md), [ARCHITECTURE.md](../ARCHITECTURE.md),
   [ROADMAP.md](../ROADMAP.md) — what we're building and in what order.
4. The ADRs in `docs/decisions/` — why key choices were made (SaaS-LLM +
   redaction boundary, Postgres, owner-in-the-loop, audit-chain v2).

## Branch & merge flow

- `main` stays green. Do work on a feature branch (`claude/<topic>-<date>`).
- Open a PR (CI runs on PRs to `main`) **or** merge locally with `--no-ff`
  after the gates + review below pass. Record the rollback target (`main`'s
  current SHA) before merging.
- Keep commits scoped and auditable (one concern per commit; security changes
  in their own commit). After merging, confirm `git diff <branch>..main` is
  empty and CI is green on the merge commit.

## The gate (run before every merge)

`scripts/check.sh` runs the core gate; CI (`.github/workflows/ci.yml`) is the
source of truth. All must pass:

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace` (unit) + the gated DB integration tests against an
  ephemeral Postgres (`TEST_DATABASE_URL`)
- `cargo deny check` (advisories + licenses + bans + sources)
- `shellcheck scripts/*.sh`
- the docs presence gate (key docs + ≥ 5 ADRs)

**Toolchain:** some transitive deps require rustc ≥ 1.88 (edition 2024). Use a
current `rustup stable` toolchain; the system 1.85 will fail to resolve. A
build failure that names a dep's MSRV is an environment issue, not a defect.

## Review / audit discipline

- Security-sensitive changes — anything touching the **PII redaction boundary**
  (`redact`, `prospect_pii_terms`, LLM call sites), the **receipts/audit-chain**
  (`salesman-receipts`), email **headers/sending**, or **secret handling** —
  get an adversarial review *before* merge. Prove the property with tests
  (e.g. "mutating field X breaks verification", "the prompt carries no raw
  email").
- Report blockers; don't silently fold large fixes into an unrelated merge
  (what you merge should equal what you reviewed). Trivial mechanical greening
  (`cargo fmt`) is fine but must re-enter the gate.

## Owner-gated — never automate these

Per [CLAUDE.md](../CLAUDE.md) / [HUMAN_IN_THE_LOOP.md](../HUMAN_IN_THE_LOOP.md):

- **Real sends.** Outreach is human-approved per batch; a real send is
  `salesman send-pending --for-real --confirm-typed` run by a person. No
  script, cron, or agent runs it (`salesman-daily.sh` documents this).
- **Go-live** (first real send, R4) and anything in
  [OWNER_BLOCKERS.md](../OWNER_BLOCKERS.md).
- No selling/sharing scraped contact data; no PII to third parties beyond the
  redaction boundary; B2B opt-in only; rate limits intact.

## Secrets

Never commit secrets. They come from the deployment env-file
(`/etc/salesman.env`, see [DEPLOYMENT_GUIDE.md](DEPLOYMENT_GUIDE.md)). Secrets
are zeroized in memory, redacted in `Debug`, and never logged
([SECURITY.md](../SECURITY.md), [PII_REDACTION_BOUNDARY.md](PII_REDACTION_BOUNDARY.md)).

## Picking up work

Available work is in [OWNER_BLOCKERS.md](../OWNER_BLOCKERS.md),
[SALESMAN_TODO.md](../SALESMAN_TODO.md), and [ROADMAP.md](../ROADMAP.md). Update
[HANDOFF.md](../HANDOFF.md) when you change runtime state or stop mid-task.
