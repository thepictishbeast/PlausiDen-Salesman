# What I need from the owner

Living list of things I can't do without owner action. I update
this as items resolve / new items appear. The owner ticks them
off when they're done; I poll this file at the start of each
work session.

## Active

### B4 — LLM credentials (API keys OR subscriber login)
**Two paths, pick one.**

**Path A — API keys (legacy, fastest to set up):**
SSH `openclaw`; edit `/etc/salesman.env` (already templated).
Uncomment + fill:
- `ANTHROPIC_API_KEY=sk-ant-...`  (one of the two LLM keys is enough)
- `GEMINI_API_KEY=...`            (both is better — bulk vs reasoning)
- `BRAVE_SEARCH_API_KEY=...`      (optional but enables OSINT search)

**Path B — subscriber login (uses your paid Pro/Max + Gemini Advanced
seats; no per-completion billing):**
See `docs/SUBSCRIBER_LOGIN.md` for the full bootstrap. Short version:
1. As `salesman` user on openclaw, install the `claude` and `gemini`
   CLIs.
2. Run `claude login` and `gemini auth login` once each (browser-auth
   flow; tokens land in salesman's home).
3. Append to `/etc/salesman.env`:
   ```
   SALESMAN_LLM_TRANSPORT=cli
   ```
4. Restart `salesman`; `journalctl -u salesman -n 20` should show
   `registered Claude (subscriber-cli) backend` and likewise Gemini.

Path B does NOT support tool-use (CLI returns plain text). Tool-using
call sites need Path A. You can run BOTH (set transport=cli for the
drafter; tool-using paths use the API backends if their keys are set).

Unblocks: every LLM-powered tool — draft, classify, comparison,
case-study, SEO, reply-classifier, and the agent loop itself.

### B4.5 — Unsubscribe minter (RFC 8058 one-click)
On the openclaw VPS, append to `/etc/salesman.env`:
- `SALESMAN_UNSUBSCRIBE_BASE_URL=https://outreach.plausiden.com/unsubscribe`
   (must be HTTPS reachable from Gmail / Yahoo egress IPs — they fetch
   the link with a fresh client; cannot be behind basic-auth)
- `SALESMAN_UNSUBSCRIBE_HMAC_SECRET=<paste output of: openssl rand -hex 32>`
   (≥32 bytes hex or base64url; never log or echo this; rotating it
   invalidates ALL previously sent unsubscribe links so do it sparingly)
Then expose `salesman-api` on `https://outreach.plausiden.com` (it
serves `/unsubscribe` un-authed; the rest of the API stays
auth-gated).
Unblocks: Gmail + Yahoo bulk-sender compliance. Without this they
will progressively spam-bin our domain regardless of SPF/DKIM/DMARC.
Run `salesman doctor` to verify — look for `[ unsub minter] OK`.

### B5 — First prospect CSV
25 friendlies for the warm-up batch (companies you actually want
me to reach out to + ideally where there's some context). CSV with
header row, **required column:** `display_name`. **Optional:**
`homepage`, `industry`, `region`, `description`, `legal_name`,
`size_band`. Drop the path on the laptop and tell me.
Unblocks: discover → enrich → draft → review → send loop running
against real data.

### B5.5 — Anti-spoof gate env (one line, do this when MX is locked)
Once you know the hostname your inbound mail server uses to stamp
`Authentication-Results:` headers (typically your MX, e.g.
`mail.plausiden.com` for the openclaw web-01 setup), append to
`/etc/salesman.env`:

```
SALESMAN_TRUSTED_AUTHSERV_ID=mail.plausiden.com
```

Then restart and verify:
```
sudo systemctl restart salesman
/opt/salesman/bin/salesman doctor 2>&1 | grep "auth gate"
# Expect: [ auth gate   ]  OK  trusted authserv-id = `mail.plausiden.com`
```

**Why this matters:** without this, `classify-replies` cannot defend
against forged inbound replies that try to poison the suppression
list. An attacker sending `From: alice@bigprospect.com` with body
"please remove me" would auto-suppress the real Alice. With the env
set, the classifier honors the SPF/DKIM/DMARC verdict your MX
already computes.

The gate is fail-OPEN until you set this — current behavior is the
legacy "trust the From header." `salesman doctor` warns on every
run while the env is unset so the gap stays visible.

Unblocks: production-grade `send-pending --for-real` at volume.

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
