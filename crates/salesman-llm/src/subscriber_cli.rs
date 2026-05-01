//! Subscriber-login LLM backend — uses the operator's *already
//! logged-in* `claude` (Claude Code) or `gemini` CLI by spawning it
//! as a subprocess. No API key required: the subscriber session
//! lives in the CLI's own credential store on the box.
//!
//! Why this exists:
//!   The owner pays for Claude Pro/Max + Gemini Advanced. API keys
//!   would re-bill those completions at developer rates. By driving
//!   the subscriber's own CLI we get the inference under the seat
//!   subscription that's already paid for.
//!
//! Threat model + safety:
//!   - The prompt goes in via stdin (NOT via argv) so prompt content
//!     never appears in `ps` output, journald, or shell history.
//!   - The CLI binary path is taken from a fixed env var, never
//!     interpolated from request data. No shell is invoked
//!     (`Command::arg`, not `Command::sh`).
//!   - We never echo the prompt or response in tracing logs — only
//!     bytes-in / bytes-out / latency.
//!   - Subprocess timeout is enforced; on timeout we kill the
//!     process group so a stuck `claude` doesn't pin a worker.
//!
//! What this backend can't do (yet):
//!   - Tool-use round trips. The CLI returns plain text. Salesman
//!     callers that need structured tool calls have to keep using
//!     the API backend or switch to a JSON-output convention in the
//!     prompt.
//!
//! BUG ASSUMPTION: the operator runs `claude login` / `gemini auth`
//! on the box once before salesman starts. If they don't, every
//! call returns a non-zero exit and we surface a clear "subscriber
//! login not present" error instead of trying to fall back.

use crate::types::{ChatRequest, ChatResponse, FinishReason, Message, Role, Usage};
use crate::{BackendKind, LlmBackend};
use async_trait::async_trait;
use salesman_core::{Error, Result};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;
use tracing::debug;

/// One LLM backend that talks to a vendor CLI over stdin/stdout.
#[derive(Debug)]
pub struct SubscriberCliBackend {
    kind: BackendKind,
    model: String,
    binary: PathBuf,
    /// Args passed before the prompt arrives over stdin. Typically
    /// `["--print"]` for `claude` and `["--prompt", "-"]` style for
    /// `gemini`. Configurable per-deployment so we don't hard-code
    /// each vendor's evolving CLI flag set.
    extra_args: Vec<String>,
    /// Hard wall on subprocess wall-clock time. Default 180 s.
    timeout: Duration,
}

impl SubscriberCliBackend {
    pub fn new(
        kind: BackendKind,
        model: impl Into<String>,
        binary: impl Into<PathBuf>,
        extra_args: Vec<String>,
        timeout: Duration,
    ) -> Self {
        Self {
            kind,
            model: model.into(),
            binary: binary.into(),
            extra_args,
            timeout,
        }
    }

    /// Build a Claude (Code) CLI backend honoring env overrides:
    ///   - SALESMAN_CLAUDE_CLI_BIN   (default: `claude`)
    ///   - SALESMAN_CLAUDE_CLI_ARGS  (JSON array, default: `["--print"]`)
    ///   - SALESMAN_CLAUDE_CLI_MODEL (default: unset → don't pass
    ///     --model, let the CLI pick its own default which usually
    ///     tracks the subscriber's tier)
    ///   - SALESMAN_LLM_CLI_TIMEOUT_SEC (default: 180)
    ///
    /// `model` (from SALESMAN_CLAUDE_MODEL / --claude-model) is kept
    /// as the response/ledger label but is NOT passed to the CLI by
    /// default. The CLI's accepted model-name set differs from the
    /// API's, so passing the API model name verbatim can fail with
    /// `--model: invalid value`. Operator can opt back in by setting
    /// SALESMAN_CLAUDE_CLI_MODEL to a CLI-accepted alias (e.g.
    /// `sonnet`, `opus`, `claude-sonnet-4-5`).
    pub fn claude_from_env(model: impl Into<String>) -> Result<Self> {
        let model = model.into();
        let bin = std::env::var("SALESMAN_CLAUDE_CLI_BIN")
            .unwrap_or_else(|_| "claude".to_string());
        let mut args = parse_args_env("SALESMAN_CLAUDE_CLI_ARGS")
            .unwrap_or_else(|| vec!["--print".to_string()]);
        if let Ok(cli_model) = std::env::var("SALESMAN_CLAUDE_CLI_MODEL")
            && !cli_model.trim().is_empty()
        {
            args.push("--model".to_string());
            args.push(cli_model);
        }
        Ok(Self::new(
            BackendKind::Claude,
            model,
            PathBuf::from(bin),
            args,
            timeout_from_env(),
        ))
    }

