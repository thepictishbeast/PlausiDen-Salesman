# AB_TESTING.md — subject-line variants in Salesman

## Convention

A subject-line A/B test in Salesman is **two templates with the same
body but different `subject_seed`**, named with a `_b` (and
optionally `_c`, `_d`) suffix.

Example pair:

```
templates/cold/introduction.toml      # A — "Quick question on {industry} workflow"
templates/cold/introduction_b.toml    # B — "{specific signal from research}"
```

The bodies should be identical (or near-identical). Only the
subject changes. That isolates the subject as the variable.

## How the bandit picks

When a sequence has multiple candidate templates for a step, the
operator passes BOTH keys to the sequencer. The existing
`pick_template_via_bandit` (already in `salesman-state`) does
epsilon-greedy selection:

- With probability `1 - epsilon`: pick the template with the highest
  `engaged_rate` so far.
- With probability `epsilon` (default 0.20): pick a random other
  candidate.

So early on the bandit explores both variants ~50/50. As data
accumulates, it tilts toward the winner. The exploration tail keeps
the loser sampled occasionally so we don't fixate on a noisy
early-game result.

## Reading the results

```sh
salesman template-stats
```

shows per-key stats. Filter to your A/B pair mentally:

```
template            drafted   sent  replied  engaged  reply%
introduction          150     142     14        7      9.9%
introduction_b        148     143     22       12     15.4%
```

`introduction_b` is winning. Once both have `sent >= 50` and the
gap is `≥ 2×` on `engaged_rate`, retire the loser:

1. Remove the losing key from your sequence's candidate list.
2. Optional: rename the winner from `_b` back to the base key, or
   keep the winner as the new baseline and create a fresh `_b`
   variant to test against it.

## Per-segment A/B

`salesman template-stats --by segment` breaks each template down by
the prospect's industry. It's possible (and common) for variant A
to win on one segment and variant B to win on another. In that case
the underperformer's flag (`⚠`) only fires within a segment — the
operator decides whether to ship per-segment routing or pick a
universal winner.

## Anti-patterns

- **Changing too many things at once.** If the body AND subject
  AND CTA all differ between A and B, you don't know which variable
  drove the result. Discipline: change ONE thing per pair.
- **Calling a winner too early.** With `sent < 30` per variant the
  noise dominates. Wait. The bandit handles the wait gracefully —
  you don't need to baby-sit.
- **Running too many variants in parallel.** With 4+ variants the
  exploration tax exceeds the signal you'll extract. Stick to 2-3.
- **Forgetting to add the variant to the sequence's candidate list.**
  If only the base key is in the candidates, the bandit never sees
  the variant and the test never runs. Verify with
  `salesman template-stats` — both keys should have `drafted > 0`.
