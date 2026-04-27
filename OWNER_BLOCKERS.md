# What I need from the owner

Living list of things I can't do without owner action. I update
this as items resolve / new items appear. The owner ticks them
off when they're done; I poll this file at the start of each
work session.

## Active

### B1 — Email forwarding to a real inbox  *(in progress, other Claude on web-01 is fixing)*
Status: messages successfully Saved to `/var/mail/vhosts/plausiden.com/william/`
on web-01 (mail.plausiden.com), but owner reads mail elsewhere.
Either set up a Postfix virtual alias forward to Gmail, OR have
owner read the Dovecot mailbox via IMAP/webmail.
Verifies: I send a test email; owner replies "got it."
Unblocks: every other communication channel. Until this is fixed,
all my emails (including daily summaries + alerts) accumulate
unread.

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

## Resolved

(none yet — items move here once owner confirms done)

---

*Maintained by claude-code session. Updated on every commit that
shifts blocker status.*