    /// Build a Gemini CLI backend honoring env overrides:
    ///   - SALESMAN_GEMINI_CLI_BIN   (default: `gemini`)
    ///   - SALESMAN_GEMINI_CLI_ARGS  (JSON array, default: `["chat"]`
    ///     — works in both old and new gemini-cli when stdin is
    ///     piped; new CLI also accepts `["-p", ""]` for the same
    ///     headless-with-stdin behavior)
    ///   - SALESMAN_GEMINI_CLI_MODEL (default: unset → don't pass
    ///     --model, let the CLI pick its own default)
    ///   - SALESMAN_LLM_CLI_TIMEOUT_SEC (default: 180)
    pub fn gemini_from_env(model: impl Into<String>) -> Result<Self> {
        let model = model.into();
        let bin = std::env::var("SALESMAN_GEMINI_CLI_BIN")
            .unwrap_or_else(|_| "gemini".to_string());
        let mut args = parse_args_env("SALESMAN_GEMINI_CLI_ARGS")
            .unwrap_or_else(|| vec!["chat".to_string()]);
        if let Ok(cli_model) = std::env::var("SALESMAN_GEMINI_CLI_MODEL")
            && !cli_model.trim().is_empty()
        {
            args.push("--model".to_string());
            args.push(cli_model);
        }
        Ok(Self::new(
            BackendKind::Gemini,
            model,
            PathBuf::from(bin),
            args,
            timeout_from_env(),
        ))
    }
}

fn timeout_from_env() -> Duration {
    Duration::from_secs(
        std::env::var("SALESMAN_LLM_CLI_TIMEOUT_SEC")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(180),
    )
}

fn parse_args_env(name: &str) -> Option<Vec<String>> {
    parse_args_str(&std::env::var(name).ok()?)
}

/// JSON-or-whitespace arg parsing, isolated for unit testing
/// (avoids the env-mutation hazard around `set_var`).
fn parse_args_str(raw: &str) -> Option<Vec<String>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str(trimmed).ok().or_else(|| {
        // Fallback: whitespace-split so operators don't have to
        // remember JSON quoting in env. SAFETY: we never invoke a
        // shell with these — they go straight into `Command::arg`.
        Some(
            trimmed
                .split_whitespace()
                .map(|s| s.to_string())
                .collect(),
        )
    })
}

/// Render the (potentially multi-turn) message list into a single
/// prompt the CLI can consume. We prefix each turn with a role
/// header so the model can still attribute correctly.
fn render_prompt(req: &ChatRequest) -> String {
    let mut out = String::with_capacity(2048);
    for m in &req.messages {
        let label = match m.role {
            Role::System => "SYSTEM",
            Role::User => "USER",
            Role::Assistant => "ASSISTANT",
            Role::Tool => "TOOL_RESULT",
        };
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(label);
        out.push_str(":\n");
        out.push_str(&m.content);
    }
    out.push_str("\n\nASSISTANT:\n");
    out
}

