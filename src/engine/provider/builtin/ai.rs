use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use crate::engine::provider::{
    Entry, InitContext, Provider, ProviderMeta, ProviderResult, QueryContext, entry,
    parse_extra_config,
};

/// Time budget for the init-time reachability probe. Short, so a server that
/// is down does not stall startup.
const REACHABILITY_TIMEOUT_SECS: u64 = 3;

/// Per-provider configuration from `[engine.provider.builtin.ai.extra]`.
///
/// The provider is a two-step interaction driven by `trigger_suffix`: a query
/// that does not end with the suffix offers a single submit entry (whose
/// action appends the suffix), and a query that does end with it makes the
/// request. This keeps the (blocking) network call off every keystroke.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct AiConfig {
    /// Appended to the prompt to trigger the request. Default `?`.
    pub trigger_suffix: String,
    /// OpenAI-compatible API root, e.g. `http://localhost:11434/v1` (Ollama).
    /// The `/chat/completions` path is appended.
    pub base_url: String,
    /// Bearer token sent as `Authorization: Bearer <key>` when non-empty.
    /// Ollama and other local servers ignore it, so it may stay empty.
    pub api_key: String,
    /// Model name sent in the request body.
    pub model: String,
    /// System message steering the answer. Defaults to requesting a single
    /// short line.
    pub system_prompt: String,
    /// Sampling temperature.
    pub temperature: f32,
    /// Optional `max_tokens` cap. `None` omits the field.
    pub max_tokens: Option<u32>,
    /// Whole-request timeout in seconds.
    pub timeout_secs: u64,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            trigger_suffix: "?".into(),
            base_url: "http://localhost:11434/v1".into(),
            api_key: String::new(),
            model: "translategemma".into(),
            system_prompt: "Answer as briefly as possible, in a single line.".into(),
            temperature: 0.7,
            max_tokens: Some(256),
            timeout_secs: 30,
        }
    }
}

/// What a query resolves to under the configured trigger suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Submission<'a> {
    /// Nothing typed (or only the suffix itself): show a hint entry.
    Hint,
    /// A prompt that has not been submitted yet: offer the submit entry.
    Submit { prompt: &'a str },
    /// A prompt carrying the trigger suffix: make the request.
    Request { prompt: &'a str },
}

/// Classify the text under the prefix into a [`Submission`].
fn classify<'a>(query: &'a str, suffix: &str) -> Submission<'a> {
    let query = query.trim();
    if query.is_empty() {
        return Submission::Hint;
    }
    if !suffix.is_empty()
        && let Some(prompt) = query.strip_suffix(suffix)
    {
        let prompt = prompt.trim();
        return if prompt.is_empty() {
            Submission::Hint
        } else {
            Submission::Request { prompt }
        };
    }
    Submission::Submit { prompt: query }
}

/// Provides answers from an OpenAI-compatible `/chat/completions` endpoint,
/// defaulting to a local Ollama server (`http://localhost:11434/v1`).
///
/// Triggered by the `?` prefix. Typing a prompt offers one entry that appends
/// the configured trigger suffix (default `?`) to the query; selecting it
/// re-queries with the suffix, at which point the provider makes a blocking
/// request and returns the answer. The answer entry copies its full text to
/// the clipboard on selection.
pub struct AiProvider {
    config: AiConfig,
    agent: ureq::Agent,
    /// Last completed `(prompt, answer)`, so re-queries of the submitted prompt
    /// return the cached answer instead of calling the API again.
    cache: Option<(String, String)>,
}

impl Default for AiProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl AiProvider {
    pub fn new() -> Self {
        Self {
            config: AiConfig::default(),
            agent: ureq::agent(),
            cache: None,
        }
    }

    /// The answer for `prompt`, from the cache when possible, otherwise from a
    /// blocking request. Errors become an error entry rather than a panic.
    fn answer(&mut self, prompt: &str) -> Vec<Entry> {
        if let Some((cached_prompt, cached_answer)) = &self.cache
            && cached_prompt == prompt
        {
            return vec![answer_entry(cached_answer.clone())];
        }

        match self.complete(prompt) {
            Ok(answer) => {
                let answer = answer.trim().to_string();
                self.cache = Some((prompt.to_string(), answer.clone()));
                vec![answer_entry(answer)]
            }
            Err(e) => vec![error_entry(&e.to_string())],
        }
    }

