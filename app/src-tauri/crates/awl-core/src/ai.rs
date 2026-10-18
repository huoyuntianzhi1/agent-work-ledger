//! Stage 2 (v0.2 rewrite) + Stage 5.8 / v0.4.0: AI semantic layer.
//!
//! Provider abstraction with three backends:
//! - `local`           — local Ollama daemon (default for v0.1 era, still supported)
//! - `openai` (compat) — any OpenAI-compatible Chat Completions API
//!                        (cloud, requires API key). Field was renamed
//!                        from `Openai` to `OpenaiCompat` to make it
//!                        clear we accept more than just openai.com —
//!                        the serialized string stays `"openai"` for
//!                        backward compatibility with existing
//!                        `ai_settings.json` files.
//! - `anthropic`       — Anthropic Messages API (cloud, requires API key)
//!
//! Field renames in v0.4.0:
//! - `openai_endpoint` / `openai_model` → `cloud_endpoint` / `cloud_model`
//! Both keep `#[serde(alias = ...)]` so a `ai_settings.json` shipped
//! by an earlier version (with the old field names) still loads
//! without manual migration. The first time the user hits Save we
//! rewrite the file in the new shape.
//!
//! Every outbound prompt goes through `Redactor::global()` first. The
//! `redact_prompts = false` opt-out is allowed but flagged on every
//! invocation in logs so we can audit which calls left the machine
//! with raw payloads.
//!
//! Cloud keys live in `<data_dir>/ai_settings.json`. The settings IPC
//! gives the user a one-shot place to paste a key; we never persist
//! it anywhere else (no keychain integration in this stage).
//!
//! No periodic background calls. LLM is invoked only when the user
//! explicitly hits a button (summarize a session, generate a report,
//! score importance, etc.). Cache is a 24h `expires_at` column on
//! session_summaries / window_reports — re-clicking the same button
//! before expiry returns the cached result without a roundtrip.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Local,
    /// v0.4.0 rename: this used to be `Openai`. The serialized form
    /// stays `"openai"` so existing `ai_settings.json` files keep
    /// loading unchanged. Use any OpenAI-compatible endpoint — the
    /// field was renamed `cloud_endpoint` to drop the vendor
    /// suggestion.
    #[serde(rename = "openai")]
    OpenaiCompat,
    Anthropic,
}

impl Provider {
    fn as_str(&self) -> &'static str {
        match self {
            Provider::Local => "local",
            // Keep the on-disk string as `"openai"` for backward compat.
            Provider::OpenaiCompat => "openai",
            Provider::Anthropic => "anthropic",
        }
    }
    fn from_str(s: &str) -> Self {
        match s {
            "openai" => Provider::OpenaiCompat,
            "anthropic" => Provider::Anthropic,
            _ => Provider::Local,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiSettings {
    pub provider: Provider,
    pub local_endpoint: String,
    pub local_model: String,
    /// v0.4.0 rename: was `openai_endpoint`. Now accepts any
    /// OpenAI-compatible base URL (DeepSeek, Moonshot Kimi, Qwen
    /// DashScope, Doubao Ark, GLM, MiniMax, OpenAI itself, a local
    /// proxy, etc.). `#[serde(alias = "openai_endpoint")]` keeps
    /// pre-v0.4.0 settings.json files loadable. The trailing path
    /// segment (`/v1`) is the caller's responsibility: we POST to
    /// `<endpoint>/chat/completions`.
    #[serde(alias = "openai_endpoint")]
    pub cloud_endpoint: String,
    /// v0.4.0 rename: was `openai_model`.
    #[serde(alias = "openai_model")]
    pub cloud_model: String,
    /// Anthropic-compatible base URL — same idea as `cloud_endpoint`,
    /// defaults to the public Anthropic API. Custom endpoints POST to
    /// `<endpoint>/v1/messages`.
    pub anthropic_endpoint: String,
    pub anthropic_model: String,
    /// Cloud provider API key. Empty for local. Persisted in plaintext
    /// JSON inside the app data dir; the settings UI should call out
    /// the threat model (other processes running as the same user can
    /// read it). macOS Keychain integration is a future stage.
    pub api_key: String,
    /// Redact API keys / Bearer / JWT / 32-char hex from every outgoing
    /// prompt. **Default true.** Disabling this is allowed for trusted
    /// local models in a sandbox; cloud providers always force-enable
    /// it server-side regardless of this setting.
    pub redact_prompts: bool,
    /// Cache window for LLM-derived artifacts (session_summaries /
    /// window_reports). Default 24h. UI shows this.
    pub cache_hours: i64,
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            provider: Provider::Local,
            local_endpoint: "http://localhost:11434".into(),
            local_model: "gemma4:e4b".into(),
            cloud_endpoint: "https://api.openai.com/v1".into(),
            cloud_model: "gpt-4o-mini".into(),
            anthropic_endpoint: "https://api.anthropic.com".into(),
            anthropic_model: "claude-3-5-haiku-latest".into(),
            api_key: String::new(),
            redact_prompts: true,
            cache_hours: 24,
        }
    }
}

