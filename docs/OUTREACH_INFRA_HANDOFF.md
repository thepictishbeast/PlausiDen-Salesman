# Outreach infrastructure — handoff to Prime

> **Ownership:** Prime owns the `plausiden.com` website, mail host, and DNS.
> Salesman does **not** make DNS or mail-server changes. This document is the
> *requirements spec* Salesman hands to Prime so cold outreach is authenticated,
> deliverable, and compliant. Record syntax lives in
> [`DEPLOYMENT_GUIDE.md`](./DEPLOYMENT_GUIDE.md) §a–d and
> [`EMAIL_DELIVERABILITY.md`](./EMAIL_DELIVERABILITY.md); this doc says *who owns
> what* and *what to publish*, not how to format a TXT record.

## 0. The constraint to internalize first

Anonymity in PlausiDen is for the **prospect's data** (sovereign storage, local
LFI generation, no PII to third parties) — **not for the sender of commercial
mail.** CAN-SPAM / CASL / PECR require an **identifiable sender, a real physical
postal address, and a working opt-out**. SPF / DKIM / DMARC / rDNS are
*attribution* mechanisms by design; mailbox providers reject mail they can't tie
to a consistent identity. "Anonymous sender" and "good deliverability" are
opposites — so the outreach identity is **proudly attributable**. We win on
privacy through **transport security + data sovereignty + verifiable provenance**,
never by hiding who sent the mail.

## 1. Does any DNS record point at the Salesman VPS? — Mostly **no** (by design)

Salesman runs **sandboxed on its own VPS, isolated from Prime**. Pointing public
DNS at that box would breach the isolation that is the whole reason it is
sandboxed. Recommended topology (best-of-all-worlds: isolation + deliverability):

```
  Internet
     │  (cold mail, replies, unsubscribe clicks)
     ▼
  PRIME  ── mail host (Postfix/Dovecot/OpenDKIM) + web/reverse-proxy
     │        · owns outreach.plausiden.com DNS + SPF/DKIM/DMARC/PTR
     │        · reverse-proxies /unsubscribe + provenance link to Salesman
     ▼  (internal network / private link only — no public DNS)
  SALESMAN VPS  ── composes drafts, queues sends, serves the salesman-api
                   /unsubscribe + receipts endpoint on the INTERNAL interface
```

- **Outbound mail egresses through Prime's relay**, so SPF/DKIM/DMARC/PTR all
  reference **Prime's** mail host — one warmed, reputable identity.
- **No public A/AAAA record points at the Salesman box.** Prime's reverse proxy
  forwards `/unsubscribe` (and the verifiable-provenance link) to Salesman over
  the internal network. Salesman stays unreachable from the public internet.

> **The one real architecture decision for the owner/Prime** (specific, not a
> pile of questions): *Does outbound mail egress directly from the Salesman VPS,
> or relay through Prime's Postfix?* Recommendation: **relay through Prime** (it
> keeps Salesman isolated and consolidates sender reputation). If instead the
> Salesman VPS sends directly, then its public egress IP needs PTR + must be in
> SPF — confirm that IP **on-box** (see §3) and tell Salesman.

## 2. Records Prime must publish for `outreach.plausiden.com`

Sending subdomain is already chosen: **`outreach.plausiden.com`** (never the apex
`plausiden.com` — a reputation hit on outreach must never poison the brand domain).

| Record | Host | Purpose |
|--------|------|---------|
| **SPF** TXT | `outreach.plausiden.com` | authorize Prime's mail-host IP(s); end `-all` |
| **DKIM** TXT | `<selector>._domainkey.outreach.plausiden.com` | public key; Prime's OpenDKIM signs |
| **DMARC** TXT | `_dmarc.outreach.plausiden.com` | start `p=none` + `rua=`/`ruf=` reporting, ratchet to `p=quarantine`→`p=reject` once clean |
| **PTR / rDNS** | the sending IP | forward-confirmed (FCrDNS): PTR → `mail.outreach...`, and that name A's back to the IP |
| **A/AAAA** | `outreach.plausiden.com` | → **Prime's** web/reverse-proxy host (serves `/unsubscribe`), **not** the Salesman box |
| **MX** | `outreach.plausiden.com` | so bounce/reply DSNs are receivable |
| mailbox | `postmaster@` + `dmarc@` | RFC 5321; some receivers reject domains without postmaster |

## 3. Stale facts to fix (do NOT carry forward)

`DEPLOYMENT_GUIDE.md` §a–d and `EMAIL_DELIVERABILITY.md` still show the **dead** IP
`45.77.217.37` (the pre-2026-05-31 box). **Do not publish any record using it.**
The real sending IP must be confirmed on-box and is deliberately **not guessed**
here (per HANDOFF.md). CLAUDE.md records the current VPS as `207.148.30.162`
(cohabits with OpenClaw) — treat that as *the box*, but confirm the actual **mail
egress** IP/host before deriving SPF/PTR, since mail now relays through Prime.

## 4. Best-of-all-worlds enhancements (the "supersociety" layer)

Once the baseline above is green, add — all legitimate, all pro-privacy:

- **MTA-STS** (`_mta-sts` TXT + policy at `https://mta-sts.outreach.plausiden.com`)
  and **TLS-RPT** (`_smtp._tls` TXT) — enforce + report on encrypted transport.
  Real transport-privacy wins, zero deliverability downside.
- **BIMI** (+ a **VMC** if budget allows) — verified brand logo in inboxes;
  trust signal that *depends on* DMARC at enforcement.
- **ARC** if any forwarding hop is involved.
- **Google Postmaster Tools** + **Microsoft SNDS** registration for telemetry.
- Per-domain warmup honoring the existing rate limits (10/h per domain,
  5 touches/30d per recipient).

## 5. What Salesman owns (not Prime)

- Emits `List-Unsubscribe` + `List-Unsubscribe-Post` (RFC 8058 one-click) when
  `SALESMAN_LIST_UNSUBSCRIBE` / `SALESMAN_UNSUBSCRIBE_BASE_URL` are set.
- Serves `/unsubscribe` + the verifiable-provenance endpoint on its **internal**
  interface for Prime to reverse-proxy (blocker B4.5b, task #9).
- Signs every send (Ed25519 receipts) and includes the hidden-but-verifiable
  provenance tag (custom header + footer link) — the honest counterpart to "no
  anonymity for the sender."
- Includes the physical postal address + clear sender identity in every body.

---
*Owner action items tracked as tasks #9 (expose /unsubscribe via Prime proxy),
#11 (confirm egress IP + re-derive SPF/DMARC/PTR), #24 (this spec).*
