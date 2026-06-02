# What I need from the owner

Living list of things I can't do without owner action. I update
this as items resolve / new items appear. The owner ticks them
off when they're done; I poll this file at the start of each
work session.

## Active

### B4.5b — Reverse-proxy salesman-api on https://outreach.plausiden.com/
The unsubscribe minter env (B4.5) is set, but the
`/unsubscribe` route still needs to be REACHABLE from
Gmail / Yahoo's egress IPs over HTTPS. Spin up:
1. `salesman-api` service on openclaw (cargo binary, listens
   localhost:NNNN).
2. Caddy / Nginx terminating TLS for `outreach.plausiden.com`,
   reverse-proxying `/unsubscribe` to that local port.
3. Cert via Let's Encrypt (Caddy auto-issues; Nginx needs
   certbot).
Until this is live, every send adds a List-Unsubscribe header
pointing at a 404 — Gmail/Yahoo eventually flag the domain.

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
- **PlausiDen-CRM ingest implemented** — `crm-cli listen` consumes
  salesman_event NOTIFYs and projects via three idempotent upserts
  (touch.sent / reply.received / reply.classified). Drift-tolerant:
  unknown event kinds + missing fields log + skip rather than crash.
  `crm-cli drain-once --timeout-ms N` for one-shot consume. Ready
  to deploy when there's a CRM Postgres + Salesman events to consume.

## Final-readiness checklist for first send

Sequence the operator works through to flip from "ready to draft" to
"ready to actually send for real":

```
[X] B2  — sender domain decision (outreach.plausiden.com)         ✓ done
[X] B3  — SPF + DKIM + DMARC + PTR for that domain                ✓ done
[ ] B4  — LLM credentials (API keys OR subscriber-CLI)
[ ] B4.5 — unsubscribe minter env + endpoint reachable
[ ] B5  — first 25-prospect CSV (template ready in samples/)
[ ] B5.5 — SALESMAN_TRUSTED_AUTHSERV_ID set + restart
[ ] B6  — template review pass
[ ] B7  — Vultr snapshot
[ ] B8  — Postmaster Tools + SNDS registration

Quick start (B4 + B4.5 + B5.5 in one shot):
  scp samples/salesman.env.example openclaw:/tmp/
  ssh openclaw 'sudo install -m 0640 -o root -g salesman \
                /tmp/salesman.env.example /etc/salesman.env && \
                sudo -e /etc/salesman.env'
  # Fill in the CHANGEME values; SALESMAN_TRUSTED_AUTHSERV_ID
  # is pre-filled. Save + quit.
  ssh openclaw 'sudo systemctl restart salesman && \
                /opt/salesman/bin/salesman doctor'
  # Walk down the rows; everything should be OK except (maybe)
  # IMAP if you haven't yet provisioned the salesman@plausiden.com
  # mailbox on web-01.

Then:
  ssh openclaw
  sudo -iu salesman /opt/salesman/bin/salesman doctor
  # All rows OK except (optionally) a couple of WARNs you accept.
  sudo -iu salesman /opt/salesman/bin/salesman dns-check \
      --sender-domain outreach.plausiden.com
  # Per-record OK/WARN/FAIL with remediation.
  sudo -iu salesman /opt/salesman/bin/salesman preflight \
      --campaign warmup-25
  # Verifies: signing key, unsub minter, SMTP, LLM backends, draft
  # queue, no test addresses. Prints 3 sample drafts.
  sudo -iu salesman /opt/salesman/bin/salesman send-pending \
      --campaign warmup-25
  # DEFAULT IS DRY-RUN. Eyeball the [DRY-RUN] would-send lines.
  sudo -iu salesman /opt/salesman/bin/salesman send-pending \
      --campaign warmup-25 --for-real --max-batch 5 --confirm-typed
  # First real send. Warmup gradient caps it at 5/day for the first
  # 2 days of a campaign regardless.
```

