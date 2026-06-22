# PRODUCT_VISION.md — what Salesman is, who it's for, how it's sold

Anchor doc for product decisions. When in doubt about whether to build a
feature, ask: does this advance the **single sentence** below?

> Salesman is the AI client-acquisition engine you can audit, you can
> self-host, and that requires a human's signed approval for every send.

The PRIMARY purpose is **getting PlausiDen new clients**. Cold outbound
is the headline workflow. But the whole acquisition stack supports it:
market intelligence, OSINT, CRM, content (comparison pages / case
studies), feedback loops (reply classification → funnel transitions),
and reputation guardrails. Each of those is a feature in service of
the same north star: **fewer, better, signed conversations with the
right prospects**.

---

## What Salesman is (one paragraph)

A self-hosted, AI-driven cold-outreach platform. It ingests prospect
lists, enriches them with public OSINT, drafts personalized first-touch
emails using your choice of SaaS LLM (Claude / Gemini; a fully-local
model is deferred — see ADR-0003), and sends them through your own
SMTP — but only after a human reviews and
approves each draft. Every send is signed with a per-org Ed25519 key
into a hash-linked receipt chain (per-receipt signatures are
tamper-evident; detecting end-of-chain truncation needs an external
anchor — see docs/AUDIT_CHAIN.md). Replies come back via IMAP, get
classified, and update the funnel. Bounces and opt-outs auto-suppress
forever. Compliance is by-construction, not by-policy: the receipt chain
is replayable, the suppression list is exportable, the unsubscribe
endpoint is RFC 8058 one-click.

It is not a SaaS that holds your customer relationships. The data is
yours, on your hardware, encrypted at rest. Drafting/reply use SaaS
Claude/Gemini, but prospect PII (email, phone, company name, homepage)
is redacted before the call and rehydrated after — a redaction boundary
that keeps PII off third parties in the clear (residual free-text names
are an accepted v1 limitation; see docs/PII_REDACTION_BOUNDARY.md).

## Who it's for (Ideal Customer Profile)

- **5-50 person B2B teams** with security, compliance, devtools, or
  data products. Big enough to need outbound; small enough to care about
  brand reputation on every send.
- **SOC 2 / ISO 27001 / GDPR-bound** orgs that have to *prove* what
  they sent and to whom. The receipt chain is auditor-friendly.
- **Sovereignty-conscious shops** — EU companies that don't want their
  customer data in a US SaaS, government contractors, finance, health.
- **Anti-spam by-conviction founders** who refuse Outreach.io / Apollo
  because those tools normalize bad practices.

## Who it's NOT for

- Spammers. Bulk blasters. "Growth hackers." Anyone who measures success
  in volume.
- Anyone who wants a SaaS to handle their relationship for them.
- Anyone whose first question is "how many emails per day."

---

## What it does today (capabilities checklist)

### Discovery
- CSV ingest with column validation
- Homepage scrape with 77 tech-stack fingerprints
- Email pattern guesser (first.last, flast, etc.)
- Brave Search adapter for query-based discovery

### Enrichment / OSINT
- DNS info (A / MX / TXT / NS — provider inference)
- GDELT (recent news mentioning the company)
- GitHub org (public repos, stars, languages)
- HackerNews search (mentions of the company / founder)
- Wikipedia summary
- Wayback Machine (historical homepage snapshots)

### Content
- Draft cold email (LLM-driven, JSON-output, detector-gated, retry loop)
- Reply classifier (engaged / question / objection / optout / OOO /
  bounce / spam / unclassified)
- Comparison page generator (us vs. competitor, honest)
- Case study draft (refuses to invent details)
- SEO meta-tag generator (title + description + JSON-LD Article)
- Static-site renderer (markdown → HTML with consistent styling)

### Outreach
- SMTP send via lettre with TLS
- RFC 8058 one-click unsubscribe (HMAC-SHA256 per-recipient tokens)
- Sync hard-bounce auto-suppress (5.1.1, 5.1.2, 5.1.6, 5.1.10, 5.4.4, 5.7.27)
- Async DSN bounce auto-suppress (RFC 3464 heuristic detection)
- Per-domain bounce-rate quarantine (3 hard bounces / 24h → soft pause)
- Sender warmup gradient (5/d days 1-3, 10 days 4-7, 25 days 8-14, 100+)
- Per-recipient + per-domain rate caps
- New-domain quota per batch (--ack-new-domains)
- AI-detector ensemble gate before approval (12 heuristics, expandable)
- Test-send-to: redirect-one-message smoke test
- Receipts: Ed25519-signed, hash-chained, replayable (truncation needs an external anchor — see docs/AUDIT_CHAIN.md)

### Reply ingest
- IMAP poll over TLS
- mail-parser RFC 5322 normalization
- DSN short-circuit before regular reply path
- LLM classifier with keyword-optout safety net

