# Contributing to PlausiDen-Salesman

Read [`SCOPE.md`](SCOPE.md) and [`CLAUDE.md`](CLAUDE.md) first. The
hard rules in CLAUDE.md are non-negotiable. See also
[`docs/COLLABORATION.md`](docs/COLLABORATION.md) for the collaborative-work guide.

## Workflow

1. Pick or create an issue.
2. Branch from `main`.
3. Run the [PlausiDen-Audits](https://github.com/thepictishbeast/PlausiDen-Audits)
   pre-commit set locally.
4. PR with the [PlausiDen-Meta CONTRIBUTOR_CHECKLIST](https://github.com/thepictishbeast/PlausiDen-Meta/blob/main/CONTRIBUTOR_CHECKLIST.md)
   filled in the description.
5. CI runs on GitHub-hosted `ubuntu-latest` runners (see
   [`.github/workflows/ci.yml`](.github/workflows/ci.yml)).

Before pushing, run the local quality gate [`scripts/check.sh`](scripts/check.sh)
— it runs `cargo fmt --check`, `cargo clippy --workspace -- -D warnings`,
`cargo test --workspace`, plus `cargo audit` / `cargo deny check` when those
tools are installed.

## Don'ts

- Don't add features beyond what the current phase requires.
- Don't add error handling for scenarios that can't happen.
- Don't write multi-line comment blocks. One short line max for non-obvious WHY.
- Don't commit `Cargo.lock` for library crates (commit it for binary crates).
- Don't add a new SaaS data-path call without redaction. Drafting/reply use SaaS
  Claude/Gemini behind a PII redaction boundary; local-only LFI is deferred (see
  [ADR-0003](docs/decisions/0003-claude-and-gemini-not-lfi-yet.md), [`docs/PII_REDACTION_BOUNDARY.md`](docs/PII_REDACTION_BOUNDARY.md)).
