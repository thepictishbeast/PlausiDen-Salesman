# Real-estate vertical pack

A drop-in config bundle that tunes Salesman for residential
real-estate agents. Same engine; different products, objections,
competitors, brief, templates.

## What's in this directory

- `samples/products.toml`     — listings / buyer-rep / referrals as "products"
- `samples/objections.toml`   — real-estate-specific objection patterns (fees, dual agency, market timing, FSBO)
- `samples/competitors.toml`  — Zillow / Redfin / Compass / FSBO platforms
- `samples/operator-brief.md` — agent identity, tone (warm + local), banned phrases ("luxury", "exclusive")
- `templates/cold/*.toml`     — first-touch + follow-up + breakup templates tuned for buyer + seller leads

## How to use

```sh
# Point the LLM router at this brief
export SALESMAN_OPERATOR_BRIEF=$(pwd)/vertical/realestate/samples/operator-brief.md

# Use this pack's templates
export SALESMAN_TEMPLATES_DIR=$(pwd)/vertical/realestate/templates/cold

# Reply-drafter pulls from this pack's data
salesman draft-replies \
    --pricing-catalog vertical/realestate/samples/products.toml \
    --objections      vertical/realestate/samples/objections.toml

# Classify replies + tag competitor mentions
salesman classify-replies \
    --competitors vertical/realestate/samples/competitors.toml
```

## What "real-estate AI search visibility" looks like

When the sister asked "what do I need to do to be the first answer
when someone asks ChatGPT 'who's the best realtor in southern Utah?'"
— that's the GEO module (queued, not yet built). Today she still
gets full value from:

- **trigger scanner**: who in her sphere hit a recent life event
  worth a check-in (job change, growing family announcement, etc.)
- **decision-maker finder**: for her commercial side, who's the
  actual buyer at a target property-management company
- **CRM analytics** (template-stats --by segment): which template
  wins for first-time buyers vs investors vs sellers
- **send-time analytics**: when do her seller leads actually reply
- **referral-ask**: 30 days post-close, ask for two intros

## What's deliberately NOT here yet

- Calendar integration with MLS / Showing-Time
- IDX feed → trigger events ("new listing in your saved area")
- Compass / Sierra-Interactive integration
- AI-search visibility (GEO module — queued; covers her #1 ask)

These need either paid APIs or vertical-specific scaffolding
beyond a TOML pack. Shipping the pack now means she gets the core
engine + relevant defaults today; the verticality deepens later.
