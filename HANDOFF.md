# HANDOFF — read this first

> Current ground-truth state for whoever (Claude or human) picks Salesman up.
> Written 2026-06-02 from **verified** sources (Vultr API + this repo + git),
> not from recollection. Where a fact could not be verified from outside the
> box, it is marked **RECONCILE ON-BOX** — confirm it yourself before relying on it.

## Runtime (VERIFIED via Vultr API 2026-06-02)

- **OpenClaw VPS was re-provisioned 2026-05-31** as a NEW box. Current:
  - label `openclaw` · **IP `207.148.30.162`** · region `ewr`
  - plan `vx1-g-2c-8g-120s` · **Debian 13 (trixie)** · status: active running
  - created `2026-05-31T05:11:18Z`
- ⚠️ **The old `45.77.217.37` is DEAD** (previous box, deleted). The repo still
  cites `45.77.217.37` in several docs (it predates the rebuild) — treat every
  occurrence as the OLD box. `CLAUDE.md` has been corrected to the new IP; the
  deliverability docs have NOT (see below).
- Access (from the prime host `plausiden-prime`): Vultr creds at
  `/tank/secrets/vultr.env` (`VULTR_API_KEY`); SSH key at
  `/tank/secrets/openclaw_id_ed25519`. (Historically access was laptop-side.)

## RECONCILE ON-BOX (could not verify from outside)

1. **Is salesman actually deployed + running on the new box?** Check
   `/opt/salesman/`, the `plausiden-salesman` systemd unit, Postgres + Redis.
   A rebuilt box may be fresh (re-clone + re-deploy needed) rather than restored.
2. **Mail-send topology / DNS.** `docs/DEPLOYMENT_GUIDE.md` +
   `docs/EMAIL_DELIVERABILITY.md` still show SPF/PTR using `45.77.217.37`, while
   `OWNER_BLOCKERS.md` (B2) says the real sender was **web-01 `207.246.86.218`**,
   and prime is now itself a mail host (Postfix/Dovecot/OpenDKIM). **Do not trust
   the doc IPs** — confirm the real current sending IP + re-derive SPF/DMARC/PTR
   before any send. This was deliberately left unedited to avoid baking in a
   wrong DNS fact.
3. **outreach.plausiden.com `/unsubscribe`** reachability (blocker B4.5b) —
   verify the reverse-proxy is actually live on the new box.

## Where we left off (grounded in this repo + git)

- **Tier 0 SHIPPED** — 15-crate Rust workspace, LLM trait + subscriber-CLI
  transport, `discover-llm` prospecting, cold-email drafting + owner review
  queue, Ed25519 receipts, SMTP outreach, reply/Authentication-Results parsing,
  anti-spoof gate. (See `ROADMAP.md` Tier 0 + `git log`; last code work
  2026-05-01, repo housekeeping through 2026-05-17.)
- **Active blockers** (`OWNER_BLOCKERS.md`): **B4.5b** (expose `/unsubscribe`
  reverse-proxy on outreach.plausiden.com) · **B6** (template voice pass).
- **Shelved** (`SALESMAN_TODO.md`): **N1** AI-text detector · **O1–O4**
  PlausiDen-Mail infra · **R3** Postmaster Tools + SNDS · **R4** first-real-send.
- **First-send runway** = the checklist at the bottom of `OWNER_BLOCKERS.md`
  (B4→B8). Most of B1–B5.5 resolved; the gate is B4.5b + a verified runtime.
- `CHANGELOG.md` is **stale** (still says "v0.0.0 pre-implementation") — trust
  `ROADMAP.md` + `git log` for real status, not the changelog.

## Repo health

- Working copy on prime is **clean and in sync with `origin/main`** (GitHub,
  `thepictishbeast/PlausiDen-Salesman`, private). Nothing stranded locally.
- Read order for a fresh Claude: **this file → `CLAUDE.md` → `SCOPE.md` →
  `OWNER_BLOCKERS.md` → `ROADMAP.md`**.