fn settings_path() -> Option<std::path::PathBuf> {
    // Inlined rather than calling `awl_db::resolve_data_dir` to avoid a
    // `awl-core ↔ awl-db` circular dependency (awl-db needs awl-core for
    // the planner hook inside migrate). The two implementations stay in
    // lock-step — see awl-db/src/lib.rs `resolve_data_dir` for the same
    // 4-line logic.
    let base = dirs::data_dir()?;
    let dir = base.join("com.minimaxcode.agentworkledger");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("ai_settings.json"))
}

pub fn load_settings() -> AiSettings {
    let Some(path) = settings_path() else {
        return AiSettings::default();
    };
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return AiSettings::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

pub fn save_settings(s: &AiSettings) -> Result<(), String> {
    let Some(path) = settings_path() else {
        return Err("could not resolve data dir".into());
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
    }
    let raw = serde_json::to_string_pretty(s).map_err(|e| format!("encode: {e}"))?;
    std::fs::write(&path, raw).map_err(|e| format!("write: {e}"))?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiStatus {
    pub reachable: bool,
    pub provider: String,
    pub endpoint: String,
    pub model: String,
    pub latency_ms: Option<i64>,
    pub installed_models: Vec<String>,
    pub error: Option<String>,
    pub has_api_key: bool,
}

/// Redact an outbound prompt respecting cloud-force-redact. Exposed so
/// callers (planner, ETL) can build their prompts and get exactly the
/// text we'd send over the wire without duplicating the policy.
pub fn redact_outbound_pub(settings: &AiSettings, prompt: &str) -> String {
    redact_outbound(settings, prompt)
}

/// Probe reachability of the *active* provider. Cloud providers
/// don't expose /models so we just confirm DNS + HTTPS handshake.
///
/// `endpoint` reflects the user-configured base URL so the status
/// card on the Settings page shows the URL the user typed (e.g. a
/// Moonshot or DeepSeek proxy) rather than always the public API.
pub fn status(settings: &AiSettings) -> AiStatus {
    let started = Instant::now();
    let (endpoint, host_port) = match settings.provider {
        Provider::Local => (
            settings.local_endpoint.clone(),
            (settings.local_endpoint.clone(), "/api/tags".to_string()),
        ),
        Provider::OpenaiCompat => {
            let ep = settings.cloud_endpoint.clone();
            let host = cloud_host_from_endpoint(&ep, "api.openai.com");
            (ep, (host, "/v1/chat/completions".into()))
        }
        Provider::Anthropic => {
            let ep = settings.anthropic_endpoint.clone();
            let host = cloud_host_from_endpoint(&ep, "api.anthropic.com");
            (ep, (host, "/v1/messages".into()))
        }
    };
    match settings.provider {
        Provider::Local => probe_local(settings, started),
        Provider::OpenaiCompat => probe_cloud_https(&host_port.0, 443, settings, &endpoint),
        Provider::Anthropic => probe_cloud_https(&host_port.0, 443, settings, &endpoint),
    }
}

/// Extract the host[:port] portion of a user-supplied cloud endpoint.
/// Falls back to the provider's public host when the input is empty or
/// not a parseable URL — the user will see "tcp connect failed" instead
/// of a confusing UI label.
fn cloud_host_from_endpoint(endpoint: &str, fallback_host: &str) -> String {
    let trimmed = endpoint.trim_end_matches('/');
    let after_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    let host_port = after_scheme.split('/').next().unwrap_or("").trim();
    if host_port.is_empty() {
        fallback_host.to_string()
    } else {
        host_port.to_string()
    }
}

fn probe_local(settings: &AiSettings, started: Instant) -> AiStatus {
    let body = serde_json::json!({});
    let elapsed = || started.elapsed().as_millis() as i64;
    let endpoint = settings.local_endpoint.clone();
    let model = settings.local_model.clone();
    let has_key = !settings.api_key.is_empty();
    match post_json::<TagsResponse>(&settings.local_endpoint, "/api/tags", &body) {
        Ok(tags) => AiStatus {
            reachable: true,
            provider: Provider::Local.as_str().into(),
            endpoint,
            model,
            latency_ms: Some(elapsed()),
            installed_models: tags.models.into_iter().map(|m| m.name).collect(),
            error: None,
            has_api_key: has_key,
        },
        Err(e) => AiStatus {
            reachable: false,
            provider: Provider::Local.as_str().into(),
            endpoint,
            model,
            latency_ms: Some(elapsed()),
            installed_models: Vec::new(),
            error: Some(e),
            has_api_key: has_key,
        },
    }
}

fn probe_cloud_https(
    host: &str,
    port: u16,
    settings: &AiSettings,
    endpoint: &str,
) -> AiStatus {
    let started = Instant::now();
    let addr = (host, port)
        .to_socket_addrs()
        .ok()
        .and_then(|mut it| it.next());
    let Some(addr) = addr else {
        return AiStatus {
            reachable: false,
            provider: settings.provider.as_str().into(),
            endpoint: endpoint.into(),
            model: model_for(settings),
            latency_ms: None,
            installed_models: Vec::new(),
            error: Some(format!("could not resolve {host}:{port}")),
            has_api_key: !settings.api_key.is_empty(),
        };
    };
    match TcpStream::connect_timeout(&addr, Duration::from_secs(3)) {
        Ok(_stream) => AiStatus {
            reachable: true,
            provider: settings.provider.as_str().into(),
            endpoint: endpoint.into(),
            model: model_for(settings),
            latency_ms: Some(started.elapsed().as_millis() as i64),
            installed_models: Vec::new(),
            error: None,
            has_api_key: !settings.api_key.is_empty(),
        },
        Err(e) => AiStatus {
            reachable: false,
            provider: settings.provider.as_str().into(),
            endpoint: endpoint.into(),
            model: model_for(settings),
            latency_ms: Some(started.elapsed().as_millis() as i64),
            installed_models: Vec::new(),
            error: Some(format!("tcp connect {addr}: {e}")),
            has_api_key: !settings.api_key.is_empty(),
        },
    }
}

fn model_for(s: &AiSettings) -> String {
    match s.provider {
        Provider::Local => s.local_model.clone(),
        Provider::OpenaiCompat => s.cloud_model.clone(),
        Provider::Anthropic => s.anthropic_model.clone(),
    }
}

// ---------------------------------------------------------------------
// Local Ollama typed protocol
// ---------------------------------------------------------------------
#[derive(Debug, Deserialize)]
struct TagsResponse {
    #[serde(default)]
    models: Vec<TagModel>,
}

#[derive(Debug, Deserialize)]
struct TagModel {
    name: String,
}

// ---------------------------------------------------------------------
// Cloud typed protocols
// ---------------------------------------------------------------------
#[derive(Debug, Serialize)]
struct OpenAiChatRequest<'a> {
    model: &'a str,
    messages: Vec<OpenAiMessage<'a>>,
    #[serde(default)]
    response_format: OpenAiResponseFormat,
    temperature: f32,
}

#[derive(Debug, Serialize)]
struct OpenAiMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Debug, Serialize, Default)]
struct OpenAiResponseFormat {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Debug, Deserialize)]
struct OpenAiChatResponse {
    choices: Vec<OpenAiChoice>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: OpenAiChoiceMessage,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoiceMessage {
    content: String,
}

#[derive(Debug, Serialize)]
struct AnthropicRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    messages: Vec<AnthropicMessage<'a>>,
}