    /// Perform the blocking chat-completion request.
    ///
    /// The agent is configured with `http_status_as_error(false)`, so an error
    /// status still yields a response: its code and body (where
    /// OpenAI-compatible servers put the reason for a 4xx/5xx) are folded into
    /// the error message shown to the user.
    fn complete(&self, prompt: &str) -> anyhow::Result<String> {
        let url = endpoint(&self.config, "/chat/completions");
        let body = build_request(&self.config, prompt);
        let mut request = self.agent.post(&url);
        if !self.config.api_key.trim().is_empty() {
            request = request.header("Authorization", &format!("Bearer {}", self.config.api_key));
        }
        let mut response = request.send_json(&body)?;
        let status = response.status();
        let text = response.body_mut().read_to_string()?;
        if !status.is_success() {
            anyhow::bail!(
                "HTTP {} {} from POST {}: {}",
                status.as_u16(),
                status.canonical_reason().unwrap_or(""),
                url,
                body_excerpt(&text),
            );
        }
        let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
            anyhow::anyhow!(
                "invalid JSON from POST {url}: {e}; body: {}",
                body_excerpt(&text)
            )
        })?;
        parse_completion(&value).ok_or_else(|| {
            anyhow::anyhow!(
                "no choices[0].message.content from POST {url}; body: {}",
                body_excerpt(&text)
            )
        })
    }

    /// Startup check: confirm the server answers, and — when it exposes a
    /// model list — that the configured model is one of them.
    ///
    /// A GET to `/models` that fails at the transport level is the only
    /// "unreachable". Any HTTP status (even 401 or 404) means the server is up;
    /// a 2xx body is then parsed for the model list and the configured model
    /// must appear in it (matching with or without a `:tag`). Servers that
    /// don't return a usable list are treated as reachable without the model
    /// check.
    fn reachable(config: &AiConfig) -> anyhow::Result<()> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(REACHABILITY_TIMEOUT_SECS)))
            .http_status_as_error(false)
            .build()
            .into();
        let url = endpoint(config, "/models");
        let mut request = agent.get(&url);
        if !config.api_key.trim().is_empty() {
            request = request.header("Authorization", &format!("Bearer {}", config.api_key));
        }
        let mut response = request.call()?;
        let status = response.status();
        let text = response.body_mut().read_to_string().unwrap_or_default();

        if !status.is_success() {
            return Ok(());
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            return Ok(());
        };
        let Some(models) = list_models(&value) else {
            return Ok(());
        };
        if models.iter().any(|m| model_matches(m, &config.model)) {
            return Ok(());
        }
        anyhow::bail!(
            "model {:?} not found; available: {}",
            config.model,
            models.join(", ")
        )
    }
}

/// The `id`s from an OpenAI-style `{"data":[{"id": ...}]}` model list, or
/// `None` when the body doesn't carry a non-empty list to check against.
fn list_models(value: &serde_json::Value) -> Option<Vec<String>> {
    let ids: Vec<String> = value
        .get("data")?
        .as_array()?
        .iter()
        .filter_map(|m| m.get("id").and_then(|id| id.as_str()).map(str::to_string))
        .collect();
    (!ids.is_empty()).then_some(ids)
}

/// Whether a listed model id satisfies the configured name, allowing the
/// server to append a tag: `translategemma` matches `translategemma:latest`.
fn model_matches(listed: &str, configured: &str) -> bool {
    listed == configured
        || listed
            .strip_prefix(configured)
            .is_some_and(|rest| rest.starts_with(':'))
}

impl Provider for AiProvider {
    fn meta(&self) -> ProviderMeta {
        // Trigger prefix: `?prompt` then Enter appends the trigger suffix.
        ProviderMeta::builder("ai")
            .name("AI")
            .prefix("?")
            .prefix_only(true)
            .build()
    }

