# DEPLOYMENT_GUIDE.md — owner walkthrough, zero to first send

> ⚠️ **STALE INFRASTRUCTURE FACTS — verify on-box before trusting.**
> The OpenClaw VPS was re-provisioned 2026-05-31; the IP `45.77.217.37`
> referenced below is **DEAD**. The real current sending IP is
> **unverified** — see `HANDOFF.md` → "RECONCILE ON-BOX". Do NOT copy any
> IP / SPF / PTR / DMARC value from this guide into DNS; re-derive them
> from the live box first. (Intentionally not replaced with a guessed IP.)

**Audience:** the owner / operator running PlausiDen-Salesman in production.
**Goal:** get from a freshly-deployed binary on the VPS to a successful
first 25-prospect warm-up send, without spam-binning the domain.

This guide consolidates `OWNER_BLOCKERS.md`, `EMAIL_DELIVERABILITY.md`,
and `OPERATOR_HANDBOOK.md` into one ordered checklist. Read top to bottom.

---

## 0. Prerequisites

You need:

1. The VPS is reachable, postgres + redis are up, and `/etc/salesman.env` exists.
   `scripts/deploy.sh` builds and installs BOTH the `salesman` and `salesman-api`
   binaries to `/opt/salesman/bin/` (matching the systemd units' `ExecStart`).
   It does NOT create `/etc/salesman.env` — you create that file by hand
   (see §1).
2. A sender domain you control DNS for. Recommended: `outreach.plausiden.com`
   (subdomain of the brand) so the apex `plausiden.com` reputation is isolated.
3. A `salesman` Linux user. **This user must already exist** — `scripts/deploy.sh`
   does NOT create it. Create it before deploying, and never run as root.
4. **No data in production yet.** Ideally migrate on a clean DB.

If the binaries in 1 are missing, run `scripts/deploy.sh` to build and install
them. The `salesman` user (3) and `/etc/salesman.env` (1) are prerequisites you
must set up by hand before deploying.

---

## 1. Set the secrets in /etc/salesman.env

SSH to the VPS and edit `/etc/salesman.env` (mode 0640, owner `root:salesman`).

### Required for any LLM work

```sh
ANTHROPIC_API_KEY=sk-ant-...   # one of the two is enough
GEMINI_API_KEY=...             # both is better — bulk vs reasoning routes
BRAVE_SEARCH_API_KEY=...       # optional but enables OSINT search
SALESMAN_LLM_TRANSPORT=cli     # subscriber-CLI transport: spawns the operator's
                               # claude/gemini CLI, auth lives in that CLI's
                               # credential store (API keys above ignored).
                               # Omit or set =api to keep the API-key path.
                               # See docs/SUBSCRIBER_LOGIN.md.
```

### Required for sending mail

```sh
SALESMAN_FROM_NAME="PlausiDen"
SALESMAN_FROM_EMAIL=you@outreach.plausiden.com
SALESMAN_REPLY_TO=you@plausiden.com         # where humans reach you back
SALESMAN_SMTP_HOST=mail.plausiden.com
SALESMAN_SMTP_PORT=587
SALESMAN_SMTP_USERNAME=you@plausiden.com
SALESMAN_SMTP_PASSWORD=...                  # SMTP submission password
SALESMAN_COMPLIANCE_FOOTER="PlausiDen — sovereign data tools.\nReply STOP to opt out."
```

### Required for RFC 8058 one-click unsubscribe (Gmail / Yahoo bulk-sender rules)

```sh
SALESMAN_UNSUBSCRIBE_BASE_URL=https://outreach.plausiden.com/unsubscribe
SALESMAN_UNSUBSCRIBE_HMAC_SECRET=$(openssl rand -hex 32)
```

> **DO NOT skip this.** Without these two vars the salesman-api `/unsubscribe`
> route is disabled and gmail/yahoo will progressively spam-bin the domain.
> Run `salesman doctor` to verify; look for `[ unsub minter] OK`.

### Required for inbox polling

```sh
SALESMAN_IMAP_HOST=mail.plausiden.com
SALESMAN_IMAP_PORT=993
SALESMAN_IMAP_USERNAME=you@plausiden.com
SALESMAN_IMAP_PASSWORD=...
SALESMAN_IMAP_MAILBOX=INBOX
```

---

## 2. DNS records on the sender domain

Set ALL of these on `outreach.plausiden.com` (or whichever sender domain).

### a. SPF — TXT on `outreach.plausiden.com`

```
v=spf1 ip4:<SENDER_IP> -all
```

`-all` is hard-fail. Use `~all` only during the first 48h of warmup; tighten
to `-all` after the first successful send.

### b. DKIM — generate keypair on the VPS, publish public half

```sh
sudo opendkim-genkey -d outreach.plausiden.com -s s1 -b 2048 -D /etc/opendkim/keys/
sudo cat /etc/opendkim/keys/s1.txt
# Publish the contents as TXT on:
#   s1._domainkey.outreach.plausiden.com
```

### c. DMARC — TXT on `_dmarc.outreach.plausiden.com`

Start permissive for the first week:

```
v=DMARC1; p=none; rua=mailto:dmarc@plausiden.com; pct=100
```

After 7 days of clean reports, escalate to `p=quarantine`, then `p=reject`.

### d. PTR — set in Vultr panel for `<SENDER_IP>` (the real on-box sending IP)

```
mail.plausiden.com
```

(Or whatever your real public sending hostname is.)

### Verify all four

```sh
salesman doctor   # checks SMTP env + LLM backends + unsub minter
salesman dns-check --domain outreach.plausiden.com \
                   --dkim-selector s1 \
                   --sender-ip "<SENDER_IP>" \
                   --expected-ptr mail.plausiden.com
```

`dns-check` returns a per-record verdict (OK / WARN / BLOCK), exits
non-zero if any DNS record is missing or misconfigured, and gives
concrete remediation copy-paste for each gap. Replace `s1` with your
actual DKIM selector if different. Skip `--sender-ip` + `--expected-ptr`
to do DNS-only (no PTR lookup).

---

## 3. Register with provider postmaster tools

These are free and tell you when you're being spam-binned BEFORE volume drops.

- **Gmail Postmaster Tools** — https://postmaster.google.com/
  Add the sender domain, verify with a TXT, watch the dashboard for the first
  week. Spam rate >0.3% is a P0.
- **Microsoft SNDS / JMRP** — https://sendersupport.olc.protection.outlook.com/
  Add the sending IP (`<SENDER_IP>`). SNDS shows reputation; JMRP forwards
  spam complaints to you.

Both can take 24–72h to populate.

---

## 4. Generate the first prospect CSV

Pick **25** companies you actually want as customers. Keep this small —
warmup volume matters more than reach for the first week.

Use the shipped template as a starting point:

```sh
cp samples/prospects-warmup-template.csv ~/prospects-warmup-2026-04-27.csv
$EDITOR ~/prospects-warmup-2026-04-27.csv
```

CSV columns: `display_name` (required), `homepage`, `legal_name`,
`industry`, `region`, `description`, `size_band` (all optional but
filling them improves draft quality).

Validate before you ingest:

```sh
salesman validate-csv --from-csv ~/prospects-warmup-2026-04-27.csv
```

Then ingest:

```sh
salesman discover --campaign warmup-2026-04 --from-csv ~/prospects-warmup-2026-04-27.csv
```

---

## 5. Generate drafts

Pick a sequence (or use the default single-touch):

```sh
salesman draft --campaign warmup-2026-04 --product Sentinel
```

`--product` is required. This generates an LLM-written draft per prospect. They land in
`awaiting_approval` — nothing is sent yet.

Review them in the dashboard (`https://salesman.plausiden.com/drafts`) or
via the CLI:

```sh
salesman review --campaign warmup-2026-04
```

Approve only the ones you'd be proud to send. Reject the rest.

---

## 6. Run preflight

This is the gate before `--for-real`:

```sh
salesman preflight --campaign warmup-2026-04
```

It verifies:
- signing key present
- unsubscribe minter wired (HTTPS)
- SMTP env + TCP reachable
- ≥1 LLM backend
- campaign has approved drafts
- no obvious test/demo prospects in queue (acme/test/demo)
- detector ensemble: no draft scores ≥0.6
- prints 3 sample drafts for eyeball review

> **On the detector threshold:** the default differs by command.
> `approve` / `preflight` / `score` default to **0.6** — these are
> manual-gate commands where a human reviews each draft, so the bar is
> looser. The bulk commands `fact-check` / `approve-all` default to a
> **stricter 0.50**, because they act on many drafts at once with no
> per-draft human review. Pass `--detector-threshold` to override either.

The verdict is one of READY / READY-WITH-WARNINGS / BLOCKED. Don't proceed
until you see READY.

---

## 7. Test-send to yourself

Redirect the whole batch to your own address as a smoke test:

```sh
salesman send-pending --campaign warmup-2026-04 \
    --for-real --confirm-typed \
    --test-send-to you@plausiden.com \
    --max-batch 1
```

You should receive ONE email. Check:
- the From / Reply-To / Subject look correct
- the `List-Unsubscribe` header is present (Gmail "Unsubscribe" button visible)
- clicking the unsubscribe link lands on the confirmation page
- the body is well-rendered (no double newlines, no `{{handlebars}}` left)
- the `Unsubscribe: <url>` line appears in the visible footer

Reject everything else if anything looks off, regenerate, repeat.

---

## 8. Real send

```sh
salesman send-pending --campaign warmup-2026-04 \
    --for-real --confirm-typed \
    --max-batch 5
```

Cap the batch at **5/day** for the first 7 days. Volume matters less than
gradient. The output line shows `sent= bounced= errored=`. Any non-zero
`bounced` is fine — those addresses are auto-suppressed. Any `errored` ≥ 1
deserves a look at the logs.

---

## 9. Daily ops

### Monitor

```sh
salesman summary                       # funnel snapshot
salesman costs --by purpose            # which subsystem ate budget
salesman costs --by purpose --since-hours 168
salesman doctor                        # full system probe
salesman suppressions count            # do-not-contact size, by source
```

### Inbox

```sh
salesman inbox-poll --every-seconds 60     # blocks; run as systemd timer
salesman classify-replies --batch 50       # classify what came in
salesman inbox --campaign warmup-2026-04   # latest replies
```

### When things go wrong

| Symptom | Diagnose | Fix |
|---|---|---|
| `bounced=N` keeps growing | `salesman suppressions list --source bounce --limit 50` | Validate the prospect list; CSV may have typos |
| Spam complaints in Gmail Postmaster Tools | dashboard shows it | Pause the campaign. `salesman queue-clear --campaign X --confirm-typed` |
| 5.7.26 / 5.7.1 errors | `salesman doctor` | DNS is wrong: re-verify SPF / DKIM / DMARC dig. Run `salesman doctor`. |
| Gmail shows mail in spam folder | check the user's "show original" → list-unsubscribe | Verify the minter is configured; `salesman doctor` `[ unsub minter]` |
| One bad prospect should never get mail | `salesman suppressions add --target user@example.com --reason "owner blocked"` | (idempotent; the next send loop skips) |

---

## 10. Owner-action blockers cross-ref

This guide replaces, in order:

- B2 (sender domain decision) → §1 above.
- B3 (DNS) → §2 above.
- B4 (LLM keys) → §1 above.
- B4.5 (unsubscribe minter) → §1 above.
- B5 (first prospect CSV) → §4 above.
- B6 (template review) → §5 above.
- B8 (postmaster registration) → §3 above.
- R1–R4 (final readiness) → §6–§8 above.

Once §8 has executed cleanly, mark all of B2/B3/B4/B4.5/B5/B6/B8/R1–R4
as RESOLVED in `OWNER_BLOCKERS.md`.

---

## 11. What's NOT in this guide

- LinkedIn / X / web-form channels (B9) — phase 2+.
- PlausiDen-Mail integration (O1–O4) — coordinate with the web-01 mail
  Claude before scaffolding.
- AI-detector via Originality.ai (N1–N2) — costs money, defer until
  reputation is established.
- Feature work in `ROADMAP.md` — that's the engineering plan, this is
  ops.