#[derive(Debug, Serialize)]
struct AnthropicMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContent>,
}

#[derive(Debug, Deserialize)]
struct AnthropicContent {
    #[serde(default)]
    text: String,
}

// ---------------------------------------------------------------------
// Generic JSON over HTTPS helper
//
// We use `rustls` from the tauri stack? No — Tauri 2 ships rustls as a
// dependency through webview, but we still want zero new direct deps.
// HTTPS support via std is impractical; instead we shell out to `curl`
// for cloud calls only (the cloud provider ports stay 443) and keep the
// in-process std::net client for local Ollama (HTTP, port 11434).
// ---------------------------------------------------------------------
fn cloud_post_json<TResp: for<'de> Deserialize<'de>>(
    url: &str,
    api_key: &str,
    body: &serde_json::Value,
) -> Result<TResp, String> {
    let body_bytes = serde_json::to_vec(body).map_err(|e| format!("encode: {e}"))?;
    let mut cmd = std::process::Command::new("curl");
    cmd.arg("--silent")
        .arg("--show-error")
        .arg("--max-time")
        .arg("60")
        .arg("--request")
        .arg("POST")
        .arg("--header")
        .arg("Content-Type: application/json")
        .arg("--header")
        .arg(format!("Authorization: Bearer {api_key}"))
        .arg("--header")
        .arg("anthropic-version: 2023-06-01")
        .arg("--data-binary")
        .arg("@-") // read body from stdin
        .arg(url);
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Err(format!("curl spawn: {e}")),
    };
    {
        let stdin = child.stdin.as_mut().ok_or("no stdin handle")?;
        stdin
            .write_all(&body_bytes)
            .map_err(|e| format!("curl stdin write: {e}"))?;
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("curl wait: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "curl exit {}: {}",
            out.status,
            stderr.trim().chars().take(400).collect::<String>()
        ));
    }
    serde_json::from_slice::<TResp>(&out.stdout).map_err(|e| {
        format!(
            "decode JSON: {e}; body: {}",
            String::from_utf8_lossy(&out.stdout)
                .chars()
                .take(400)
                .collect::<String>()
        )
    })
}

