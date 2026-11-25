use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use company_ai::{
    classify_provider_error, load_bundle, sha256_hex, ChatTransport, DiagnosticsSource, Error,
    FixtureAdapter, FixtureBehavior, LoadedBundle, ObjectBody, ObjectReader, OllamaWorker,
    ReaderFailure, S3Diagnostics, SecurityBroker, ServiceProvider, Worker,
};
use serde_json::{json, Value};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn bundle_with_s3() -> LoadedBundle {
    let mut document: Value = serde_json::from_str(
        &std::fs::read_to_string(root().join("governance/bundle.json")).unwrap(),
    )
    .unwrap();
    document["resources"]["demo-service-a"]["s3"] =
        json!({"bucket": "demo-bucket", "key": "diagnostics/demo-service-a.json"});
    document["policy"]["identities"]["investigator"]["allow"] = json!([
        {"operation": "diagnostics.read", "resources": ["demo-service-a", "demo-malformed", "demo-oversized", "demo-timeout"]}
    ]);
    let dir = std::env::temp_dir().join(format!("company-ai-adapter-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("skills")).unwrap();
    std::fs::copy(
        root().join("governance/skills/diagnostic-investigation.md"),
        dir.join("skills/diagnostic-investigation.md"),
    )
    .unwrap();
    let path = dir.join("bundle.json");
    std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
    load_bundle(path).unwrap()
}

fn permit(
    bundle: &LoadedBundle,
    alias: &str,
    service: ServiceProvider,
) -> company_ai::AuthorizedRead {
    match SecurityBroker::new(bundle).evaluate("investigator", "diagnostics.read", alias, service) {
        Ok(decision) => match decision.authorized {
            Some(authorized) => authorized,
            None => panic!("expected a permit for {alias}, got {}", decision.reason),
        },
        Err(error) => panic!("broker failed: {error}"),
    }
}

#[test]
fn fixture_and_s3_satisfy_the_same_read_contract() {
    let bytes = std::fs::read(root().join("governance/fixtures/service-a.json")).unwrap();
    let bundle = bundle_with_s3();
    let fixture = FixtureAdapter::new(root().join("governance/fixtures"));
    let fixture_result = fixture.read(
        &permit(&bundle, "demo-service-a", ServiceProvider::Fixture),
        Duration::from_secs(2),
    );
    struct Reader(Vec<u8>);
    impl ObjectReader for Reader {
        fn get(&self, bucket: &str, key: &str, range: &str) -> Result<ObjectBody, ReaderFailure> {
            assert_eq!(bucket, "demo-bucket");
            assert_eq!(key, "diagnostics/demo-service-a.json");
            assert_eq!(range, "bytes=0-8192");
            Ok(ObjectBody {
                bytes: self.0.clone(),
                etag: Some("\"example-etag\"".to_string()),
                content_type: Some("application/json".to_string()),
            })
        }
    }
    let s3 = S3Diagnostics::injected(Arc::new(Reader(bytes.clone())));
    let s3_result = s3.read(
        &permit(&bundle, "demo-service-a", ServiceProvider::S3),
        Duration::from_secs(2),
    );
    assert!(fixture_result.ok && s3_result.ok);
    assert_eq!(fixture_result.payload, s3_result.payload);
    assert_eq!(
        fixture_result.content_sha256.as_deref(),
        Some(sha256_hex(&bytes).as_str())
    );
    assert_eq!(fixture_result.content_sha256, s3_result.content_sha256);
    assert_eq!(
        s3_result.source["provider_metadata"]["ETag"],
        "\"example-etag\""
    );
    assert!(s3_result.source["authenticity"]
        .as_str()
        .unwrap()
        .contains("not authentication"));
}

#[test]
fn fixture_rejects_escape_and_bad_documents() {
    let bundle = bundle_with_s3();
    let adapter = FixtureAdapter::new(root().join("governance/fixtures"));
    assert_eq!(
        adapter
            .read(
                &permit(&bundle, "demo-malformed", ServiceProvider::Fixture),
                Duration::from_secs(1)
            )
            .error_code
            .as_deref(),
        Some("malformed")
    );
    let oversized = adapter.read(
        &permit(&bundle, "demo-oversized", ServiceProvider::Fixture),
        Duration::from_secs(1),
    );
    assert_eq!(oversized.error_code.as_deref(), Some("oversized"));
    assert!(oversized.payload.is_none());
    assert_eq!(
        adapter
            .read(
                &permit(&bundle, "demo-timeout", ServiceProvider::Fixture),
                Duration::from_secs(1)
            )
            .error_code
            .as_deref(),
        Some("timeout")
    );
    let outside = root().join("secret.txt");
    std::fs::write(&outside, "secret-outside").unwrap();
    let mut document = serde_json::from_str::<Value>(
        &std::fs::read_to_string(root().join("governance/bundle.json")).unwrap(),
    )
    .unwrap();
    document["resources"]["demo-service-a"]["fixture"]["path"] = json!("../secret.txt");
    let dir = std::env::temp_dir().join(format!("company-ai-escape-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("skills")).unwrap();
    std::fs::copy(
        root().join("governance/skills/diagnostic-investigation.md"),
        dir.join("skills/diagnostic-investigation.md"),
    )
    .unwrap();
    let path = dir.join("bundle.json");
    std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
    assert!(load_bundle_result(&path).is_err());
    let _ = std::fs::remove_file(outside);
    let _ = FixtureBehavior::Read;
}

fn load_bundle_result(path: &std::path::Path) -> Result<LoadedBundle, Error> {
    company_ai::load_bundle(path)
}

struct ScriptedTransport {
    calls: std::sync::Mutex<Vec<Value>>,
}

impl ChatTransport for ScriptedTransport {
    fn chat(&self, url: &str, body: &Value, _timeout: Duration) -> company_ai::Result<Value> {
        assert_eq!(url, "http://127.0.0.1:11434/api/chat");
        assert_eq!(body["stream"], false);
        assert_eq!(body["think"], false);
        assert_eq!(body["options"]["num_predict"], 64);
        assert!(!body.to_string().contains("SECRET_REASONING"));
        self.calls.lock().unwrap().push(body.clone());
        if self.calls.lock().unwrap().len() == 1 {
            Ok(json!({
                "message": {
                    "role": "assistant",
                    "content": "",
                    "thinking": "SECRET_REASONING",
                    "tool_calls": [{"function": {"name": "diagnostics.read", "arguments": {"resource_alias": "demo-service-a"}}}]
                }
            }))
        } else {
            let tool = serde_json::from_str::<Value>(
                body["messages"].as_array().unwrap().last().unwrap()["content"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
            Ok(json!({
                "message": {
                    "role": "assistant",
                    "content": "",
                    "thinking": "SECRET_REASONING",
                    "tool_calls": [{"function": {"name": "submit_report", "arguments": {
                        "observations": [format!("service={}", tool["diagnostic"]["service"].as_str().unwrap())],
                        "hypotheses": ["Unproven."],
                        "missing": ["No baseline."],
                        "evidence_ids": [tool["evidence_id"]]
                    }}}]
                }
            }))
        }
    }
}

#[test]
fn ollama_worker_drops_thinking_and_uses_documented_chat_shape() {
    let transport = ScriptedTransport {
        calls: std::sync::Mutex::new(Vec::new()),
    };
    let mut worker = OllamaWorker::new(
        "http://127.0.0.1:11434",
        "configured-model",
        64,
        transport,
        true,
    );
    let context = json!({
        "mission_objective": "Investigate one alias.",
        "workflow_id": "diagnostic-investigation",
        "skill_instructions": "SKILL_SENTINEL read the alias.",
        "event": {
            "event_id": "evt-1",
            "kind": "diagnostic.requested",
            "resource_alias": "demo-service-a",
            "observed_at": "2026-11-25T12:04:00Z",
            "summary": "Synthetic."
        },
        "tools": [
            {"name": "diagnostics.read", "description": "Read one alias.", "arguments": {"type": "object"}},
            {"name": "submit_report", "description": "Submit the report.", "arguments": {"type": "object"}}
        ]
    });
    let proposal = worker.turn(&context, None).unwrap();
    assert_eq!(proposal["operation"], "diagnostics.read");
    let report = worker.turn(&context, Some(&json!({
        "status": "ok",
        "evidence_id": "ev_123",
        "diagnostic": {"service": "demo-service-a", "status": "degraded", "recent_error_count": 3, "recent_deployment_id": "deploy-1007", "timestamp": "2026-11-25T12:00:00Z"}
    }))).unwrap();
    assert_eq!(report["kind"], "final_report");
    assert_eq!(report["evidence_ids"][0], "ev_123");
    let prompts = worker.prompts();
    assert!(!prompts
        .iter()
        .any(|prompt| prompt.to_string().contains("SECRET_REASONING")));
    assert!(prompts[0]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("SKILL_SENTINEL"));
}

#[test]
fn provider_errors_are_classified_and_redacted() {
    let (code, message) = classify_provider_error(
        Some("AccessDenied"),
        "AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
    );
    assert_eq!(code, "provider_denied");
    assert!(!message.contains("wJalrXUtnFEMI"));
}

#[test]
fn malformed_model_output_fails_clearly() {
    struct Bad;
    impl ChatTransport for Bad {
        fn chat(&self, _: &str, _: &Value, _: Duration) -> company_ai::Result<Value> {
            Ok(json!({"message": {"role": "assistant", "content": "not a report"}}))
        }
    }
    let mut worker = OllamaWorker::new("http://127.0.0.1:11434", "configured-model", 32, Bad, true);
    let context = json!({
        "skill_instructions": "skill",
        "mission_objective": "Investigate.",
        "workflow_id": "diagnostic-investigation",
        "event": {"event_id": "evt-1", "resource_alias": "demo-service-a"},
        "tools": []
    });
    assert_eq!(
        worker.turn(&context, None).unwrap_err().code(),
        "malformed_model_output"
    );
}