    fn init(&mut self, ctx: InitContext) -> ProviderResult {
        match parse_extra_config::<AiConfig>(&ctx.extra) {
            Err(result) => return result,
            Ok(Some(config)) => self.config = config,
            Ok(None) => {}
        }

        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(self.config.timeout_secs)))
            .http_status_as_error(false)
            .build();
        self.agent = config.into();

        if let Err(e) = Self::reachable(&self.config) {
            return ProviderResult::Unsupported(format!(
                "cannot use AI server at {}: {e}",
                self.config.base_url
            ));
        }

        ProviderResult::Ok
    }

    fn query(&mut self, ctx: QueryContext) -> Vec<Entry> {
        match classify(ctx.query, &self.config.trigger_suffix) {
            Submission::Hint => vec![hint_entry()],
            Submission::Submit { prompt } => {
                vec![submit_entry(prompt, &self.config.trigger_suffix)]
            }
            Submission::Request { prompt } => self.answer(prompt),
        }
    }
}

/// The hint shown while nothing is typed under the prefix.
fn hint_entry() -> Entry {
    entry("ai-hint", "Type to query").subtitle("AI").score(1.0)
}

/// The submit entry: its title shows the prompt plus the suffix so the user
/// sees what selecting it will do; the action applies exactly that text.
fn submit_entry(prompt: &str, suffix: &str) -> Entry {
    let submitted = format!("{prompt}{suffix}");
    entry("ai-submit", submitted.clone())
        .subtitle("Submit")
        .action_set_query_keeping_prefix(submitted)
        .score(1.0)
}

fn answer_entry(answer: String) -> Entry {
    entry("ai-answer", answer.clone())
        .clipboard(answer)
        .score(1.0)
}

/// An error row. The full message (status, endpoint, and response body) is in
/// the title; selecting it copies that so it can be shared or inspected.
fn error_entry(message: &str) -> Entry {
    let text = format!("AI error: {message}");
    entry("ai-error", text.clone())
        .subtitle("Error")
        .clipboard(text)
        .score(1.0)
}

/// `path` joined onto the configured API root.
fn endpoint(config: &AiConfig, path: &str) -> String {
    format!("{}{path}", config.base_url.trim_end_matches('/'))
}

/// A bounded view of a response body for an error message, so a huge body
/// cannot swamp the entry.
fn body_excerpt(body: &str) -> String {
    const LIMIT: usize = 500;
    let body = body.trim();
    if body.is_empty() {
        return "<empty body>".into();
    }
    if body.chars().count() <= LIMIT {
        return body.to_string();
    }
    let head: String = body.chars().take(LIMIT).collect();
    format!("{head}…")
}

/// Build the OpenAI chat-completions request body.
fn build_request(config: &AiConfig, prompt: &str) -> serde_json::Value {
    let mut body = json!({
        "model": config.model,
        "messages": [
            { "role": "system", "content": config.system_prompt },
            { "role": "user", "content": prompt },
        ],
        "temperature": config.temperature,
    });
    if let Some(max_tokens) = config.max_tokens {
        body["max_tokens"] = json!(max_tokens);
    }
    body
}