// Local Ollama over plain HTTP via std::net. Same as v0.1 but
// trimmed and only used by the Local provider.
fn post_json<TResp: for<'de> Deserialize<'de>>(
    endpoint: &str,
    path: &str,
    body: &serde_json::Value,
) -> Result<TResp, String> {
    let parsed = parse_endpoint(endpoint, path)?;
    let addr = parsed
        .host_port
        .to_socket_addrs()
        .map_err(|e| format!("resolve {host}: {e}", host = parsed.host_port))?
        .next()
        .ok_or_else(|| format!("no addresses for {}", parsed.host_port))?;
    let started = Instant::now();
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(3))
        .map_err(|e| format!("tcp connect {addr}: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .map_err(|e| format!("set_read_timeout: {e}"))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| format!("set_write_timeout: {e}"))?;
    let body_bytes = serde_json::to_vec(body).map_err(|e| format!("encode body: {e}"))?;
    let req = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: {host}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\r\n",
        host = parsed.host_port,
        len = body_bytes.len()
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|e| format!("write request: {e}"))?;
    stream
        .write_all(&body_bytes)
        .map_err(|e| format!("write body: {e}"))?;
    stream.flush().map_err(|e| format!("flush: {e}"))?;
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| format!("read response: {e}"))?;
    let elapsed = started.elapsed();
    tracing::debug!(
        path,
        bytes = raw.len(),
        elapsed_ms = elapsed.as_millis() as i64,
        "ai: local HTTP round-trip"
    );
    let body_start = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| "no header/body separator".to_string())?
        + 4;
    let body = &raw[body_start..];
    serde_json::from_slice::<TResp>(body).map_err(|_| {
        let preview: String = String::from_utf8_lossy(body)
            .chars()
            .take(400)
            .collect();
        format!("decode JSON failed; body preview: {preview}")
    })
}

#[derive(Debug)]
struct ParsedUrl {
    host_port: String,
}

fn parse_endpoint(endpoint: &str, path: &str) -> Result<ParsedUrl, String> {
    let trimmed = endpoint.trim_end_matches('/');
    let after_scheme = trimmed
        .strip_prefix("http://")
        .ok_or_else(|| format!("only http:// endpoints supported, got {trimmed}"))?;
    Ok(ParsedUrl {
        host_port: after_scheme.to_string(),
    })
}

