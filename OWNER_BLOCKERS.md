# What I need from the owner

Living list of things I can't do without owner action. I update
this as items resolve / new items appear. The owner ticks them
off when they're done; I poll this file at the start of each
work session.

## Active

### B2 — Sender domain decision
Pick the actual sending domain (recommendation:
`outreach.plausiden.com` subdomain to keep reputation isolated
from main brand mail). Tell me:
- domain
- display name (e.g. "PlausiDen", "William Armstrong")
- reply-to address
Unblocks: DKIM key generation + DNS record drafting + first send.

### B3 — DNS records on the chosen sender domain
Once B2 lands, follow `docs/EMAIL_DELIVERABILITY.md`:
- SPF TXT listing 45.77.217.37
- DKIM keypair (opendkim-genkey on the openclaw VPS) + public-key TXT
- DMARC TXT (start `p=none` for reporting)
- Vultr PTR set on 45.77.217.37 to the chosen sending hostname
I can generate the exact copy-paste DNS entries once B2 lands.
Unblocks: any send that doesn't get spam-binned by Gmail/Outlook.

### B4 — LLM API keys
SSH `openclaw`; edit `/etc/salesman.env` (already templated).
Uncomment + fill:
- `ANTHROPIC_API_KEY=sk-ant-...`  (one of the two LLM keys is enough)
- `GEMINI_API_KEY=...`            (both is better — bulk vs reasoning)
- `BRAVE_SEARCH_API_KEY=...`      (optional but enables OSINT search)
Unblocks: every LLM-powered tool — draft, classify, comparison,
case-study, SEO, reply-classifier, and the agent loop itself.

### B5 — First prospect CSV
25 friendlies for the warm-up batch (companies you actually want
me to reach out to + ideally where there's some context). CSV with
header row, **required column:** `display_name`. **Optional:**
`homepage`, `industry`, `region`, `description`, `legal_name`,
`size_band`. Drop the path on the laptop and tell me.
Unblocks: discover → enrich → draft → review → send loop running
against real data.

### B6 — Template review pass
`templates/cold/*.toml` — 10 starter templates (5 segment-agnostic
+ 5 segment-specific intros + a security follow-up). Tune
`subject_seed`, `body_seed`, `forbidden_phrases`, `mandatory_phrases`
to your voice. The model uses these as STRUCTURE/TONE references,
not literal substitution; the prospect-specific content stays
LLM-generated.
Unblocks: drafts that sound like you, not like generic AI sales.

## Lower-priority / opportunistic

### B7 — Vultr snapshot
Snapshot reminder timer fires Mondays 09:00 UTC. Take one when
the email lands so we have a known-good rollback point. Once.

### B8 — Postmaster Tools registration
After B3 (DNS records live), register the sending domain with
Google Postmaster Tools (postmaster.google.com) and Microsoft
SNDS (sendersupport.olc.protection.outlook.com). Both are
free deliverability dashboards.

### B9 — Decide on LinkedIn / X / web-form channels
Default OFF (per `CLAUDE.md` hard rules). Lift only if you opt in.
Lifting requires: (a) per-account credential storage plan, (b)
TOS acceptance for browser-automation paths.

## Heads-up (info-only, no action needed)

- **PlausiDen-CRM repo created** on GitHub (private):
  https://github.com/thepictishbeast/PlausiDen-CRM
  Scaffolded with 5 crates + initial CRM schema. Subscribes to
  Salesman events via Postgres LISTEN/NOTIFY (no coupling).
- **Salesman now fires NOTIFY** on touch.sent / reply.received /
  reply.classified — CRM ingest will get sub-second updates when
  it lands. CRM downtime never breaks Salesman.

## Resolved

### B1 — Sieve classification on web-01  *(resolved 2026-04-27)*
Web-01 deployed a typed Sieve `internal_source` rule (score 100)
pinning From: @vultr.guest, @plausiden.com, @plausiden.internal,
@web-01.plausiden.internal → INBOX. Verification ping from openclaw
(queue 8407E4DF598, salesman@vultr.guest) landed in INBOX as
expected. Daily summary timer + failure alerts will reach owner
going forward. Pre-fix emails sit in Promotions/Updates folders;
owner reclassifies manually with `doveadm move` as needed.

---

*Maintained by claude-code session. Updated on every commit that
shifts blocker status.*
