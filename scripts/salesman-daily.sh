#!/usr/bin/env bash
# salesman-daily.sh — the operator's daily runbook.
#
# The CLI has 60+ subcommands; this script is the AUTHORITATIVE
# answer to "what do I run, in what order, every day?". Each
# section is one human-comprehensible phase of a sales day. Pause
# points are explicit so the operator stays in the loop on every
# real send.
#
# Usage:
#   SALESMAN_CAMPAIGN=acme-warmup SALESMAN_PRODUCT=Sentinel ./salesman-daily.sh
#   ./salesman-daily.sh --campaign acme-warmup --product Sentinel
#
# Optional autonomy knobs (set in env):
#   SALESMAN_DISCOVER_QUERY  — free-form Brave search query. When set
#                              AND BRAVE_SEARCH_API_KEY is in env, the
#                              script auto-discovers prospects each run
#                              before drafting. Skip this var to stick
#                              to whoever is already in the campaign.
#   SALESMAN_DISCOVER_TOP    — cap per discovery run (default 10).
#   SALESMAN_FIND_BUYERS     — set to 1 to auto-persist the top contact
#                              per company as the prospect's primary.
#                              Default 0 (read-only — operator runs
#                              `find-buyers --persist` manually).
#   SALESMAN_AUTOPILOT       — set to 1 to skip interactive pauses
#                              (cron use). Operator review still
#                              required before send-pending --for-real.
#
# Hard rules embedded here:
#   - never auto-`send-pending --for-real`. Operator confirms.
#   - never skip the doctor preflight.
#   - dry-run approve-all first, never bulk-approve without
#     reviewing the held drafts.

set -euo pipefail

# ---------------------------------------------------------------- args
CAMPAIGN="${SALESMAN_CAMPAIGN:-}"
PRODUCT="${SALESMAN_PRODUCT:-}"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --campaign) CAMPAIGN="$2"; shift 2 ;;
    --product)  PRODUCT="$2";  shift 2 ;;
    --help|-h)
      echo "Usage: $0 [--campaign NAME] [--product NAME]"
      echo "  Or set SALESMAN_CAMPAIGN / SALESMAN_PRODUCT in env."
      echo "  Optional: SALESMAN_DISCOVER_QUERY for autonomous prospecting."
      exit 0
      ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

if [[ -z "$CAMPAIGN" || -z "$PRODUCT" ]]; then
  echo "ERROR: --campaign and --product (or SALESMAN_CAMPAIGN / SALESMAN_PRODUCT) are required." >&2
  exit 2
fi

DISCOVER_TOP="${SALESMAN_DISCOVER_TOP:-10}"
FIND_BUYERS_PERSIST_FLAG=""
if [[ "${SALESMAN_FIND_BUYERS:-0}" == "1" ]]; then
  FIND_BUYERS_PERSIST_FLAG="--persist"
fi

SALESMAN="${SALESMAN_BIN:-salesman}"

section() {
  echo
  echo "=================================================================="
  echo "$*"
  echo "=================================================================="
}

pause_for() {
  if [[ "${SALESMAN_AUTOPILOT:-0}" == "1" ]]; then
    echo "(autopilot on; skipping pause)"
    return
  fi
  echo
  read -r -p ">>> $*  press Enter to continue, Ctrl-C to abort: " _
}

# ------------------------------------------------------------- 1. doctor
section "[1/12] doctor — preflight health check"
$SALESMAN doctor

# --------------------------------------------- 2. autonomous prospect discovery
if [[ -n "${SALESMAN_DISCOVER_QUERY:-}" ]]; then
  section "[2/12] discover-search — autonomous prospecting (\"$SALESMAN_DISCOVER_QUERY\")"
  $SALESMAN discover-search \
    --campaign "$CAMPAIGN" \
    --query "$SALESMAN_DISCOVER_QUERY" \
    --top "$DISCOVER_TOP" \
    --persist || echo "(discover-search soft-failed; continuing with existing prospects)"
else
  section "[2/12] discover-search — SKIPPED (set SALESMAN_DISCOVER_QUERY to enable)"
fi

# ----------------------------------- 3. enrich (homepage scrape)
section "[3/12] enrich — homepage scrape into industry / tech_signals / description"
$SALESMAN enrich --campaign "$CAMPAIGN" --concurrency 4 || echo "(enrich soft-failed; continuing)"

# --------------------------------------- 4. find-buyers (decision-maker)
section "[4/12] find-buyers — team-page scrape for buyer email + role"
$SALESMAN find-buyers --campaign "$CAMPAIGN" $FIND_BUYERS_PERSIST_FLAG \
  || echo "(find-buyers soft-failed; continuing)"

# ----------------------------------------------------- 5. trigger scan
section "[5/12] triggers scan — fresh OSINT signals (GDELT + HN)"
$SALESMAN triggers scan --campaign "$CAMPAIGN"

# ------------------------------------------------ 6. auto-draft on triggers
section "[6/12] triggers draft — auto-draft on top-5 unused triggers"
$SALESMAN triggers draft --campaign "$CAMPAIGN" --product "$PRODUCT" --top 5

# --------------------------------------------------- 7. inbox + classify
section "[7/12] inbox-poll — pull new replies from IMAP"
$SALESMAN inbox-poll || echo "(inbox-poll soft-failed; continuing)"

section "[8/12] classify-replies — classify + extract interests + inline alert"
$SALESMAN classify-replies --batch 50

# --------------------------------------------- 9. draft replies for queue
section "[9/12] draft-replies — auto-draft responses to engaged/question/objection"
$SALESMAN draft-replies --batch 25 \
  $( [[ -f samples/pricing.toml      ]] && echo "--pricing-catalog samples/pricing.toml" ) \
  $( [[ -f samples/meeting-slots.toml ]] && echo "--meeting-slots samples/meeting-slots.toml" ) \
  $( [[ -f samples/objections.toml   ]] && echo "--objections samples/objections.toml" )

# --------------------------------------------------- 10. fact-check queue
section "[10/12] fact-check — bulk fact-trace + personalization gates"
$SALESMAN fact-check --campaign "$CAMPAIGN" || echo "(fact-check flagged drafts; review with 'salesman review')"

# --------------------------------------------------- 11. approve-all preview
section "[11/12] approve-all --dry-run — preview which drafts would auto-approve"
$SALESMAN approve-all --campaign "$CAMPAIGN" --dry-run

pause_for "Review the held drafts above. Run 'salesman review' to inspect, \
'salesman reject --touch <UUID>' to drop, then 'salesman approve-all --campaign $CAMPAIGN' \
when you're ready to bless the clean ones. Send is STILL gated by 'send-pending --for-real'."

# --------------------------------------------------- 12. operator briefing
section "[12/12] next-best-actions — today's prioritized to-do"
$SALESMAN next-best-actions --campaign "$CAMPAIGN"

section "summary — last 24h pipeline"
$SALESMAN summary --since-hours 24

echo
echo "Daily cycle complete. Next: review + 'salesman send-pending --campaign $CAMPAIGN --for-real --confirm-typed' when ready."
