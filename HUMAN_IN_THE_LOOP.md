# Human-in-the-loop guarantees

Every gate where a human MUST act before something irreversible
happens. This is the contract — code that violates it is a bug,
even if tests pass.

> **Owner directive 2026-04-27:** "very important we do this right
> and that the salesman has a human in the loop to ensure quality.
> we dont want it damaging our reputation."

## The gates (in send-path order)

### Gate 1 — `awaiting_approval` → `approved`
**Where:** `salesman approve --touch <uuid>`
**Who:** human operator
**Pre-conditions checked automatically before approve succeeds:**
- Touch is in `awaiting_approval` outcome (not `approved`,
  `sent`, etc. — strict)
- AI-detector score < threshold (default 0.6) — refuses approval
  with reasons listed unless `--force-override "<reason>"` passed
**Override path:**
- `--force-override "<reason>"` is the only way past a detector
  fail. Reason is logged at WARN.
**Code:** `salesman-cli/src/main.rs::Cmd::Approve` +
`salesman-state::approve_touch` (UPDATE WHERE outcome = 'awaiting_approval')

### Gate 2 — `approved` → `sent`
**Where:** `salesman send-pending --campaign <name> --for-real`
**Who:** human operator (the `--for-real` flag is the act)
**Pre-conditions checked automatically per touch:**
- `to_address` is resolved (skip + log if no primary contact email)
- `is_suppressed(to_address)` is false (skip + log if hit)
- Per-recipient touch count over `--per-recipient-window-hours`
  is below `--per-recipient-max` (skip + log if hit)
- Per-domain touch count over `--per-domain-window-hours` is below
  `--per-domain-max` (skip + log if hit)
- (planned) Per-campaign cost cap not exceeded (auto-pause if so)
**Default behaviour:** without `--for-real`, every touch is logged
as `[DRY-RUN] would send: ...` — no SMTP traffic.
**Code:** `salesman-cli/src/main.rs::Cmd::SendPending` +
`salesman-state::mark_touch_sent` (strict: only from `approved`)

### Gate 3 — Sequence advancement → next draft generation
**Where:** `salesman tick-sequences`
**Who:** scheduler (cron / systemd timer); but the result is a
draft, NOT a send. Each new draft re-enters Gate 1.
**Pre-conditions:**
- `prospect_sequence_state.next_due_at <= NOW()`
- `paused = false` (sequence paused for any reason — e.g. opt-out
  reply — skips this prospect entirely)
**Output:** new touch in `awaiting_approval` outcome. Operator
review required (Gate 1).

### Gate 4 — Reply triggers state transitions
**Where:** `salesman classify-replies` (cron / timer)
**Auto-actions allowed (reputation-PROTECTING only):**
- Optout reply → instant suppression of from-address + pause
  sequence + suppress all in-flight touches for that prospect
  (`outcome → suppressed` for any `awaiting_approval`/`approved`)
- Bounce reply → mark `contact.email_verified = FALSE`
- Optout signal from heuristic keyword check fires INDEPENDENTLY
  of LLM — either signal is sufficient
**Auto-actions NOT allowed:**
- Engaged reply does NOT auto-respond. Operator handles the human
  reply themselves.
- Question reply does NOT auto-respond.

### Gate 5 — Brand-content publication
**Where:** `salesman render-site` produces HTML; publication
(uploading + Caddy serving) requires owner action.
**Who:** human (no auto-deploy of comparison pages or case studies).

## What the operator can NOT bypass

These are NOT exposed as flags or env vars. Code that bypasses
them is a bug:

1. Approval gate without an explicit `approve` action.
2. `for_real` without the `--for-real` flag literal on the command line.
3. Suppression check before send. (Even `--force-override` only
   bypasses the *detector*, not the suppression list.)
4. Rate caps without the operator passing larger numbers explicitly.
5. AI-detector run on every draft (even templated drafts get scored).
6. Receipt signing on every send. (Failure to sign = failure to mark
   as sent.)

## What the operator MUST do for the system to function

1. Set sender identity (`SALESMAN_FROM_NAME`, `SALESMAN_FROM_EMAIL`,
   `SALESMAN_REPLY_TO`, `SALESMAN_LIST_UNSUBSCRIBE`,
   `SALESMAN_COMPLIANCE_FOOTER`).
2. Set LLM keys (`ANTHROPIC_API_KEY` and/or `GEMINI_API_KEY`).
3. Pre-set Postgres signing key (auto-generated on first run if
   missing — but operator should back it up).
4. Periodically: `salesman audit` to verify the receipt chain.
5. Periodically: `salesman costs --since-hours N` to watch LLM spend.
6. Periodically: `salesman summary --since-hours N` (or read the
   automated daily email).

## Override audit trail

Every `--force-override` is logged at WARN with:
- the touch_id
- the operator-supplied reason
- the detector score that triggered the override
- the detector reasons (so the override can be rationalised in
  hindsight)

Search for overrides:
```
journalctl -u salesman-* --since 30d | grep -E "OPERATOR OVERRIDE"
```

## Reputation-protecting defaults (don't lower without thinking)

| Default | Set in | Rationale |
|---|---|---|
| Detector threshold = 0.6 | `salesman approve` | Strict-enough that "I hope this finds you well" fails |
| Per-recipient = 5 / 30d | `send-pending` | More than 5 touches in a month is harassment |
| Per-domain = 10 / 1h | `send-pending` | Burning a domain in 24h is the easy mistake |
| Sequence delay step 1 = 0 days | `templates/cold/introduction.toml` | First touch on assign |
| Sequence delay step 2 = 5 days | `follow_up_1.toml` | Standard B2B pacing |
| Sequence delay step 3 = 7 days | `follow_up_2.toml` | Slow down between touches |
| Sequence delay step 4 = 14 days | `breakup.toml` | Final touch after a real gap |
| Detector retry max = 2 | `DraftColdEmailTool` | 3 attempts total — past that, defer to human |

## What this guarantees + doesn't

**Guarantees:**
- No outbound message reaches the wire without an explicit human
  `--for-real` invocation that names the campaign + carries the
  flag literally.
- No outbound message that the AI detector flagged can leave
  without a logged override reason.
- Optouts auto-suppress instantly; you can't accidentally re-send
  to someone who said no.

**Does NOT guarantee:**
- That a human read the draft carefully (the gate is "approved",
  not "comprehended"). Operator discipline is required.
- That the AI detector catches every problem. It catches obvious
  AI tells; it does not catch tone-deafness, factual errors, or
  inappropriate timing.
- That a human can be reached when something goes wrong at 3am.
  (The `kill-switch` `salesman halt` exists for this — invoke as
  soon as anything looks off.)

---

*This file is the authoritative spec. Behaviour drift between code
and spec is a bug; either fix the code or update the spec with a
SHIP-DECISION block in `AUDIT.md`.*
