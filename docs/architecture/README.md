# Architecture diagrams

Mermaid sources. Render to SVG with `mmdc` (Mermaid CLI):

```bash
npm install -g @mermaid-js/mermaid-cli
mmdc -i docs/architecture/pipeline.mmd -o docs/architecture/pipeline.svg
mmdc -i docs/architecture/funnel-state-machine.mmd -o docs/architecture/funnel.svg
mmdc -i docs/architecture/touch-lifecycle.mmd -o docs/architecture/touch.svg
```

Or paste the source into <https://mermaid.live> for ad-hoc viewing.

## Files

| Source | What it shows |
|---|---|
| `pipeline.mmd` | End-to-end Salesman dataflow — discovery → drafts → defense → outreach → receipts. |
| `funnel-state-machine.mmd` | `FunnelState` transitions (proptested in `salesman-core`). |
| `touch-lifecycle.mmd` | `TouchOutcome` transitions (enforced in `salesman-state`). |
