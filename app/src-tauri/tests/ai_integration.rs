//! Stage 2 (v0.2 rewrite) integration: prove the AI module returns a
//! clear `reachable=false` when no Ollama daemon is running, and that
//! `AiSettings` can survive a JSON round-trip across the new field set
//! (provider / per-provider model / api_key / cache_hours).

use agent_work_ledger_lib::ai::{self, AiSettings, Provider};

#[test]
fn status_when_local_ollama_not_running_is_graceful() {
    // Point at a port nothing is listening on. The probe must return
    // reachable=false plus an error string rather than panic.
    let settings = AiSettings {
        provider: Provider::Local,
        local_endpoint: "http://127.0.0.1:1".to_string(),
        ..AiSettings::default()
    };
    let s = ai::status(&settings);
    assert!(!s.reachable);
    assert!(s.error.is_some(), "expected error, got {:?}", s);
    assert_eq!(s.provider, "local");
}

#[test]
fn default_settings_use_local_provider_and_redact_on() {
    let s = AiSettings::default();
    assert_eq!(s.provider, Provider::Local);
    assert_eq!(s.local_endpoint, "http://localhost:11434");
    assert!(s.redact_prompts);
    assert_eq!(s.cache_hours, 24);
}

#[test]
fn cloud_provider_status_reports_has_api_key_flag() {
    let settings = AiSettings {
        provider: Provider::OpenaiCompat,
        api_key: "sk-test-not-real".into(),
        ..AiSettings::default()
    };
    // We don't even hit the network reliably here; just verify the
    // shape round-trips through serde and the api_key presence flag is
    // surfaced as expected. The probe itself may succeed (TCP handshake
    // to api.openai.com is feasible) or fail (no DNS) — both are
    // acceptable; we just need `has_api_key == true`.
    let s = ai::status(&settings);
    assert!(s.has_api_key);
    assert_eq!(s.provider, "openai");
    assert_eq!(s.model, "gpt-4o-mini");
}

#[test]
fn settings_roundtrip_via_temp_file() {
    let mut original = AiSettings::default();
    original.provider = Provider::Anthropic;
    original.local_endpoint = "http://example.test:9999".into();
    original.cloud_model = "test-model".into();
    original.api_key = "sk-fake".into();
    original.cache_hours = 6;
    original.redact_prompts = false;
    let raw = serde_json::to_string(&original).expect("encode");
    let round: AiSettings = serde_json::from_str(&raw).expect("decode");
    assert_eq!(round.provider, Provider::Anthropic);
    assert_eq!(round.local_endpoint, original.local_endpoint);
    assert_eq!(round.cloud_model, original.cloud_model);
    assert_eq!(round.cache_hours, 6);
    assert_eq!(round.redact_prompts, false);
    assert_eq!(round.api_key, "sk-fake");
}
