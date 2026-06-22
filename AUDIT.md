# AVP-2 Tier 1 audit — first pass

> Per `AVP2_PROTOCOL.md` Tier 1 (Existence proof, passes 1–6).
> This file is the audit ledger; each entry is a per-crate first-pass
> review. Subsequent passes get appended.

Conventions:
- `OK` — checked + clean
- `NOTE` — checked, has annotated trade-offs
- `TODO` — known gap, tracked separately

---

## Pass 1 — 2026-04-27

Reviewer: claude-code session (auto). Subsequent human-review +
sign-off should add an entry below this with reviewer name + date.

### Skeleton audit (no unused public APIs)

| Crate | Status | Notes |
|---|---|---|
| salesman-core | OK | All public re-exports point at types in active use by other crates. Newtypes for IDs (`UUIDv7`) prevent boolean-blindness. |
| salesman-state | OK | `State` is the only public surface; query methods are all called by salesman-cli or salesman-api. |
| salesman-llm | OK | Trait `LlmBackend`, struct `LlmRouter`, two backends. All used. |
| salesman-tools | OK | `Tool` trait + `ToolRegistry` + `EchoTool` (smoke-test only — kept for `salesman tools` CLI surface visibility). |
| salesman-discovery | OK | 3 + 1 (Brave) tools, all called by CLI. `HomepageFetcher` used by both tool path and `enrich` CLI. |
| salesman-content | OK | 9 tools + 1 site-renderer. All called. |
| salesman-osint | OK | 6 tools (GDELT/GitHub/HN/DNS/Wayback/Wikipedia), all called. |
| salesman-outreach | OK | `SmtpSender` + helpers; all called by `send-pending`. |
| salesman-reply | OK | `ImapPoller` + `ParsedReply`; all called by `inbox-poll`. |
| salesman-orchestrator | NOTE | Currently agent-loop skeleton; `ToolRegistry::schemas()` is the only loop-tool plumbing. Will see heavier use when sequence scheduling drives the agent. |
| salesman-cli | OK | 58 subcommands, all wired. |
| salesman-api | OK | 7 operator routes + 2 public unsubscribe routes, all wired. |
| salesman-receipts | OK | `Signer`, `Receipt`, `verify_receipt`, `verify_chain`, helpers. All called by CLI + state + api. |
| salesman-detector | OK | `score()`, `RiskScore`, `SignalHit`. All called by `approve` CLI. |
| salesman-competitor | TODO | Empty placeholder. Land in P2.5. |

### Null/zero/empty sweep

| Crate | Status | Notes |
|---|---|---|
| salesman-state | OK | `insert_companies(&[])` returns Ok(0). `upsert_prospects_for_campaign(&[])` returns Ok(0). Empty CSV reads return Ok(empty). Empty body → draft skipped by detector or LLM error path. |
| salesman-discovery | OK | Empty homepage HTML produces empty signals; CSV with no rows returns empty Vec. |
| salesman-receipts | OK | `verify_chain(&[], _, &zero_hash())` returns Ok (vacuously) — a hash chain alone can't see an emptied/truncated table; detecting that needs `verify_chain_anchored` against a trusted off-box/append-only anchor (in-Postgres anchor = no guarantee). `prev_hash != HASH_LEN` errors loudly. See `docs/AUDIT_CHAIN.md`. |
| salesman-detector | OK | Empty body → score 0.0. Body `< 80` chars skips em-dash density (avoids divide-by-tiny). |

### Boundary sweep

| Crate | Status | Notes |
|---|---|---|
| salesman-state | OK | All counts use `::BIGINT`; UUIDv7 sorts naturally; CITEXT for case-insensitive email + domain compare. |
| salesman-receipts | OK | Hash + signature lengths checked (32 / 64 bytes) on every receipt verify. Scheme v2: the signature authenticates the FULL receipt (id, event_kind, signing_key_id, created_at, prev_hash, payload), not just `prev_hash‖payload`, so mutating any field invalidates it. `verify_chain` enforces per-key scoping — a chain whose `signing_key_id` changes mid-stream is rejected. See `docs/AUDIT_CHAIN.md`. |
| salesman-discovery::homepage | OK | 4 MiB body cap before parse. 5-redirect cap. 15s timeout. |
| salesman-llm | OK | Both backends time out at 180s. |
| salesman-content::seo_meta | OK | Title hard-truncated to 60 chars; description to 160. Char-count, not byte-count, so unicode safe. |
| salesman-detector | OK | Score clamped to [0.0, 1.0]. Threshold compare is strict `<`. |

### Error-path completeness

| Crate | Status | Notes |
|---|---|---|
| salesman-cli | OK | All `?` paths return through `anyhow::Result`; `Cmd::Approve` rejects detector-fail with `--force-override` as the explicit escape hatch. |
| salesman-state | OK | All sqlx errors map to `Error::Db(string)` so we don't leak internal path types via sqlx. |
| salesman-llm | OK | Transport errors and parse errors both map to `Error::Llm` with backend name. |
| salesman-outreach | OK | Pre-flight failures (suppression, rate-cap) skip + log; SMTP failures bubble up + caller decides retry. |
| salesman-receipts | OK | Tampering detected by recompute; signature failure detected by verify. Either is `Error::Validation`. |

### Type tightening

| Crate | Status | Notes |
|---|---|---|
| salesman-core | OK | UUIDv7 newtypes for every ID. Enum variants `#[strum(serialize_all = "snake_case")]` so DB serialization matches. `FunnelState::can_transition_to` exposes the legal transition graph in code; proptest verifies. |
| salesman-llm | OK | `BackendKind` enum + `Hash` derive enables HashMap routing. `RouteHint::Backend(BackendKind)` for explicit pinning. |
| salesman-tools | OK | `Tool` trait + JSON Schema descriptor; tools can't be invoked without a registered name. |
| salesman-detector | OK | `RiskScore` carries hits as data; not an opaque float. |

### Dependency audit

| Status | Notes |
|---|---|
| OK | Workspace deps are pinned at minor for major libs. `cargo audit` not yet run on the box (TODO: install + run). `cargo deny` likewise. Both wired into `scripts/check.sh` so they're a one-command run when needed. |
| NOTE | `aws-lc-rs` pulled in transitively by lettre/rustls — large native dep but mainstream + maintained. |
| NOTE | No `geiger` count taken yet — workspace is `#![forbid(unsafe_code)]` at every crate root, so geiger should report 0. |

### Sign-off

`SHIP-DECISION: 2026-04-27 — current state is operator-review-only.
Send path is keyed off explicit --for-real flag + owner-set SMTP env.
Accepted residual risks:
  - cargo audit not yet executed against this commit (TODO)
  - cargo deny not yet executed against this commit (TODO)
  - K3 integration test not yet run against live Postgres (test infra
    in tree; needs TEST_DATABASE_URL exported)
  - K4 perf bench (1000 prospect cycle) not yet measured
  - LinkedIn / X / web-form channels not yet implemented
  - First-real-send blocked on owner-decided sender identity (OD-2)
Reviewer: claude-code session 2026-04-27`
