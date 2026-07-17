//! AI integration: bring-your-own-key, two adapters (OpenAI-compatible —
//! which covers Ollama, LM Studio, OpenRouter, vLLM — and Anthropic).
//!
//! Privacy stance, enforced by construction:
//! - Config (including the API key) lives in the APP data dir, never in
//!   any vault — it can't sync or leak into backups.
//! - Every outbound request is recorded in the request log the UI can
//!   show: endpoint, model, and the exact payload. Nothing else in Onyx
//!   touches the network for AI.
//! - No key configured → no request, ever.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AiConfig {
    /// "openai" (any OpenAI-compatible endpoint) or "anthropic".
    pub provider: String,
    /// e.g. "https://api.openai.com/v1", "http://localhost:11434/v1",
    /// "https://api.anthropic.com".
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// Embedding model for RAG (e.g. "text-embedding-3-small",
    /// "nomic-embed-text"). Empty disables vault-context retrieval.
    pub embed_model: String,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            provider: "openai".into(),
            base_url: String::new(),
            api_key: String::new(),
            model: String::new(),
            embed_model: String::new(),
        }
    }
}

fn config_path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("ai.json")
}

pub fn load_config(app_data_dir: &Path) -> AiConfig {
    std::fs::read(config_path(app_data_dir))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save_config(app_data_dir: &Path, config: &AiConfig) -> Result<(), String> {
    std::fs::create_dir_all(app_data_dir).map_err(|error| error.to_string())?;
    std::fs::write(
        config_path(app_data_dir),
        serde_json::to_vec_pretty(config).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

// ---------------------------------------------------------------------------
// Request log
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiLogEntry {
    pub at_epoch_secs: u64,
    pub endpoint: String,
    pub model: String,
    /// The exact JSON body that left this machine.
    pub request_body: String,
    pub response_chars: usize,
}

#[derive(Default)]
pub struct AiLog {
    entries: Mutex<VecDeque<AiLogEntry>>,
}

impl AiLog {
    pub fn record(&self, entry: AiLogEntry) {
        let mut entries = self.entries.lock();
        if entries.len() >= 20 {
            entries.pop_front();
        }
        entries.push_back(entry);
    }

    pub fn snapshot(&self) -> Vec<AiLogEntry> {
        self.entries.lock().iter().rev().cloned().collect()
    }
}

// ---------------------------------------------------------------------------
// Chat
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String, // "user" | "assistant"
    pub content: String,
}

/// One chat completion. Blocking (callers use a worker thread).
pub fn chat(
    config: &AiConfig,
    system: Option<&str>,
    messages: &[ChatMessage],
    log: &AiLog,
) -> Result<String, String> {
    if config.api_key.is_empty() && config.provider != "openai" {
        return Err("no API key configured".into());
    }
    if config.base_url.is_empty() || config.model.is_empty() {
        return Err("AI provider is not configured (settings → AI)".into());
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|error| error.to_string())?;
    let base = config.base_url.trim_end_matches('/');

    let (endpoint, body, reply) = match config.provider.as_str() {
        "anthropic" => {
            let endpoint = format!("{base}/v1/messages");
            let body = serde_json::json!({
                "model": config.model,
                "max_tokens": 2048,
                "system": system,
                "messages": messages,
            });
            (endpoint, body, "anthropic")
        }
        _ => {
            let endpoint = format!("{base}/chat/completions");
            let mut all = Vec::with_capacity(messages.len() + 1);
            if let Some(system) = system {
                all.push(serde_json::json!({ "role": "system", "content": system }));
            }
            for message in messages {
                all.push(serde_json::json!({
                    "role": message.role,
                    "content": message.content,
                }));
            }
            let body = serde_json::json!({ "model": config.model, "messages": all });
            (endpoint, body, "openai")
        }
    };

    let body_text = body.to_string();
    let mut request = client
        .post(&endpoint)
        .header("content-type", "application/json")
        .body(body_text.clone());
    request = if reply == "anthropic" {
        request
            .header("x-api-key", &config.api_key)
            .header("anthropic-version", "2023-06-01")
    } else if config.api_key.is_empty() {
        request // local endpoints (Ollama) need no key
    } else {
        request.bearer_auth(&config.api_key)
    };

    let response = request.send().map_err(|error| error.to_string())?;
    let status = response.status();
    let payload: serde_json::Value = response.json().map_err(|error| error.to_string())?;
    if !status.is_success() {
        return Err(format!("{status}: {payload}"));
    }

    let text = if reply == "anthropic" {
        payload["content"][0]["text"].as_str().unwrap_or_default()
    } else {
        payload["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or_default()
    }
    .to_owned();

    log.record(AiLogEntry {
        at_epoch_secs: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0),
        endpoint,
        model: config.model.clone(),
        request_body: body_text,
        response_chars: text.len(),
    });

    if text.is_empty() {
        return Err(format!("empty response: {payload}"));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_roundtrip_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_config(dir.path()).provider, "openai");
        let config = AiConfig {
            provider: "anthropic".into(),
            base_url: "https://api.anthropic.com".into(),
            api_key: "sk-test".into(),
            model: "claude-sonnet-5".into(),
            embed_model: "text-embedding-3-small".into(),
        };
        save_config(dir.path(), &config).unwrap();
        let loaded = load_config(dir.path());
        assert_eq!(loaded.model, "claude-sonnet-5");
        assert_eq!(loaded.api_key, "sk-test");
    }

    #[test]
    fn log_is_a_ring_buffer_newest_first() {
        let log = AiLog::default();
        for index in 0..25 {
            log.record(AiLogEntry {
                at_epoch_secs: index,
                endpoint: "e".into(),
                model: "m".into(),
                request_body: "{}".into(),
                response_chars: 0,
            });
        }
        let snapshot = log.snapshot();
        assert_eq!(snapshot.len(), 20);
        assert_eq!(snapshot[0].at_epoch_secs, 24);
        assert_eq!(snapshot[19].at_epoch_secs, 5);
    }

    #[test]
    fn unconfigured_provider_never_sends() {
        let log = AiLog::default();
        let result = chat(&AiConfig::default(), None, &[], &log);
        assert!(result.is_err());
        assert!(log.snapshot().is_empty(), "no request may be logged/sent");
    }
}