### Operator surface
- `salesman doctor` — full system probe
- `salesman dns-check` — SPF/DKIM/DMARC/PTR verifier
- `salesman preflight --campaign` — per-campaign go/no-go gate
- `salesman validate-csv` — pre-ingest sanity
- `salesman whoami` — sender identity audit
- `salesman summary` — funnel snapshot (text or JSON)
- `salesman alerts` — triaged digest of important recent activity
- `salesman costs --by purpose|model` — LLM spend report
- `salesman suppressions {list,add,remove,export,import,count}`
- `salesman audit` — recent receipt verification
- `salesman audit-chain` — end-to-end hash-chain integrity attest
- `salesman queue-clear --confirm-typed` — bulk reject
- `salesman approve-batch --confirm-typed` — bulk approve

### Audit / governance
- Per-call LLM cost ledger (backend, model, purpose, tokens, latency)
- Per-campaign cost cap (UPDATE / SELECT in state)
- Receipt chain (Ed25519) with daily systemd timer attest
- Suppression NOTIFY → CRM projection (cross-repo)
- All migrations, all secrets in `/etc/salesman.env` mode 0640

### Production deploy
- 6 systemd units (api / inbox-poll / classify / audit-chain / daily / doctor-watch) with full
  defence-in-depth lockdown (NoNewPrivileges, ProtectSystem=strict,
  PrivateTmp, MemoryDenyWriteExecute, ReadWritePaths-pinned)
- Binaries (`salesman` + `salesman-api`) installed to `/opt/salesman/bin/` by `scripts/deploy.sh`
- CI: cargo check / test / fmt / clippy / detector corpus / template
  bench / docs sanity / CLI --help smoke / shellcheck

---

## What it should do next (roadmap)

### Tier 0 — be exceptional at cold-selling (the headline)

These are the features that turn "tool that sends emails" into
"tool that closes deals". The two halves are inseparable:

1. **Reputation protection** — RFC 8058 unsubscribe, sync + async
   bounce auto-suppress, sender-warmup gradient, per-domain
   bounce-rate quarantine, AI-detector ensemble gate, signed
   receipts, suppression CRUD, daily audit-chain attest. ALL
   already wired and continuously enforced.
2. **Cold-selling quality** — the features below. Reputation safety
   is necessary but not sufficient. Quality of output is what wins.

The bar: every email this system sends should be one a senior B2B
operator would be proud to put their name on, AND that the recipient
has a verifiable, audit-grade way to opt out of forever.

- **Research engine** (`salesman-research`): given a prospect, run
  ALL the OSINT adapters in parallel (GDELT / GitHub / HN / Wikipedia
  / Wayback / DNS / homepage), aggregate into a single "anchor facts"
  bundle ranked by recency + relevance. Feed top-3 anchors to the
  drafter as REQUIRED inputs — draft must reference at least one
  anchor or the gate fails. This is the personalization quality
  multiplier.
- **Auto-angle selection**: given prospect facts + sender's product
  catalog, the LLM picks the BEST product to pitch + the BEST angle
  (compliance / cost / speed / segment-specific). Operator can
  override but the default is auto.
- **Subject-line A/B harness**: every campaign gets two subject
  variants. Reply rate per variant is tracked. After 50 sends per
  variant, the loser auto-deprecates.
- **Multi-turn reply drafting**: when a reply lands as
  `question` / `objection`, draft a great REPLY for the operator to
  approve. Today we classify — tomorrow we draft the response too.
  The conversation is the relationship; auto-drafting it is the win.
- **Send-time optimization**: per-prospect (or per-segment), learn
  which hour-of-day / day-of-week produces the highest reply rate.
  Default to 09:00 + 14:00 local on Tue/Wed/Thu; adjust as data
  accumulates.
- **Intent signals**: ingest "this prospect just hired a CISO" /
  "they just raised a Series A" / "their product just launched" as a
  tag on the prospect. Boost priority when fresh; cool when stale.
  Today: CSV column. Tomorrow: API integrations (Crunchbase, Owler,
  WhoFunded).
- **Adaptive cadence**: open-without-reply gets a different follow-up
  than no-open-no-reply. Reply that asks a question gets answered
  fast; objection gets a different track than question.
- **Per-segment template benchmarking**: which template wins for
  security CISOs vs devops engineers vs data leaders? The system
  knows; the operator gets a per-segment "your best template is X
  with Y% reply rate" each week.
- **Quality-regression analytics**: track reply rate per (backend,
  model, purpose) tuple. Detect "Gemini-Flash drafts get 2x worse
  reply rate than Claude Opus" and surface it.
- **Voice-memo ingest** (mobile workflow): record a 30-second memo
  → Whisper → LLM → draft. The owner's gut intuition about a
  prospect becomes a real first-touch in 60 seconds.

