# Layman's pitch — Salesman to a first-time reader

**Audience:** founder / VP Sales / Head of GTM at a 5-50 person B2B
SaaS, security, devtools, or data company. Has done cold outbound,
got burned, cares about reputation. Has never heard of Salesman.

**Goal:** get them to reply.

**Length budget:** under 320 words in the body (Gmail truncates around
that mark on mobile).

---

## Subject options (pick one)

1. The cold-email tool we couldn't find for sale, so we built it
2. Outbound that won't get your domain blacklisted
3. Cold-email tooling you can defend in a SOC 2 audit
4. We open-sourced the outbound tool we wish we'd had three years ago

(My pick: #1. Personal, specific, sets up the "founder built it"
arc that B2B SaaS buyers respond to.)

---

## Body

Hi {first_name},

Three things kept being true at every B2B company I worked at:

1. Outbound sales worked when each email was specific and personal. It hurt the brand when it wasn't.
2. Every tool we tried (Outreach, Apollo, Lemlist) optimized for volume — not reputation.
3. None of them let us prove what we sent if a regulator or auditor asked.

So I built Salesman.

It's a self-hosted system that runs on your own server. Your prospect list, your drafts, your replies — everything stays on your hardware. We never see any of it.

What it does:

- Ingests a prospect CSV and enriches each row with public OSINT — recent news, GitHub activity, prior press coverage, the tech stack their site uses.
- Drafts personalized first-touch emails using your choice of LLM (Claude, Gemini, or a local model), and rejects anything that reads generic before you ever see it. An ensemble of detectors catches AI clichés, marketing-speak, and fake hedging.
- Requires you to type your approval before each batch goes out. No auto-blast, no exceptions, ever.
- Signs every email into a cryptographic receipt chain. You can answer "what did we send and when" for any audit, ever.
- Auto-suppresses anyone who bounces, opts out, or replies with STOP. The unsubscribe link meets the new Gmail/Yahoo bulk-sender rules.
- Wraps fresh sender domains in a warmup ramp so you don't get blacklisted on day one.

The whole thing is open source. Read every line. Run it for free.

We charge for the hosted version, premium support, and integration help. The core is and always will be free.

Worth 20 minutes? Reply yes and I'll send install instructions or set up a walkthrough.

— William
PlausiDen
william@plausiden.com

---

## Why this draft (sales-craft notes)

- **Opens with three concrete pains** — not "I noticed your company."
  Each one lands a tell that this person knows the buyer's job.
- **"So I built it"** — founder-built tools sell at a premium in B2B;
  the buyer trusts the maker more than a sales rep.
- **First feature is the reputation/audit angle** — that's the
  differentiator vs. Outreach et al, and it's the SOC2/GDPR conversation
  the buyer is already having internally.
- **Open-source claim** is concrete and falsifiable — buyer can verify
  in 30 seconds.
- **Pricing model is upfront** — no surprise. Free + premium support is
  the plg-into-managed pattern that B2B buyers respect.
- **CTA is conversational** ("worth 20 minutes? reply yes") not
  "schedule via this calendly link" — gives the buyer permission to
  push back.
- **No marketing superlatives** — the detector ensemble would catch
  "industry-leading" / "best-in-class" / "transformative" and FAIL this
  draft. So we don't use them.
- **No fake hedging** — "I was wondering if you might possibly have a
  moment" would also fail the gate. Direct ask, real value, real CTA.
