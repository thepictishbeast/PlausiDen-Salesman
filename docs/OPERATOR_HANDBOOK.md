# PlausiDen-Salesman operator handbook

This is the day-to-day operator reference. Every CLI subcommand is
documented with: what it does, env it needs, an example invocation,
common-failure-modes table, and pointers.

For the underlying architecture, see `PLAN.md`. For the phased
delivery order, see `ROADMAP.md`. For first-time deployment, see
`docs/EMAIL_DELIVERABILITY.md`.

## Quick reference — the daily workflow

```bash
# Once
salesman migrate                                    # apply schema

# Per campaign — operator flow
salesman discover --campaign cyber-smb --from-csv prospects.csv
salesman enrich   --campaign cyber-smb
salesman draft    --campaign cyber-smb --product Sentinel
salesman review   --campaign cyber-smb              # eyes on each
salesman approve  --touch <uuid>                    # per touch
salesman send-pending --campaign cyber-smb          # DRY-RUN by default
salesman send-pending --campaign cyber-smb --for-real

# Per-batch ops
salesman classify-replies                           # if not on systemd timer
salesman audit                                      # verify chain
salesman summary --since-hours 24
salesman costs   --since-hours 24
salesman status                                     # JSON health
```

## Required environment

`/etc/salesman.env` on the VPS holds these. Mode 0640 root:salesman.

| Variable | Required for | Notes |
|---|---|---|
| `SALESMAN_DATABASE_URL` | everything that touches state | `postgresql:///salesman?host=/var/run/postgresql` for VPS |
| `ANTHROPIC_API_KEY` | draft / classify / comparison / case_study | one of two LLM keys must be set for any LLM op |
| `GEMINI_API_KEY` | bulk classify, grounded search | cheap default for high-volume |
| `BRAVE_SEARCH_API_KEY` | discovery.brave_search tool | optional; OSINT works without it |
| `SALESMAN_SMTP_HOST` / `_PORT` / `_USERNAME` / `_PASSWORD` | send-pending --for-real | SMTP relay credentials |
| `SALESMAN_FROM_NAME` / `_FROM_EMAIL` | send-pending --for-real | sender identity (per ADR-0003) |
| `SALESMAN_REPLY_TO` | optional reply routing | else From= is used |
| `SALESMAN_LIST_UNSUBSCRIBE` | RFC 8058 one-click | mailto: or https:// URL |
| `SALESMAN_COMPLIANCE_FOOTER` | every body footer | physical address + opt-out per CAN-SPAM |
| `SALESMAN_IMAP_HOST` / `_PORT` / `_USERNAME` / `_PASSWORD` | inbox-poll | only port 993 (TLS) supported |
| `SALESMAN_IMAP_MAILBOX` | inbox-poll | default `INBOX` |
| `SALESMAN_API_BIND` | salesman-api | default `127.0.0.1:8080` |
| `SALESMAN_API_BASIC_AUTH` | salesman-api auth | format `user:pass`; if absent, auth is OFF |
| `SALESMAN_TEMPLATES_DIR` | draft with template_key | path to `templates/cold/` |

---

## Subcommand reference

### `migrate`
Applies pending schema migrations.

```bash
salesman migrate
```

Run after every deploy. Idempotent. If you see "migrations applied
(or already current)" it succeeded.

**Common failure modes:**

| Symptom | Cause | Fix |
|---|---|---|
| `migration X was previously applied but has been modified` | Migration file changed after being applied | Recompute sha384 of the file; UPDATE `_sqlx_migrations.checksum` |
| `database does not exist` | Wrong URL, or salesman db not created | Create db: `sudo -u postgres createdb -O salesman salesman` |

### `discover`
Ingest a CSV of companies into a campaign.

```bash
salesman discover \
  --campaign cyber-smb \
  --from-csv prospects.csv \
  --goal "land 10 cyber SMB clients in Q3" \
  --segment "US/EU SMB security teams"
```

CSV must have a header row. **Required column:** `display_name`.
**Optional:** `homepage`, `industry`, `region`, `description`,
`legal_name`, `size_band`.

Idempotent — re-running with the same CSV is safe.

### `enrich`
Fetches each company's homepage and writes back title /
meta-description / 70 tech-stack fingerprints.

```bash
salesman enrich --campaign cyber-smb --concurrency 4
```