### Tier 1 — model resilience + ops polish (this commit + next)
- **Touch-tagging**: record (backend, model) on every drafted touch so
  the operator can see which model produced what. Foundation for
  quality-regression analytics + Tier 0 reply-rate-by-model metric.
- **Backend-health gate**: preflight + send-pending refuse REAL send
  when any required LLM backend is degraded / rate-limited. Sensitive
  ops only when fully functional.
- **Operator-brief preamble**: a SALESMAN_OPERATOR_BRIEF env var
  pointing at a file that gets prepended to every system prompt. Keeps
  swapped-in fallback models tone-aligned.
- **Daily summary email**: cron-fired email to the operator at 09:00
  local with prospects-added / sent / replied / costs / blockers.
- **Slack/Discord webhook**: alerts pipe to a webhook on positive
  replies + bounce-rate spikes. Operator sets `SALESMAN_ALERT_WEBHOOK`.

### Tier 2 (operator-friction kill)
- **Sequencing analytics**: which template-step generates the most
  replies? Auto-pause weakest performers.
- **A/B template harness**: split a campaign across two variants and
  report a confidence-weighted winner.
- **Per-prospect "next action"**: heuristic suggestion ("send follow-up
  in 3 days" / "give up" / "wait for reply") with rationale.
- **Mobile-friendly approval UI**: the dashboard gets a phone-shaped
  layout for one-handed review on the go.
- **Voice-memo to draft**: record a 30s memo, get a draft. Whisper +
  LLM. (Defer until LFI is wired as a backend.)

### Tier 3 (sovereign-stack)
- **LFI as third LLM backend** — local model, no SaaS dependency.
- **Public-page enrichment**: LinkedIn-without-LinkedIn — find founders
  + roles via crawl4j-style public-page scraping.
- **Calendar integration**: parse a reply asking for a meeting time and
  propose 3 slots from the operator's published calendar.
- **Per-customer model-budget cap** with soft + hard gates.

### Tier 4 (multi-tenant SaaS)
- Hosted-tier deployment (per-customer Postgres, per-customer keys)
- White-label dashboard
- Per-customer KMS for sealed receipts
- SOC 2 / GDPR data-flow attestation generator
- Customer-facing "your suppression status" page (transparency)

### Tier 5 — beyond cold sales (same engine, broader use cases)

The same architecture (prospects + signed touches + classified replies
+ receipt chain) extends naturally to anything that's "personalized,
audit-grade, opt-in correspondence with a list of named entities."

**Customer success after the close**
- Funnel state `customer` post-deal; same touch + reply + receipt machinery
- NPS / CSAT cycle: a template that asks one question; classify replies
- Churn prevention: signals from inbox + product usage feed disengagement detection

**Account-based marketing**
- Multi-stakeholder coordinated outreach: when prospect at company X
  engages, auto-queue parallel touches at the CISO + CTO of the same
  company, each personalized to their role
- Per-account playbook + shared receipts
- Roll-up dashboard: deals-by-account-not-just-by-prospect

**Market intelligence (the "MI" arm)**
- Competitor monitoring: poll competitors' homepage / pricing / docs /
  blog / GitHub on a daily timer; diff and surface changes
- Brand monitoring: GDELT + HN + Reddit search for your own company
  name; sentiment-classify + alert
- SEO opportunity: ingest search-volume data; map to comparison-page
  topics; auto-draft outlines
- Funding-event ingest (Crunchbase / Owler) → mark prospects "fresh
  budget" and bump priority

**Content marketing arm** (already wired)
- Comparison-page generator (us vs. competitor, honest)
- Case study draft (refuses to invent details)
- SEO meta-tag generator + JSON-LD Article schema
- Static-site renderer (markdown → HTML)
- Future: blog-post draft from a 5-bullet outline; podcast show-notes;
  conference-talk transcript-to-blog

**Referral + advocacy**
- After `won`, fire a referral-ask template: "would you intro us to 2
  similar companies?"
- Auto-detect customer-quote candidates from positive replies; flag
  for case-study sourcing

**Trial-to-paid**
- Sequence specifically for trial users; classify replies for purchase
  intent vs. churn risk
- Drip cadence adapts to engagement signal

**Hiring** (same engine, different goal)
- Reuse the prospect/touch/reply machinery for candidate outreach
- Templates targeting eng/sales/CS roles
- Reputation safeguards apply (no spamming candidates either)

**Conference + event prep**
- Ingest attendee list CSV
- Run full OSINT pass against everyone
- Generate per-person talking-points sheet for the booth

**Operator productivity**
- Voice-memo to draft (mobile)
- Calendar integration (auto-suggest meeting times in replies)
- Slack/Discord webhook on positive replies (alert what matters)
- Daily summary email (briefing at 09:00 local)
- Per-prospect "next action" recommendation

**Compliance / governance**
- SOC 2 / GDPR data-flow attestation auto-generator
- Customer-facing "your suppression status" lookup page
- Per-tenant KMS-sealed receipts (Tier 4)

The point: **one engine, many surfaces.** As long as a use case is
"named entities + personalized correspondence + audit trail + reputation
discipline," Salesman handles it. The reputation-protection layer is
the moat: no other tool gives you the right to send AND the proof of
what you sent.

---

## How an operator actually uses it

### First-time setup (≤2h, mostly DNS waits)
1. Buy or pick a sender domain (e.g. `outreach.plausiden.com`).
2. SSH the VPS, set 9 env vars in `/etc/salesman.env`.
3. Set 4 DNS records (SPF / DKIM / DMARC / PTR).
4. `salesman dns-check` — green.
5. Register with Gmail Postmaster Tools + Microsoft SNDS.
6. `salesman doctor` — green.

### Daily (≤10 min)
1. Morning: read the daily summary email or run `salesman alerts`.
2. Approve / reject any drafts in the queue (CLI or web UI).
3. `salesman preflight --campaign foo` then `send-pending --for-real`.
4. Glance at `salesman costs --by purpose` weekly.

### Recovering from problems
- Bounces spike: `salesman suppressions list --source bounce --limit 50`
  → check whether the prospect list was junk or a domain is tarpiting.
- Spam complaints in Gmail Postmaster: pause the campaign with
  `queue-clear --confirm-typed`, audit the templates, regenerate.
- Suppression added in error: `salesman suppressions remove --target X
  --confirm-typed` (typed-name confirm; the recipient WILL receive
  future sends).

---

## How clients should perceive + interact with it

### What they SEE on the receiving end
- A short, specific, personalized email from a real person at a real
  company with a verifiable domain.
- A `List-Unsubscribe` header that one-click works in Gmail / Apple
  Mail / Outlook bulk-sender views.
- A visible "Unsubscribe: <url>" line in the footer for older mail
  clients.
- A reply-to that goes to a real human, not a no-reply.
- A receipt-link they can request to see exactly when the message was
  sent + by whom (audit-grade transparency — future tier 4 feature).

### What they CAN'T see
- The data they shared isn't being aggregated and resold.
- Their address isn't on a 50,000-person blast list.
- Their reply doesn't fall into a hosted SaaS where any employee can
  read it.

---

## How we sell it

### The pitch (45 seconds)
> "If you've ever signed a SOC 2 audit and worried about how to defend
> your outbound email tooling, Salesman is built for that conversation.
> It's a self-hosted Rust binary. Drafting uses SaaS Claude/Gemini, but
> prospect PII (email, phone, company name, homepage) is redacted before
> the call and rehydrated after — a redaction boundary, so PII doesn't
> leave your box to third parties in the clear. Every email is signed
> with a per-org Ed25519 key into a hash chain you can replay. Every
> send needs a human to type their approval.
> RFC 8058 one-click unsubscribe is wired by default. Bounces auto-
> suppress, opt-outs auto-suppress, and a recipient ends up on the
> do-not-contact list within 60 seconds of a STOP reply. We made this
> tool because we needed it."

### Anti-positioning ("we are not")
- Not Outreach.io. Not Apollo. Not Lemlist. Not Mailshake.
- Not for spammers. Not for blast lists. Not for "growth hackers."
- Not a SaaS holding your relationships hostage.
- Not a black-box LLM that you can't audit.

### Pricing model (proposed; owner picks)
- **Tier 0 (FREE / open source)**: self-hosted, single org, all features.
- **Tier 1 (~$500/mo)**: hosted version + premium support + custom
  template review. Target: SMB SaaS that wants the brand promise but
  doesn't want to run the VPS.
- **Tier 2 (~$3000/mo)**: white-label, multi-tenant per-customer keys,
  per-customer KMS, SOC 2 / GDPR attestation generator. Target:
  consultants / agencies running outbound for multiple clients.
- **Premium support / training / custom adapters**: hourly.

### Distribution channels
- Open-source repos (already public on GitHub: thepictishbeast/PlausiDen-*)
- Hacker News launch when the first 25-prospect warm-up has produced
  measurable engagement
- Show HN: "Built our own self-hosted outbound tool because we couldn't
  audit Outreach"
- Direct: SOC 2 auditors, GDPR-aware DPOs, security-conscious B2B SaaS
  founders. They're already paying $500/mo for tools they don't trust.

### Existential differentiator
**Plausible Deniability.** The sender can prove what they sent, when,
and to whom — and equally, the recipient (or a regulator) can verify
the same. Most SaaS sales tools can't even tell you reliably if a
specific email was actually delivered. Salesman keeps a signed receipt
chain as a side-effect of normal operation; that artifact is the
trust anchor.