// ---------------------------------------------------------------------
// The single entry point everything calls. All callers go through
// `generate(prompt)` so the redaction + cache + provider routing
// stays in one place.
// ---------------------------------------------------------------------
#[derive(Debug, Serialize, Deserialize)]
pub struct GenerateRequest {
    pub prompt: String,
    /// Allow callers to request JSON-only output (used by summarize /
    /// score / window-report). When `true` the OpenAI provider is
    /// hinted with `response_format = json_object` and Anthropic gets
    /// the same instruction in the system prompt.
    #[serde(default)]
    pub json_mode: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GenerateReport {
    pub ok: bool,
    pub response: String,
    pub provider: String,
    pub model: String,
    pub duration_ms: i64,
    pub redacted_chars: i64,
    pub error: Option<String>,
}

pub fn generate(settings: &AiSettings, prompt: &str, json_mode: bool) -> GenerateReport {
    let started = Instant::now();
    let outbound = redact_outbound(settings, prompt);
    let redacted_chars = (prompt.len() as i64) - (outbound.len() as i64);
    let model = model_for(settings);
    let provider = settings.provider;
    let result = match provider {
        Provider::Local => generate_local(settings, &outbound, json_mode),
        Provider::OpenaiCompat => generate_openai(settings, &outbound, json_mode),
        Provider::Anthropic => generate_anthropic(settings, &outbound, json_mode),
    };
    GenerateReport {
        ok: result.is_ok(),
        response: result.as_ref().map(|s| s.clone()).unwrap_or_default(),
        provider: provider.as_str().into(),
        model,
        duration_ms: started.elapsed().as_millis() as i64,
        redacted_chars,
        error: result.err(),
    }
}

fn redact_outbound(settings: &AiSettings, prompt: &str) -> String {
    // Cloud providers always get redacted — even if user toggled the
    // opt-out. Local providers respect the setting.
    let force = !matches!(settings.provider, Provider::Local);
    if settings.redact_prompts || force {
        crate::redactor::Redactor::global().apply(prompt)
    } else {
        prompt.to_string()
    }
}

fn generate_local(
    settings: &AiSettings,
    prompt: &str,
    json_mode: bool,
) -> Result<String, String> {
    #[derive(Serialize)]
    struct Gen<'a> {
        model: &'a str,
        prompt: &'a str,
        #[serde(default)]
        stream: bool,
        /// Only present when `json_mode = true`. Ollama accepts `format = "json"`
        /// (string), not a JSON schema in this version of the API.
        #[serde(skip_serializing_if = "Option::is_none")]
        format: Option<&'a str>,
    }
    #[derive(Deserialize)]
    struct Resp {
        response: String,
    }
    let format_value = if json_mode { Some("json") } else { None };
    let body = serde_json::to_value(Gen {
        model: &settings.local_model,
        prompt,
        stream: false,
        format: format_value,
    })
    .map_err(|e| format!("encode local gen: {e}"))?;
    let resp: Resp = post_json(&settings.local_endpoint, "/api/generate", &body)?;
    Ok(resp.response)
}

fn generate_openai(
    settings: &AiSettings,
    prompt: &str,
    json_mode: bool,
) -> Result<String, String> {
    if settings.api_key.is_empty() {
        return Err("OpenAI api_key is empty".into());
    }
    let body = serde_json::to_value(OpenAiChatRequest {
        model: &settings.cloud_model,
        messages: vec![OpenAiMessage {
            role: "user",
            content: prompt,
        }],
        response_format: if json_mode {
            OpenAiResponseFormat {
                kind: "json_object".into(),
            }
        } else {
            OpenAiResponseFormat::default()
        },
        temperature: 0.2,
    })
    .map_err(|e| format!("encode openai: {e}"))?;
    let resp: OpenAiChatResponse = cloud_post_json(
        &format!("{}/chat/completions", settings.cloud_endpoint.trim_end_matches('/')),
        &settings.api_key,
        &body,
    )?;
    resp.choices
        .into_iter()
        .next()
        .map(|c| c.message.content)
        .ok_or_else(|| "OpenAI returned no choices".to_string())
}