`--concurrency` defaults to 4 (don't hammer one host).

### `draft`
Generates a cold-email draft per prospect using the LLM router.
Requires at least one LLM key set.

```bash
salesman draft \
  --campaign cyber-smb \
  --product Sentinel \
  --angle-hint "lead with sovereign-data angle"
```

Drafts land in `awaiting_approval`. Will not auto-send.
`--skip-existing` (default true) avoids re-drafting prospects
already in the queue.

### `review`
Lists awaiting-approval drafts in a campaign.

```bash
salesman review --campaign cyber-smb
```

Shows touch id, company, subject, full body for each.

### `approve` / `reject`
Move one draft from `awaiting_approval`.

```bash
salesman approve --touch <uuid>
salesman reject  --touch <uuid>
```

Approve runs the AI-detector gate first (default threshold 0.6).
If detector flags the draft, refuses approval. Override with:

```bash
salesman approve --touch <uuid> \
  --detector-threshold 0.7 \
  --force-override "operator-reviewed; AI-tells acceptable in this context"
```

Override is logged at WARN with the reason for audit.

### `suppress`
Add an email or domain to the never-contact list.

```bash
salesman suppress --target someone@example.com --reason "explicit opt-out"
salesman suppress --target spammer-domain.example --kind domain
```

`--kind` auto-detects from `@` if omitted.

### `send-pending`
Sends approved drafts via SMTP. **DEFAULT IS DRY-RUN.**

```bash
# Dry-run: see what would happen
salesman send-pending --campaign cyber-smb

# Real send — recommended invocation for first send / new domains
salesman send-pending --campaign cyber-smb --for-real --confirm-typed \
  --max-batch 10 \
  --ack-new-domains 5

# Production send (after warmup, established domains)
salesman send-pending --campaign cyber-smb --for-real \
  --per-recipient-window-hours 720 \
  --per-recipient-max 5 \
  --per-domain-window-hours 1 \
  --per-domain-max 10 \
  --max-batch 25 \
  --ack-new-domains 10
```

**Reputation safeguards (layered, all active by default):**

| Flag | Default | What it does |
|---|---|---|
| (no `--for-real`) | dry-run | Logs what WOULD happen; no SMTP |
| `--max-batch N` | 25 | HARD ceiling on touches sent in one invocation |
| `--ack-new-domains N` | 10 | Refuses send if more than N domains never previously touched |
| `--per-recipient-max N` | 5 | Per-recipient touch cap |
| `--per-recipient-window-hours N` | 720 (30d) | Window for per-recipient cap |
| `--per-domain-max N` | 10 | Per-domain touch cap |
| `--per-domain-window-hours N` | 1 | Window for per-domain cap |
| `--confirm-typed` | off | Operator must TYPE campaign name to proceed |
| `--no-pause` | off | Skip 5s pre-send pause (CI/scripts only) |
| `--test-send-to <addr>` | off | Send EXACTLY ONE message redirected to `<addr>` for proof; touch stays `approved` for the real run |

A pre-flight summary prints BEFORE any SMTP work — review it
carefully. After 5 seconds (or after typed confirmation if
`--confirm-typed`), sending begins. The full reputation gate
spec is in [`HUMAN_IN_THE_LOOP.md`](../HUMAN_IN_THE_LOOP.md).

For each touch:
1. Resolve to-address from primary contact
2. Suppression check (skip + log if hit)
3. Per-recipient + per-domain rate-cap check
4. (real) SMTP send via lettre with List-Unsubscribe header
5. (real) Sign event + insert receipt + mark touch sent

### `inbox-poll`
Poll IMAP for new replies.

```bash
salesman inbox-poll                        # once
salesman inbox-poll --every-seconds 300    # forever, every 5min
```

Persists each new message as a `replies` row with `kind=unclassified`.
Threading: matches reply.from_address to a prospect's primary contact.
No match → reply dropped + warned (not your prospect).

The `salesman-classify.timer` runs `classify-replies` every 10 min,
so you typically don't run this manually unless debugging.

### `classify-replies`
LLM-classifies pending replies and applies funnel-state transitions.

```bash
salesman classify-replies --batch 50
```

For each reply:
- LLM classifies into Engaged / Question / Objection / Optout /
  OutOfOffice / Bounce / Spam
- Heuristic keyword check ALSO runs; either signal forces optout
- On Optout: instant suppression + sequence pause + alert
- On Bounce: contact.email_verified = FALSE
- On Engaged/Question: prospect → engaged

### `inbox`
Show recent classified replies for a campaign.

```bash
salesman inbox --campaign cyber-smb --limit 50
```

### `define-sequence`
Create a multi-touch sequence from a TOML file.

```bash
salesman define-sequence \
  --campaign cyber-smb \
  --name "v1-introduction-3-touch" \
  --from-toml sequence.toml
```

TOML schema: `[[steps]]` with `template_key`, `channel`, `delay_days`.

### `assign-sequence`
Assign every prospect in a campaign to a sequence at step 0.

```bash
salesman assign-sequence --campaign cyber-smb --sequence v1-introduction-3-touch
```

### `tick-sequences`
For every prospect whose `next_due_at` has passed, generate the
next draft (using the step's template_key) and advance the sequence.

```bash
salesman tick-sequences --batch 100 --product Sentinel
```

### `summary`
Pipeline counts + N-hour activity.

```bash
salesman summary --since-hours 24
```

Emitted by `salesman-summary.timer` daily at 09:00 UTC.

### `costs`
LLM cost report by (backend, model) over a window.

```bash
salesman costs --since-hours 168    # last 7 days
```

Shows: calls, prompt/output/cache tokens, USD cost, avg + p95 latency.

### `audit`
Verify the receipt chain. Loads the signing key, recomputes hashes,
reports OK / FAIL per receipt.

```bash
salesman audit --limit 100
```

If any receipt shows BAD, the chain has been tampered with —
investigate immediately.

### `status`
JSON health probe. Exits non-zero if any required component is down.

```bash
salesman status
```

Reports: db reachable, LLM backends registered, signing key present,
SMTP/IMAP env presence.

### `doctor`
Comprehensive human-readable diagnostic. Strong superset of `status`
with optional connection probes.

```bash
salesman doctor
salesman doctor --probe-smtp --probe-imap   # actually try the connections
```

Per-check OK/WARN/FAIL lines with a final GREEN/YELLOW/RED verdict.
Exit 1 on RED. Useful as the FIRST thing to run after editing
`/etc/salesman.env`, or in a daily cron.

### `render-site`
Markdown → static HTML with index + sitemap.

```bash
salesman render-site \
  --src docs/comparisons \
  --dst /opt/salesman/data/site \
  --origin https://plausiden.com \
  --site-name "PlausiDen"
```

### `tools` / `backends`
List the registered tools and LLM backends.

```bash
salesman tools
salesman backends
```

### `halt`
Stub. Will pause every active campaign in Phase 1.4.

---

## Operational runbook

### Daily

- Read the 09:00 UTC summary email
- If anything looks off, `salesman status` + `salesman audit`

### Weekly

- Take the Vultr snapshot when the snapshot-reminder email lands
- Skim the cost report: `salesman costs --since-hours 168`
- Review awaiting-approval queue: `salesman review --campaign <name>`

### Incident: SMTP failures

- `journalctl -u salesman-classify --since 1h`
- Check Postmaster Tools dashboard for reputation flap
- Pause sequence: `salesman halt --reason "investigating bounces"`
  (when implemented; for now manually `UPDATE campaigns SET status =
  'paused' WHERE id = '<id>'`)

### Incident: receipt audit shows BAD

- Stop all sends immediately
- Diff the suspect receipt's content against what the touches /
  llm_calls tables show for that timestamp
- If genuine tampering: rotate the signing key + investigate
  filesystem access on the VPS

---

## Where things live

| Concern | Path |
|---|---|
| Schema migrations | `crates/salesman-state/migrations/` |
| Cold-email templates | `templates/cold/*.toml` |
| Comparison pages | `docs/comparisons/*.md` |
| Operational scripts | `scripts/` |
| Audit ledger | `AUDIT.md` |
| Decisions | `docs/decisions/*.md` |
| Deliverability runbook | `docs/EMAIL_DELIVERABILITY.md` |
| This handbook | `docs/OPERATOR_HANDBOOK.md` |
| VPS env | `/etc/salesman.env` (mode 0640) |
| VPS binary | `/opt/salesman/bin/salesman` |
| VPS data | `/opt/salesman/data/` |
| VPS backups | `/opt/salesman/data/backups/` |
| VPS logs | `journalctl -u salesman-*` |
| Signing key | `/opt/salesman/config/signing.seed` (mode 0600) |
