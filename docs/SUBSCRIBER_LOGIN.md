# Subscriber-login transport for the LLM router

Owner directive 2026-04-30: **Salesman on openclaw must use the
subscriber login (Claude Pro/Max + Gemini Advanced via the operator's
own logged-in CLIs), not API keys.** API keys re-bill the same
completions at developer rates; the subscriptions are flat-fee and
already paid for.

## How it works

`salesman-llm` ships two transports for each vendor backend:

| Transport | Source | Auth | Cost ledger |
|---|---|---|---|
| `api`  *(default)* | `salesman-llm::claude::ClaudeBackend` / `GeminiBackend` | API key in env | per-token, real $$ |
| `cli`              | `salesman-llm::subscriber_cli::SubscriberCliBackend`     | subprocess-spawn the operator's `claude` / `gemini` CLI; auth lives in *that* CLI's credential store | always 0 µ-USD (sub is flat-fee); token counts are byte/4 estimates for capacity planning |

Selection is one env var on the box that runs `salesman`:

```bash
SALESMAN_LLM_TRANSPORT=cli   # or `api` to keep legacy behavior
```

Restart the salesman service after changing it. The transport is read
once at router-build time.

## openclaw bootstrap

1. **Install the vendor CLIs** as the salesman user (so the auth
   tokens land in `~salesman/.config/...` not root's home):

   ```bash
   sudo -iu salesman bash -lc '
     # Claude Code (Anthropic): https://docs.anthropic.com/en/docs/claude-code
     curl -fsSL https://claude.ai/install.sh | sh
     # Gemini CLI (Google): https://github.com/google-gemini/gemini-cli
     curl -fsSL https://gemini.google.com/install.sh | sh
   '
   ```

2. **Log in interactively as the salesman user** so the subscriber
   session lands in that user's credential store:

   ```bash
   sudo -iu salesman bash -lc 'claude login'
   sudo -iu salesman bash -lc 'gemini auth login'
   ```

   Both flows open a browser-auth URL the operator pastes into a
   workstation browser; on completion the token is written to the
   salesman user's home dir on openclaw. After this point the
   subscription is the auth — no API key gets created.

3. **Smoke-test the CLIs as that user** before pointing salesman at
   them:

   ```bash
   sudo -iu salesman bash -lc 'echo "say one word" | claude --print'
   sudo -iu salesman bash -lc 'echo "say one word" | gemini chat'
   ```

   Each should print one short word. If they prompt for re-auth or
   refuse, repeat step 2.

4. **Flip the transport in the salesman env file** at
   `/etc/salesman/env` (or wherever `EnvironmentFile=` in the
   systemd unit points):

   ```bash
   SALESMAN_LLM_TRANSPORT=cli
   # Optional overrides (defaults shown):
   # SALESMAN_CLAUDE_CLI_BIN=claude
   # SALESMAN_CLAUDE_CLI_ARGS=["--print"]
   # SALESMAN_GEMINI_CLI_BIN=gemini
   # SALESMAN_GEMINI_CLI_ARGS=["chat"]
   # SALESMAN_LLM_CLI_TIMEOUT_SEC=180
   ```

   You can leave `ANTHROPIC_API_KEY` and `GEMINI_API_KEY` unset (or
   set to dummy values) — when `transport=cli` they are ignored.

5. **Restart the service:**

   ```bash
   sudo systemctl restart salesman
   sudo journalctl -u salesman -n 20 --no-pager | grep -i 'subscriber-cli'
   ```

   Expect to see `registered Claude (subscriber-cli) backend` and
   `registered Gemini (subscriber-cli) backend` in the log.

6. **End-to-end smoke** with the new `quick-stub` subcommand:

   ```bash
   sudo -iu salesman /opt/salesman/bin/salesman quick-stub \
       --campaign smoke-cli --count 3
   sudo -iu salesman /opt/salesman/bin/salesman draft \
       --campaign smoke-cli --product Sentinel
   ```

   The drafter will route through the subscriber-CLI backend; the
   first call may take a few extra seconds while the CLI warms up.

## What this transport does NOT support (yet)

- **Tool calls.** The CLI returns plain text only. Salesman call sites
  that pass `tools` to the router (e.g. structured tool-use loops)
  will get a clean error message, not a silent text fallback. Keep
  `transport=api` for those, or switch the call site to a JSON-shaped
  prompt that doesn't need tool-use round-trips.
- **Cache-control / prompt caching.** The API path uses Anthropic's
  ephemeral cache markers; the CLI path doesn't expose them. First-
  call latency is the same; the savings on cold-cache amortization
  don't apply.
- **Real cost accounting.** Cost ledger always reads 0 µUSD because
  the subscription is flat-fee. Use latency + token-count estimates
  for capacity planning.

## Threat model + safety

- **Prompt never lands in argv.** It's piped over stdin, so it can't
  be observed via `ps`, journald with cmdline logging, or shell
  history.
- **No shell.** `Command::arg` (not `Command::sh`) — request data
  cannot inject shell metacharacters.
- **Subprocess timeout.** Default 180 s; configurable via
  `SALESMAN_LLM_CLI_TIMEOUT_SEC`. On timeout the child is
  killed-on-drop so a stuck CLI doesn't pin a worker.
- **Auth scope.** The subscriber session lives in the salesman user's
  home directory. `mode 700` on `~salesman/.config/...` keeps it off
  the openclaw user's data and the root account. Never run salesman
  as root.

## Reverting

Set `SALESMAN_LLM_TRANSPORT=api` (or unset it) and restart the
service. The `api` transport path is unchanged from before this
patch — no migration step needed.