/// Pull the assistant text out of a chat-completions response.
fn parse_completion(value: &serde_json::Value) -> Option<String> {
    value
        .get("choices")?
        .get(0)?
        .get("message")?
        .get("content")?
        .as_str()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::provider::Action;

    fn query(text: &str) -> QueryContext<'_> {
        QueryContext {
            prefix: Some("?"),
            query: text,
            original: text,
        }
    }

    #[test]
    fn prefix_is_question_mark_and_gated_on_it() {
        let meta = AiProvider::new().meta();
        assert_eq!(meta.id, "ai");
        assert_eq!(meta.prefixes, vec!["?"]);
        assert!(meta.prefix_only);
    }

    #[test]
    fn classify_distinguishes_the_states() {
        assert_eq!(classify("", "?"), Submission::Hint);
        assert_eq!(classify("   ", "?"), Submission::Hint);
        assert_eq!(classify("?", "?"), Submission::Hint);
        assert_eq!(
            classify("hello", "?"),
            Submission::Submit { prompt: "hello" }
        );
        assert_eq!(
            classify("hello?", "?"),
            Submission::Request { prompt: "hello" }
        );
        // A trailing suffix is stripped, surrounding space trimmed.
        assert_eq!(
            classify("  hi there?  ", "?"),
            Submission::Request { prompt: "hi there" }
        );
    }

    #[test]
    fn an_empty_suffix_never_submits() {
        assert_eq!(
            classify("hello?", ""),
            Submission::Submit { prompt: "hello?" }
        );
    }

    #[test]
    fn submit_entry_shows_the_suffix_as_title_and_submit_as_subtitle() {
        let entries = AiProvider::new().query(query("hello"));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].entry.title, "hello?");
        assert_eq!(entries[0].entry.subtitle.as_deref(), Some("Submit"));

        match &entries[0].entry.action {
            Action::SetQuery { suggestion } => {
                assert!(suggestion.keep_prefix);
                assert_eq!(suggestion.query, "hello?");
                assert_eq!(suggestion.resolve(Some("?")), "?hello?");
            }
            other => panic!("expected a SetQuery action, got {other:?}"),
        }
    }

    #[test]
    fn empty_prompt_offers_a_type_to_query_hint() {
        let mut provider = AiProvider::new();
        for text in ["", "   ", "?"] {
            let entries = provider.query(query(text));
            assert_eq!(entries.len(), 1, "one hint entry for {text:?}");
            assert_eq!(entries[0].entry.id, "ai-hint");
            assert_eq!(entries[0].entry.title, "Type to query");
        }
    }

    /// A cached answer is returned without touching the network, which is what
    /// keeps re-renders of the submitted prompt from firing extra requests.
    #[test]
    fn a_cached_answer_is_returned_without_a_request() {
        let mut provider = AiProvider::new();
        provider.cache = Some(("hello".into(), "world".into()));

        let entries = provider.query(query("hello?"));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].entry.title, "world");
        assert_eq!(copy_value(&entries[0]), "world");
        assert_eq!(entries[0].entry.id, "ai-answer");
    }

    fn copy_value(row: &Entry) -> String {
        let Action::Clipboard { value } = &row.entry.action else {
            panic!("expected a clipboard action, got {:?}", row.entry.action);
        };
        value.clone()
    }

    #[test]
    fn build_request_carries_model_messages_and_limits() {
        let config = AiConfig {
            model: "gpt-test".into(),
            system_prompt: "be terse".into(),
            temperature: 0.25,
            max_tokens: Some(64),
            ..AiConfig::default()
        };
        let body = build_request(&config, "ping");
        assert_eq!(body["model"], "gpt-test");
        assert_eq!(body["temperature"], 0.25);
        assert_eq!(body["max_tokens"], 64);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "be terse");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"], "ping");
    }

    #[test]
    fn build_request_omits_max_tokens_when_unset() {
        let config = AiConfig {
            max_tokens: None,
            ..AiConfig::default()
        };
        assert!(build_request(&config, "x").get("max_tokens").is_none());
    }

    #[test]
    fn parse_completion_reads_the_first_choice() {
        let value = json!({
            "choices": [
                { "message": { "role": "assistant", "content": "42" } }
            ]
        });
        assert_eq!(parse_completion(&value).as_deref(), Some("42"));
    }

    #[test]
    fn parse_completion_rejects_a_missing_choice() {
        assert_eq!(parse_completion(&json!({})), None);
        assert_eq!(parse_completion(&json!({ "choices": [] })), None);
        assert_eq!(
            parse_completion(&json!({ "choices": [{ "message": {} }] })),
            None
        );
    }

    #[test]
    fn extra_config_overrides_defaults() {
        let config: AiConfig = serde_json::from_value(json!({
            "trigger_suffix": "::",
            "base_url": "http://localhost:8080/v1",
            "api_key": "secret",
            "model": "local",
            "system_prompt": "custom",
            "temperature": 0.0,
            "max_tokens": null,
            "timeout_secs": 5
        }))
        .expect("valid config");
        assert_eq!(config.trigger_suffix, "::");
        assert_eq!(config.base_url, "http://localhost:8080/v1");
        assert_eq!(config.api_key, "secret");
        assert_eq!(config.model, "local");
        assert_eq!(config.system_prompt, "custom");
        assert_eq!(config.max_tokens, None);
        assert_eq!(config.timeout_secs, 5);
    }

    #[test]
    fn defaults_point_at_a_local_ollama_server() {
        let config = AiConfig::default();
        assert_eq!(config.base_url, "http://localhost:11434/v1");
        assert_eq!(config.model, "translategemma");
        assert!(config.api_key.is_empty());
    }

    fn config_with_base_url(base_url: &str) -> AiConfig {
        AiConfig {
            base_url: base_url.into(),
            ..AiConfig::default()
        }
    }

    /// Serve exactly one HTTP request with `status` and `body`, returning the
    /// API root to point a config at and the server thread's handle.
    fn serve_once(status: &str, body: &str) -> (String, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let handle = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (format!("http://{addr}/v1"), handle)
    }

    /// Reachability is about the transport, not the status: an error status
    /// still proves the server answered, so the probe is `Ok`.
    #[test]
    fn reachable_accepts_any_http_status() {
        let (base_url, server) = serve_once("500 Internal Server Error", "");
        let config = config_with_base_url(&base_url);
        assert!(AiProvider::reachable(&config).is_ok());
        server.join().expect("server thread");
    }

    #[test]
    fn reachable_accepts_a_tagged_model_match() {
        let body = r#"{"object":"list","data":[{"id":"translategemma:latest"}]}"#;
        let (base_url, server) = serve_once("200 OK", body);
        let config = config_with_base_url(&base_url);
        assert!(
            AiProvider::reachable(&config).is_ok(),
            "the default model translategemma matches translategemma:latest"
        );
        server.join().expect("server thread");
    }

    #[test]
    fn reachable_rejects_a_missing_model() {
        let body = r#"{"object":"list","data":[{"id":"gemma4:latest"}]}"#;
        let (base_url, server) = serve_once("200 OK", body);
        let config = AiConfig {
            model: "nope".into(),
            ..config_with_base_url(&base_url)
        };
        let err = AiProvider::reachable(&config).expect_err("the model is missing");
        assert!(err.to_string().contains("not found"), "{err}");
        assert!(err.to_string().contains("gemma4:latest"), "{err}");
        server.join().expect("server thread");
    }

    #[test]
    fn model_matches_ignores_a_server_appended_tag() {
        assert!(model_matches("translategemma:latest", "translategemma"));
        assert!(model_matches("translategemma", "translategemma"));
        assert!(model_matches("granite4.2:3b", "granite4.2:3b"));
        assert!(!model_matches("gemma4:latest", "gemma"));
        assert!(!model_matches("qwen3.8:latest", "qwen3"));
    }

    #[test]
    fn list_models_needs_a_nonempty_data_array() {
        assert_eq!(
            list_models(&json!({ "data": [{ "id": "a" }, { "id": "b" }] })),
            Some(vec!["a".to_string(), "b".to_string()])
        );
        assert_eq!(list_models(&json!({ "data": [] })), None);
        assert_eq!(list_models(&json!({ "models": [] })), None);
    }

    /// A refused connection at init disables the provider with a clear reason
    /// instead of failing later on every query.
    #[test]
    fn init_disables_the_provider_when_the_server_is_unreachable() {
        let mut provider = AiProvider::new();
        let result = provider.init(InitContext {
            data_dir: &std::env::temp_dir(),
            extra: Some(json!({ "base_url": "http://127.0.0.1:1/v1" })),
        });
        match result {
            ProviderResult::Unsupported(msg) => {
                assert!(msg.contains("cannot use AI server"), "message was {msg:?}");
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }
}
