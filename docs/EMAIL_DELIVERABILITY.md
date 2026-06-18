# Email deliverability runbook

> ⚠️ **STALE INFRASTRUCTURE FACTS — verify on-box before trusting.**
> The OpenClaw VPS was re-provisioned 2026-05-31; the IP `45.77.217.37`
> used in the SPF / PTR / DNS examples below is **DEAD**. The real current
> sending IP is **unverified** (`HANDOFF.md` → "RECONCILE ON-BOX" also
> flags that the true sender may be a different host than this doc assumes).
> Do NOT publish any IP / SPF / PTR record from this runbook until you have
> re-derived it from the live box. (Intentionally not replaced with a guess.)

This is the operator playbook for getting cold-outreach mail from
the Salesman VPS to land in the recipient's **inbox** rather than
spam. Follow it in order before turning on Salesman's send path.

## TL;DR

To send legitimate B2B email you need, on the **sending domain**:

1. **SPF** TXT record listing the IPs/services allowed to send.
2. **DKIM** TXT record with a public key; messages signed by the
   matching private key.
3. **DMARC** TXT record telling receivers what to do when SPF/DKIM
   fail, and where to send aggregate reports.
4. A **mailbox at `postmaster@<domain>`** (RFC 5321 recommends; some
   receivers reject mail from domains without one).
5. A **PTR (reverse DNS)** record on the sending IP pointing to the
   sending hostname (Vultr lets you set this in the control panel).
6. A **`List-Unsubscribe`** header on every outbound message
   (Salesman emits this when `SALESMAN_LIST_UNSUBSCRIBE` is set).
7. A **physical address** in the body (CAN-SPAM requires this).

Once the records propagate (5 min to 24 h), register the domain with
**Google Postmaster Tools** and **Microsoft SNDS** to get
deliverability telemetry.

## Step-by-step

### Step 1 — pick the sending domain

Use a subdomain of your main brand for outbound: e.g.
`outreach.plausiden.com`. Keeps reputation isolated from your
primary domain so a flap on outreach can't burn your transactional
or marketing mail.

### Step 2 — set SPF

Add a TXT record at the sending domain:

```
outreach.plausiden.com.  3600  IN  TXT  "v=spf1 ip4:45.77.217.37 -all"
```

If you also send via a relay (Mailgun, Postmark, SendGrid, AWS SES),
include them:

```
v=spf1 ip4:45.77.217.37 include:mailgun.org -all
```

`-all` (hard fail) is preferred over `~all` (soft fail) for outreach
domains. Verify with `dig +short txt outreach.plausiden.com`.

### Step 3 — set DKIM

Generate an Ed25519 or 2048-bit RSA keypair on the VPS:

```
sudo apt install opendkim opendkim-tools
sudo opendkim-genkey -t -s salesman -d outreach.plausiden.com
```

This produces `salesman.private` (private key — protect with mode
0600) and `salesman.txt` (public key, ready to drop into DNS). Add a
TXT record at `salesman._domainkey.outreach.plausiden.com`:

```
salesman._domainkey.outreach.plausiden.com.  3600  IN  TXT
  "v=DKIM1; k=rsa; p=MIIBIjANBg...PUBLIC_KEY..."
```

Configure your MTA (Postfix + opendkim, or whatever relay) to sign
all outbound with the `salesman` selector. Salesman itself doesn't
sign — the MTA in front does.

### Step 4 — set DMARC

Add a TXT record at `_dmarc.outreach.plausiden.com`:

```
_dmarc.outreach.plausiden.com.  3600  IN  TXT
  "v=DMARC1; p=none; rua=mailto:dmarc@plausiden.com; pct=100; aspf=s; adkim=s"
```

Start with `p=none` so you get reporting without rejection. After 2
weeks of clean reports, move to `p=quarantine`, then `p=reject`.

`aspf=s` and `adkim=s` enforce strict alignment (the From domain
must EXACTLY match the SPF/DKIM domain). Loose alignment (`r`) is
the default but offers less protection.