fn generate_anthropic(
    settings: &AiSettings,
    prompt: &str,
    json_mode: bool,
) -> Result<String, String> {
    if settings.api_key.is_empty() {
        return Err("Anthropic api_key is empty".into());
    }
    let mut user_prompt = prompt.to_string();
    if json_mode {
        user_prompt.push_str("\n\nRespond with ONLY a JSON object, no prose or markdown fences.");
    }
    let body = serde_json::to_value(AnthropicRequest {
        model: &settings.anthropic_model,
        max_tokens: 2048,
        messages: vec![AnthropicMessage {
            role: "user",
            content: &user_prompt,
        }],
    })
    .map_err(|e| format!("encode anthropic: {e}"))?;
    let resp: AnthropicResponse = cloud_post_json(
        &format!("{}/v1/messages", settings.anthropic_endpoint.trim_end_matches('/')),
        &settings.api_key,
        &body,
    )?;
    resp.content
        .into_iter()
        .next()
        .map(|c| c.text)
        .ok_or_else(|| "Anthropic returned no content".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_have_local_provider_and_redact_on() {
        let s = AiSettings::default();
        assert_eq!(s.provider, Provider::Local);
        assert_eq!(s.local_endpoint, "http://localhost:11434");
        assert!(s.redact_prompts);
        assert_eq!(s.cache_hours, 24);
    }

    #[test]
    fn provider_round_trips_via_string() {
        for p in [Provider::Local, Provider::OpenaiCompat, Provider::Anthropic] {
            let s = p.as_str();
            assert_eq!(Provider::from_str(s), p);
        }
        assert_eq!(Provider::from_str("nonsense"), Provider::Local);
    }

    #[test]
    fn cloud_providers_force_redaction_even_when_opted_out() {
        let mut s = AiSettings::default();
        s.provider = Provider::OpenaiCompat;
        s.api_key = "sk-deadbeef".into();
        s.redact_prompts = false;
        let redacted = redact_outbound(&s, "my key is sk-abcdefghijklmnopqrstuvwxyz0123456789");
        assert!(!redacted.contains("sk-abc"));
        assert!(redacted.contains("[REDACTED:"));
    }

    #[test]
    fn parse_local_endpoint_strips_scheme() {
        let p = parse_endpoint("http://localhost:11434/", "/api/tags").unwrap();
        assert_eq!(p.host_port, "localhost:11434");
    }

    #[test]
    fn rejects_https_endpoints() {
        assert!(parse_endpoint("https://example.com/", "/api/tags").is_err());
    }

    #[test]
    fn cloud_host_from_endpoint_strips_scheme_and_path() {
        assert_eq!(
            cloud_host_from_endpoint("https://api.moonshot.cn/v1", "api.openai.com"),
            "api.moonshot.cn"
        );
        assert_eq!(
            cloud_host_from_endpoint("http://127.0.0.1:8080/v1", "api.openai.com"),
            "127.0.0.1:8080"
        );
        // Empty / malformed input falls back to the provider's public host.
        assert_eq!(cloud_host_from_endpoint("", "api.openai.com"), "api.openai.com");
        assert_eq!(
            cloud_host_from_endpoint("not-a-url", "api.anthropic.com"),
            "not-a-url"
        );
    }

    #[test]
    fn default_settings_include_cloud_endpoints() {
        let s = AiSettings::default();
        assert_eq!(s.cloud_endpoint, "https://api.openai.com/v1");
        assert_eq!(s.anthropic_endpoint, "https://api.anthropic.com");
    }

    #[test]
    fn cloud_post_url_uses_configured_endpoint() {
        // Sanity: format the same way the caller does, so a regression
        // in the path concatenation shows up here.
        let s = AiSettings {
            provider: Provider::OpenaiCompat,
            cloud_endpoint: "https://api.moonshot.cn/v1/".into(),
            ..AiSettings::default()
        };
        let url = format!(
            "{}/chat/completions",
            s.cloud_endpoint.trim_end_matches('/')
        );
        assert_eq!(url, "https://api.moonshot.cn/v1/chat/completions");
    }

    /// v0.4.0 migration: an `ai_settings.json` shipped by a pre-v0.4.0
    /// build uses the field names `openai_endpoint` / `openai_model`.
    /// The `#[serde(alias = ...)]` attribute on the new fields should
    /// keep those files loadable without manual migration.
    #[test]
    fn load_pre_v0_4_settings_with_old_field_names() {
        let legacy = r#"{
            "provider": "openai",
            "local_endpoint": "http://localhost:11434",
            "local_model": "gemma4:e4b",
            "openai_endpoint": "https://api.deepseek.com/v1",
            "openai_model": "deepseek-chat",
            "anthropic_endpoint": "https://api.anthropic.com",
            "anthropic_model": "claude-3-5-haiku-latest",
            "api_key": "sk-test",
            "redact_prompts": true,
            "cache_hours": 24
        }"#;
        let parsed: AiSettings = serde_json::from_str(legacy)
            .expect("pre-v0.4.0 settings must still parse");
        assert_eq!(parsed.cloud_endpoint, "https://api.deepseek.com/v1");
        assert_eq!(parsed.cloud_model, "deepseek-chat");
        assert_eq!(parsed.api_key, "sk-test");
        assert_eq!(parsed.provider, Provider::OpenaiCompat);
    }
}