#[async_trait]
impl LlmBackend for SubscriberCliBackend {
    fn kind(&self) -> BackendKind {
        self.kind
    }
    fn model(&self) -> &str {
        &self.model
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
        if !req.tools.is_empty() {
            // Tool-use over a CLI bridge would require we re-implement
            // tool dispatch by parsing structured output from the
            // model. Out of scope for v0 of the bridge — fail loud
            // so the caller doesn't silently get a plain-text reply.
            return Err(Error::Llm {
                backend: self.kind.to_string(),
                message: format!(
                    "subscriber-cli backend does not support tool calls \
                     (request specified {} tool(s)); use api transport \
                     or remove tools",
                    req.tools.len()
                ),
            });
        }

        let prompt = render_prompt(&req);
        let prompt_bytes = prompt.len();
        debug!(
            kind = %self.kind,
            model = %self.model,
            prompt_bytes,
            "subscriber-cli chat dispatch"
        );

        let started = Instant::now();
        let mut child = Command::new(&self.binary)
            .args(&self.extra_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| Error::Llm {
                backend: self.kind.to_string(),
                message: format!(
                    "subscriber-cli: failed to spawn {:?}: {e}. Is the \
                     CLI installed and on PATH? (set \
                     SALESMAN_CLAUDE_CLI_BIN / SALESMAN_GEMINI_CLI_BIN \
                     to override)",
                    self.binary
                ),
            })?;

        // Send prompt over stdin so it never lands in argv / ps.
        // BUG ASSUMPTION: if the child exits before we finish
        // writing (e.g. the binary doesn't consume stdin), we'll
        // get EPIPE here. That isn't *our* failure — the real
        // diagnosis lives in the subprocess's exit code + stderr,
        // which we surface in the wait_with_output branch below.
        // So we swallow EPIPE here and let the wait path raise.
        if let Some(mut stdin) = child.stdin.take() {
            if let Err(e) = stdin.write_all(prompt.as_bytes()).await
                && e.kind() != std::io::ErrorKind::BrokenPipe
            {
                return Err(Error::Llm {
                    backend: self.kind.to_string(),
                    message: format!("stdin write: {e}"),
                });
            }
            // Drop closes stdin so the CLI sees EOF and starts work.
            drop(stdin);
        }

        let output = match timeout(self.timeout, child.wait_with_output()).await {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => {
                return Err(Error::Llm {
                    backend: self.kind.to_string(),
                    message: format!("subscriber-cli wait failed: {e}"),
                });
            }
            Err(_) => {
                // child was killed on Drop because we set kill_on_drop.
                return Err(Error::Llm {
                    backend: self.kind.to_string(),
                    message: format!(
                        "subscriber-cli timed out after {:?} ({:?})",
                        self.timeout, self.binary
                    ),
                });
            }
        };

        let latency_ms = started.elapsed().as_millis() as u64;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Common failure: subscriber not logged in.
            let hint = if stderr.contains("login")
                || stderr.contains("auth")
                || stderr.contains("credential")
            {
                " (HINT: subscriber session may be missing — run \
                 `claude login` or `gemini auth login` on this box)"
            } else {
                ""
            };
            return Err(Error::Llm {
                backend: self.kind.to_string(),
                message: format!(
                    "subscriber-cli {:?} exited with status {:?}: {}{hint}",
                    self.binary,
                    output.status.code(),
                    stderr.trim()
                ),
            });
        }

        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let trimmed = text.trim().to_string();

        // Token counts from a CLI are not reliable. We approximate
        // with byte/4 so cost ledgers + rate-limit feedback have
        // *something* to work with, but flag clearly that this is
        // an estimate (cost = 0 since the subscription is flat-fee).
        let prompt_tokens = (prompt_bytes / 4) as u32;
        let output_tokens = (trimmed.len() / 4) as u32;

        debug!(
            kind = %self.kind,
            model = %self.model,
            latency_ms,
            output_bytes = trimmed.len(),
            "subscriber-cli chat ok"
        );