### Step 5 — PTR / reverse DNS

In the Vultr control panel: Servers → your-server → Settings → IPv4
→ Reverse DNS. Set it to your sending hostname:

```
45.77.217.37  →  mail.outreach.plausiden.com
```

Verify with `dig -x 45.77.217.37`.

### Step 6 — postmaster mailbox

Create `postmaster@outreach.plausiden.com` and forward to a mailbox
you actually read. Some receivers verify the existence of this
address before accepting mail.

### Step 7 — `List-Unsubscribe` header

Set `SALESMAN_LIST_UNSUBSCRIBE` in `/etc/salesman.env`. Use both:

```
SALESMAN_LIST_UNSUBSCRIBE=mailto:unsubscribe@outreach.plausiden.com
```

You ALSO need an HTTPS opt-out URL (per RFC 8058) for one-click
unsubscribe. Salesman emits both `List-Unsubscribe` and
`List-Unsubscribe-Post: List-Unsubscribe=One-Click`. Make sure the
URL accepts a `POST` from any user-agent and immediately suppresses
the recipient.

### Step 8 — physical address in the body

Required by CAN-SPAM. Set `SALESMAN_COMPLIANCE_FOOTER` to include
your physical mailing address. Salesman appends this footer to every
body.

```
SALESMAN_COMPLIANCE_FOOTER="PlausiDen, 123 Some Street, Pittsburgh PA 15201
Reply STOP to opt out of further messages."
```

### Step 9 — register with deliverability dashboards

- **Google Postmaster Tools** (postmaster.google.com): verify domain
  ownership via DNS TXT, watch reputation + spam-rate.
- **Microsoft SNDS** (sendersupport.olc.protection.outlook.com):
  request access to your sending IP for Outlook telemetry.
- **MXToolbox blacklist check** (mxtoolbox.com/blacklists.aspx):
  weekly check for sneaky listings.

## Going live checklist

Before you flip `--for-real` for the first time:

- [ ] SPF record live and resolving with `-all`
- [ ] DKIM record live, MTA signing test passed
  (`https://www.mail-tester.com`)
- [ ] DMARC record live with `p=none` + `rua` reporting
- [ ] PTR set on the Vultr IP
- [ ] `postmaster@<domain>` mailbox exists and reads
- [ ] `SALESMAN_LIST_UNSUBSCRIBE` set
- [ ] `SALESMAN_COMPLIANCE_FOOTER` includes physical address
- [ ] `SALESMAN_FROM_NAME` and `SALESMAN_FROM_EMAIL` set
- [ ] mail-tester.com score ≥ 9/10 from a test send
- [ ] First batch is **small** (≤25 messages) to a known list to
  warm the IP before scaling up

## Operational hygiene

- Monitor your bounce rate. If hard-bounce rate exceeds 3 %, pause
  sends and investigate. Salesman auto-suppresses on bounce
  (Phase 1.6) — but a domain-wide bounce flood is your problem.
- Watch the optout rate. If > 1 % in any 7-day window, your
  prospect list isn't qualified. Stop sending and re-sift.
- Rotate the DKIM key annually. Update DNS first (publish new
  selector), wait for propagation, then switch the MTA to sign with
  the new key.
- Keep DMARC reports flowing. They're how you find spoofing in the
  wild and how you prove to receivers you're a legitimate sender.

## Common failure modes

| Symptom | Likely cause | Fix |
|---|---|---|
| All mail to Gmail goes to spam | Missing DKIM, bad PTR, brand-new domain | Add DKIM, set PTR, warm slowly |
| All mail to Outlook bounces | IP on Microsoft blocklist | Apply via SNDS to be reviewed |
| DMARC report shows alignment failures | From-domain != DKIM-domain | Fix MTA signing config to use the right d= |
| Some receivers reject "no MX" | The sending domain has no MX record | Add an MX record (even pointing to the sending host itself) |
| Sudden drop in deliverability | IP listed on a blocklist | Check MXToolbox; request delisting |
