# FOSS audit

Per-subsystem audit of what we built vs what FOSS already exists.
Owner directive: prefer existing FOSS when it fits; only build from
scratch when no fit exists.

Format: **Subsystem** → **Decision** → **Reasoning**.

## Already on FOSS (✅ absorbed)

| Subsystem | FOSS used | Notes |
|---|---|---|
| SMTP send | `lettre` 0.11 | Mature, well-maintained, rustls-only build keeps native-tls out |
| IMAP poll | `async-imap` 0.10 + `mail-parser` 0.9 | TLS-only; bridged to tokio via `tokio-util::compat` |
| Markdown render | `pulldown-cmark` 0.13 | Tables, footnotes, strikethrough |
| Postgres | `sqlx` 0.8 | Compile-time-optional macros; native types |
| HTTP server | `axum` 0.7 + `tower-http` 0.5 | What everyone uses |
| HTTP client | `reqwest` 0.12 (rustls) | What everyone uses |
| Crypto signing | `ed25519-dalek` 2 + `sha2` 0.10 | Standard |
| CSV ingest | `csv` 1.3 | Standard |
| HTML scraping | `scraper` 0.20 | Standard |
| CLI | `clap` 4 derive | Standard |
| Tracing | `tracing` 0.1 + `tracing-subscriber` 0.3 | Standard |
| Time | `chrono` 0.4 | Could swap to `jiff` someday — not urgent |
| Async runtime | `tokio` 1 | The default |
| Serde | `serde` 1 + `serde_json` + `toml` | The default |
| zeroize | `zeroize` 1 | Standard for secret wiping |
| Property tests | `proptest` 1 | Standard |
| Rate limiting | (built-in via Postgres COUNT) | Considered: `governor` crate. Decided against — our caps are per-recipient over a 30d window, more naturally expressed as a SQL count than as a token-bucket. |

## Built ourselves but FOSS exists (TODO: evaluate)

### CRM front-end (crm-api)
**Built:** ~400-line axum + server-rendered HTML dashboard with SVG funnel.
**FOSS alternatives:**
- **Twenty** (twenty.com) — modern open-source CRM, NestJS + Postgres + React. AGPL.
- **EspoCRM** — PHP, GPL. Very mature.
- **SuiteCRM** — PHP, AGPL. Even more mature, heavier.
- **ERPNext** — Python/Frappe, GPL. Full ERP, CRM is a module.
**Decision:** keep our minimal dashboard for now (zero-dep, integrated
with our event bus). Document Twenty as the upgrade path when:
- Owner needs multi-user (we're single-operator)
- Needs deal-pipeline workflows beyond our funnel view
- Needs custom fields / forms / kanban
**Migration path when ready:** crm-ingest projector writes to
Twenty's Postgres directly OR via Twenty's REST API; deprecate
crm-api.

### Static-site renderer (salesman-content::site)
**Built:** ~250-line markdown→HTML renderer with sitemap + index.
**FOSS alternatives:**
- **Zola** (getzola.org) — Rust, single binary, themes, RSS,
  taxonomies, multilingual, image processing, sass. Mature.
- **mdBook** — Rust, optimized for documentation sites.
- **Hugo** — Go, very fast, biggest theme ecosystem.
**Decision:** keep ours for now (14 source files; integrated into
the CLI). Switch to Zola when:
- Site exceeds ~30 pages
- We need RSS / Atom for the comparison-page track
- We need taxonomies (industry tag, product tag)
- We need image processing
**Migration path:** point Zola's `content/` at our `docs/` + add a
`config.toml` and a theme. The 5 ADRs and 3 comparison pages port
unchanged.

### Sieve adversarial-AI detector (salesman-detector)
**Built:** Heuristic ensemble (cliché openers, banned phrases,
em-dash density, "delve"/"paradigm" overuse) + corpus regression
test.
**FOSS alternatives:**
- **Ghostbuster** (Berkeley NLP) — open-source AI-text detector;
  not great accuracy on short-form / outreach.
- **GPT-Zero open release** — heuristic-based, similar to ours.
- **Binoculars** (research code) — needs two LLMs side-by-side.
- **Hugging Face roberta-base-openai-detector** — old, rough.
**Paid alternatives:** Originality.ai, GPTZero, Copyleaks.
**Decision:** keep our heuristic ensemble (it's domain-specific and
gives REASONS, not just a number). Plan: integrate Originality.ai
as a *second opinion* (task N1) — refusal triggered only when both
agree.

### LLM cost ledger (salesman-state::llm_calls)
**Built:** llm_calls table + cost_summary report.
**FOSS alternatives:**
- **LiteLLM** (BerriAI) — proxy with cost tracking, multi-provider.
  Python.
- **Helicone** — proxy + dashboard. Open core.
- **OpenLIT** — OpenTelemetry-based, vendor-neutral. Apache 2.
- **Phoenix** (Arize) — eval + tracing.
**Decision:** keep our table (it's 1 table + 1 report). Add
OpenTelemetry export to Helicone/OpenLIT when we want fancy
dashboards or multi-tenant cost views. Not urgent.

### Agent loop (salesman-orchestrator)
**Built:** ToolRegistry + LlmRouter + plan→act→observe loop.
**FOSS alternatives:**
- **AutoGen** (Microsoft) — Python. Multi-agent.
- **LangChain / LangGraph** — Python. Heavyweight.
- **smolagents** (Hugging Face) — Python. Minimal.
- Rust-native: `rig` (rig-rs.org), `swiftide`, `langchain-rust` — all early.
**Decision:** keep ours (Rust constraint per ADR-0001). Trade is
real: we don't get the agent-pattern library Python has. Mitigate
by porting individual patterns we need (ReAct, reflexion, etc.)
rather than importing a framework.

## Built ourselves and FOSS doesn't fit (✅ keep)

### Cold-email template library (templates/cold/*.toml)
Domain-specific to PlausiDen voice + segments. No FOSS template
library would carry our anti-tells / mandatory phrases.

### LLM router with backend-pinning hints (salesman-llm::router)
Specific to our routing rules (Reasoning → Claude, Bulk → Gemini
Flash, Sovereign → LFI). FOSS routers exist (LiteLLM) but they
abstract differently.

### Receipt chain (salesman-receipts)
Tiny + integrated. Could absorb a generic Merkle library someday
but the surface area doesn't justify the dep churn.

### Multi-touch sequence schema (sequence_steps + prospect_sequence_state)
Domain-specific. No FOSS sequencer fits without a heavy framework.

## Open questions

- Should we cross-compile via `cargo zigbuild` instead of building
  on the VPS? (FOSS, simpler ops once set up.) — TBD.
- Should the CLI use `dialoguer` for the human-confirm prompts in
  `--first-domain-confirm`? — Yes, when we add that flow.

---

*Maintained at the start of each subsystem decision. New entries
appended; old entries get updated `Decision` lines as the call
changes.*