Run `salesman alerts --since-hours 24` daily after that. The 🛑
LEGAL THREAT(S) banner trumps everything else; positive replies
need same-day response; auth_failed-tagged replies need a manual
look (don't auto-suppress those).

## Resolved

### B4 — LLM credentials  *(resolved 2026-05-01, Path B subscriber-CLI)*
Subscriber-login CLIs (`claude` + `gemini`) installed system-wide on
openclaw, OAuth tokens copied to /home/salesman/.{claude,gemini}/
(owner had logged in as root; tokens are location-agnostic).
`SALESMAN_LLM_TRANSPORT=cli` set in /etc/salesman.env.
Verified end-to-end: `salesman draft` generates real personalized
cold emails via the subscriber-paid Claude Sonnet — no API
billing. See docs/SUBSCRIBER_LOGIN.md "Common gotchas" section
for the four real-world failure modes encountered + their fixes.

### B4.5 — Unsubscribe minter  *(resolved 2026-05-01)*
SALESMAN_UNSUBSCRIBE_BASE_URL=https://outreach.plausiden.com/unsubscribe
+ HMAC secret set in /etc/salesman.env.
NOTE: the salesman-api `/unsubscribe` route still needs to be
exposed on https://outreach.plausiden.com — Caddy / Nginx terminate
TLS + reverse-proxy to salesman-api on whatever local port. Until
that's live, the List-Unsubscribe header points at a 404 and
Gmail/Yahoo's bots will eventually flag the domain. Owner: spin
up the salesman-api service + reverse-proxy.

### B5 — First 25-prospect CSV  *(resolved 2026-05-01, autonomous)*
Used the new `salesman discover-llm` (LLM enumeration + homepage
HTTP validation, no Brave Search needed) to populate
`warmup-2026-05` with 25 real EU cybersecurity SMBs:
  Almond, Approach Cyber, Computest, CrowdSec, EclecticIQ,
  Enginsight, Gatewatcher, HarfangLab, HiSolutions, Hunt & Hackett,
  Intrinsec, I-Tracing, ITrust, Northwave, NVISO, r-tec, Schutzwerk,
  Sekoia.io, Synetis, SySS, Tehtris, Tesorion, Toreon, usd AG, 8com.
Then ran `find-buyers` (team-page scraper) — 14 contacts persisted,
but ~9 of them have garbage names (HTML scraping artifacts like
"All Rights Reserved", "Pentest Pentest", "Cyber Architects He").
Then `draft --product Sentinel` produced 25 personalized cold
emails — all 25 ok=2 err=0. Each draft references the prospect's
actual public signals (from homepage meta + tech_signals).

Operator review pass needed before send: drafts are in
`awaiting_approval`. Walk them with `salesman review --campaign
warmup-2026-05`. Manually fix the bad contact emails (find-buyers
guesses are visible per row); LinkedIn lookup is the easiest path
for finding real decision-maker names. Approve the ones with real
recipients; reject or hold the rest. The good auto-discovered
contacts (Gatewatcher CEO Philippe Gillet, Hunt & Hackett founder
Ronald Prins, Northwave CEO Steven Dondorp) can go as-is.

### B5.5 — Anti-spoof gate  *(resolved 2026-05-01)*
`SALESMAN_TRUSTED_AUTHSERV_ID=mail.plausiden.com` set; doctor:
`[ auth gate ] OK`.

### B3 — DNS records on outreach.plausiden.com  *(resolved 2026-05-01)*
Owner published all four records. Verified via
`salesman dns-check --domain outreach.plausiden.com --sender-ip 207.246.86.218 --expected-ptr mail.plausiden.com`:
- SPF: `v=spf1 ip4:207.246.86.218 ip6:... ~all` (warmup softfail —
  escalate to `-all` after 48h of clean sends)
- DKIM: `s1._domainkey.outreach.plausiden.com` (420-char TXT, valid)
- DMARC: `v=DMARC1; p=quarantine; rua=mailto:team@plausiden.com`
- PTR: `207.246.86.218 → mail.plausiden.com`
VERDICT: YELLOW (1 warning, 0 blockers). DKIM signing test landed
2026-05-01 — outbound is signing end-to-end.

### B2 — Sender domain decision  *(resolved 2026-05-01, implicit via B3)*
Domain locked to `outreach.plausiden.com` per the published DNS.
Sending IP is `207.246.86.218` (web-01), not openclaw's
`45.77.217.37`. The Postfix relay on web-01 signs + sends; salesman
on openclaw submits via `mail.plausiden.com:587`. Display name +
reply-to live in the env file (samples/salesman.env.example
template, sections 3 + 4).

> ⚠️ STALE (pre-2026-05-31 rebuild): openclaw was re-provisioned; its old IP
> `45.77.217.37` is dead (openclaw is now `207.148.30.162`). The web-01
> sending IP `207.246.86.218` and the relay topology above are UNVERIFIED
> after the rebuild — re-confirm the real sender on-box and re-derive
> SPF/PTR/DMARC before any send (HANDOFF.md → "RECONCILE ON-BOX", item 2).
> Intentionally not asserting a corrected sending IP here.

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
