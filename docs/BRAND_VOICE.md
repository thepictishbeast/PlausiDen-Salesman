# Salesman brand voice

> Salesman writes in **its own PlausiDen voice** — not any individual's
> personal voice. This guide defines that voice and is enforced, where it can
> be, by `crates/salesman-content/tests/template_voice.rs` (every shipped cold
> template is checked against the hard rules below). Prose here; teeth there.

## Who is speaking

PlausiDen — a privacy / security / anonymity FOSS ecosystem. The framing
(per `CLAUDE.md`) is a **civil-rights tool for go-to-market**: plausible
deniability, sovereign data, presumption of innocence. We are not a generic
SaaS vendor and we never sound like one.

## The voice in five lines

1. **Substance over ask.** Lead with something useful (a comparison, a CVE
   implication, an honest trade-off). Earning a reply beats requesting one.
2. **Honest, including about ourselves.** Say where a competitor wins. Name
   where our product would *not* have helped. Credibility is the conversion.
3. **Short and scannable.** Cold subjects are terse; bodies are a few tight
   lines, not a wall. If it reads like a brochure, cut it.
4. **Plain, specific, human.** Concrete nouns and real specifics over
   adjectives. No hype, no buzzwords, no "thought leadership."
5. **Respectful and reversible.** Every message offers a one-line, working
   opt-out, and we honor it immediately.

## Hard rules (enforced by test)

- **No dark patterns.** No fake urgency, fake social proof, or fake
  countdowns — "act now", "limited time", "last chance", "don't miss out",
  "exclusive offer", "100% guaranteed", etc. (`CLAUDE.md` → No dark patterns.)
- **Always an opt-out.** Every cold message carries a cessation/opt-out signal
  (e.g. "Reply STOP and I won't follow up.") in its body or mandatory phrases.
- **No self-contradiction.** A template never uses a phrase it lists in its own
  `forbidden_phrases`.

## Enforced at send time (not by the template test)

- **Identifiable sender.** A real sender identity + physical postal address ship
  with the message (the sender is attributable; only prospect *data* is
  sovereign — see `docs/OUTREACH_INFRA_HANDOFF.md`). This is enforced by the
  runtime sender config at send time, not by the template voice test.

## Banned reflexes (the clichés we keep out)

"circle back", "touch base", "checking in", "wanted to make sure you saw",
"robust security posture", "comprehensive solution", "next-generation",
"single pane of glass", "synergy", "best-in-class", "cutting-edge",
"revolutionary", "thought leadership". Per-template `forbidden_phrases` extend
this list; the test enforces each template against its own list.

## How templates encode the voice

Each `templates/**/cold/*.toml` carries:
- `subject_seed` / `body_seed` — the example the LLM expands (placeholders in
  `{{double_braces}}`), written *in voice*.
- `mandatory_phrases` — verbatim lines the model must include (the opt-out, any
  compliance footer).
- `forbidden_phrases` — phrases the model must not fall back to.

When adding a template, keep it inside these rules; the test will tell you if it
drifts. Avoid the words "spam", "blast", "manipulate", "trick" anywhere
(`CLAUDE.md`).