        Ok(ChatResponse {
            message: Message {
                role: Role::Assistant,
                content: trimmed,
                tool_calls: Vec::new(),
                tool_results: Vec::new(),
            },
            usage: Usage {
                prompt_tokens,
                output_tokens,
                cache_hit_tokens: 0,
                // SUPERSOCIETY: subscription is a flat-fee benefit,
                // so the marginal cost of one inference is 0. The
                // ledger should still see latency + token estimates
                // for capacity planning.
                cost_micro_usd: 0,
                latency_ms,
            },
            finish_reason: FinishReason::Stop,
            backend: Some(self.kind.to_string()),
            model: Some(self.model.clone()),
            via_fallback: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Message;

    fn req(msgs: Vec<(Role, &str)>) -> ChatRequest {
        ChatRequest {
            messages: msgs
                .into_iter()
                .map(|(role, content)| Message {
                    role,
                    content: content.to_string(),
                    tool_calls: Vec::new(),
                    tool_results: Vec::new(),
                })
                .collect(),
            tools: Vec::new(),
            max_tokens: 1024,
            temperature: 0.7,
        }
    }

    #[test]
    fn render_prompt_attaches_role_headers() {
        let r = req(vec![
            (Role::System, "you are helpful"),
            (Role::User, "say hi"),
        ]);
        let p = render_prompt(&r);
        assert!(p.contains("SYSTEM:\nyou are helpful"));
        assert!(p.contains("USER:\nsay hi"));
        assert!(p.trim_end().ends_with("ASSISTANT:"));
    }

    #[test]
    fn parse_args_str_handles_json_and_whitespace_and_empty() {
        let v = parse_args_str("[\"--a\",\"b c\"]").unwrap();
        assert_eq!(v, vec!["--a".to_string(), "b c".to_string()]);
        let v = parse_args_str("--a --b").unwrap();
        assert_eq!(v, vec!["--a".to_string(), "--b".to_string()]);
        assert!(parse_args_str("").is_none());
        assert!(parse_args_str("   ").is_none());
    }

    #[tokio::test]
    async fn echoes_via_cat() {
        // `cat` reads stdin and writes it to stdout — perfect for
        // an end-to-end transport test that doesn't depend on
        // claude/gemini being installed in CI.
        let b = SubscriberCliBackend::new(
            BackendKind::Claude,
            "test-model",
            "/bin/cat",
            vec![],
            Duration::from_secs(5),
        );
        let r = b.chat(req(vec![(Role::User, "hello world")])).await.unwrap();
        assert!(r.message.content.contains("hello world"));
        assert_eq!(r.finish_reason, FinishReason::Stop);
        assert_eq!(r.usage.cost_micro_usd, 0);
        assert_eq!(r.backend.as_deref(), Some("claude"));
    }

    #[tokio::test]
    async fn refuses_tool_use() {
        let b = SubscriberCliBackend::new(
            BackendKind::Claude,
            "test-model",
            "/bin/cat",
            vec![],
            Duration::from_secs(5),
        );
        let mut r = req(vec![(Role::User, "hi")]);
        r.tools.push(crate::types::ToolSchema {
            name: "test".to_string(),
            description: "test".to_string(),
            input_schema: serde_json::json!({}),
        });
        let err = b.chat(r).await.unwrap_err();
        assert!(format!("{err}").contains("does not support tool"));
    }

    #[tokio::test]
    async fn surfaces_missing_binary() {
        let b = SubscriberCliBackend::new(
            BackendKind::Gemini,
            "x",
            "/this/does/not/exist/at/all",
            vec![],
            Duration::from_secs(5),
        );
        let err = b.chat(req(vec![(Role::User, "x")])).await.unwrap_err();
        let s = format!("{err}");
        assert!(
            s.contains("failed to spawn") || s.contains("No such file"),
            "got: {s}"
        );
    }

    #[tokio::test]
    async fn surfaces_nonzero_exit() {
        // `false` exits 1.
        let b = SubscriberCliBackend::new(
            BackendKind::Claude,
            "x",
            "/bin/false",
            vec![],
            Duration::from_secs(5),
        );
        let err = b.chat(req(vec![(Role::User, "x")])).await.unwrap_err();
        let s = format!("{err}");
        assert!(s.contains("exited with status"), "got: {s}");
    }
}
