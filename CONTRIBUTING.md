# Contributing to PlausiDen-Salesman

Read [`SCOPE.md`](SCOPE.md) and [`CLAUDE.md`](CLAUDE.md) first. The
hard rules in CLAUDE.md are non-negotiable.

## Workflow

1. Pick or create an issue.
2. Branch from `main`.
3. Run the [PlausiDen-Audits](https://github.com/thepictishbeast/PlausiDen-Audits)
   pre-commit set locally.
4. PR with the [PlausiDen-Meta CONTRIBUTOR_CHECKLIST](https://github.com/thepictishbeast/PlausiDen-Meta/blob/main/CONTRIBUTOR_CHECKLIST.md)
   filled in the description.
5. CI runs on the self-hosted runner pool (registered in
   [PlausiDen-Runner](https://github.com/thepictishbeast/PlausiDen-Runner)).

## Don'ts

- Don't add features beyond what the current phase requires.
- Don't add error handling for scenarios that can't happen.
- Don't write multi-line comment blocks. One short line max for non-obvious WHY.
- Don't commit `Cargo.lock` for library crates (commit it for binary crates).
- Don't introduce a SaaS LLM dependency. LFI only for personalization.
