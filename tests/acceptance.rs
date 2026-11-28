#[path = "common/mod.rs"]
mod support;

use std::fs;
use std::sync::Arc;
use std::time::Duration;

use company_ai::{
    load_bundle, main_from, require_ready, s3_retry_attempts, worker_context, ManualClock,
    ObjectBody, ObjectReader, ReaderFailure, RunPlan, S3Diagnostics, Script, ServiceMode, Store,
    WorkerMode,
};
use serde_json::json;
use support::{
    allow, bundle_path, event, fixtures, hub, investigator, root, run_case, write_bundle, Scratch,
};

const SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
const ACCESS_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

#[test]
fn fixture_command_writes_an_evidence_backed_report() {
    let scratch = Scratch::new();
    let output = scratch.path().join("demo");
    let code = main_from([
        "company-ai",
        "demo",
        "--output",
        output.to_str().unwrap(),
        "--bundle",
        bundle_path().to_str().unwrap(),
        "--event",
        root()
            .join("examples/events/diagnostic-demo-service-a.json")
            .to_str()
            .unwrap(),
        "--fixture-dir",
        fixtures().to_str().unwrap(),
    ]);
    assert_eq!(code, 0);
    let report_path = fs::read_dir(output.join("reports"))
        .unwrap()
        .find_map(|entry| {
            let path = entry.unwrap().path();
            path.extension()
                .map(|ext| ext == "json")
                .unwrap_or(false)
                .then_some(path)
        })
        .unwrap();
    let document: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&report_path).unwrap()).unwrap();
    assert_eq!(document["status"], "succeeded");
    assert_eq!(document["worker_mode"], "scripted");
    assert_eq!(document["service_mode"], "fixture");
    assert!(document["model_id"].is_null());
    assert_eq!(document["governance"]["policy_version"], "2026.11.25.1");
    assert_eq!(
        document["governance"]["cursor_hub_commit"],
        "14b375bbc77c0dfb15d03cb26f1231c3b5d5d0da"
    );
    assert_eq!(document["completion"]["passed"], true);
    assert_eq!(document["authorization_decisions"][0]["allowed"], true);
    assert_eq!(
        document["recorded_facts"][0]["payload"]["service"],
        "demo-service-a"
    );
    assert_eq!(document["recorded_facts"][0]["run_id"], document["run_id"]);
    assert!(document["model_report"]["hypotheses"][0]
        .as_str()
        .unwrap()
        .contains("does not establish"));
    assert!(!document["model_report"]["missing"]
        .as_array()
        .unwrap()
        .is_empty());
    let limitations = document["limitations"].to_string();
    assert!(limitations.contains("do not prove that the diagnosis is true"));
    let markdown = fs::read_to_string(report_path.with_extension("md")).unwrap();
    assert!(markdown.contains("Model hypotheses") && markdown.contains("Recorded facts"));
    assert_eq!(
        main_from([
            "company-ai",
            "demo",
            "--output",
            output.to_str().unwrap(),
            "--bundle",
            bundle_path().to_str().unwrap(),
            "--event",
            root()
                .join("examples/events/diagnostic-demo-service-a.json")
                .to_str()
                .unwrap(),
            "--fixture-dir",
            fixtures().to_str().unwrap(),
        ]),
        0
    );
    let store = Store::open(output.join("state.sqlite"), true).unwrap();
    let runs: i64 = store_count(&store, "SELECT COUNT(*) FROM runs");
    let evidence: i64 = store_count(&store, "SELECT COUNT(*) FROM evidence");
    assert_eq!((runs, evidence), (1, 1));
}

#[test]
fn allowed_read_is_recorded_before_execution() {
    let scratch = Scratch::new();
    let run = run_case(
        scratch.path(),
        &bundle_path(),
        Script::Investigate,
        investigator(false),
        event(
            "evt-allowed",
            "demo-service-a",
            "Synthetic diagnostic request.",
        ),
        None,
    );
    assert_eq!(run.outcome.status, "succeeded");
    assert_eq!(run.calls, ["demo-service-a"]);
    assert!(run.saw_started);
    let store = Store::open(&run.database, true).unwrap();
    let payload: String = query(&store, "SELECT payload_json FROM evidence");
    assert!(payload.contains("\"recent_error_count\":3"));
}

#[test]
fn forbidden_resource_does_not_call_the_adapter() {
    let scratch = Scratch::new();
    let run = run_case(
        scratch.path(),
        &bundle_path(),
        Script::Forbidden,
        investigator(false),
        event(
            "evt-forbidden",
            "demo-service-a",
            "Synthetic diagnostic request.",
        ),
        None,
    );
    assert_ne!(run.outcome.status, "succeeded");
    assert_eq!(
        run.outcome.failure_code.as_deref(),
        Some("missing_evidence")
    );
    assert!(run.calls.is_empty());
    let store = Store::open(&run.database, true).unwrap();
    let (allowed, reason, alias): (i64, String, String) = store_query3(
        &store,
        "SELECT allowed, reason, resource_alias FROM decisions",
    );
    assert_eq!(allowed, 0);
    assert_eq!(reason, "resource_not_allowed");
    assert_eq!(alias, "demo-service-b");
    assert_eq!(
        store_count(
            &store,
            "SELECT COUNT(*) FROM steps WHERE kind='provider_attempt'"
        ),
        0
    );
}

#[test]
fn identity_changes_permission_and_spoofing_does_not() {
    let scratch = Scratch::new();
    let denied = run_case(
        &scratch.path().join("inv"),
        &bundle_path(),
        Script::Investigate,
        investigator(false),
        event(
            "evt-b-inv",
            "demo-service-b",
            "Synthetic diagnostic request.",
        ),
        None,
    );
    assert_ne!(denied.outcome.status, "succeeded");
    assert!(denied.calls.is_empty());

    let mut auditor = investigator(false);
    auditor.identity = "auditor".to_string();
    let allowed = run_case(
        &scratch.path().join("aud"),
        &bundle_path(),
        Script::Investigate,
        auditor,
        event(
            "evt-b-aud",
            "demo-service-b",
            "Synthetic diagnostic request.",
        ),
        None,
    );
    assert_eq!(allowed.outcome.status, "succeeded");
    assert_eq!(allowed.calls, ["demo-service-b"]);

    let spoof = run_case(
        &scratch.path().join("spoof"),
        &bundle_path(),
        Script::SpoofIdentity,
        investigator(false),
        event(
            "evt-spoof",
            "demo-service-a",
            "Synthetic diagnostic request.",
        ),
        None,
    );
    assert_eq!(
        spoof.outcome.failure_code.as_deref(),
        Some("rejected_untrusted_field")
    );
    assert!(spoof.calls.is_empty());
}

#[test]
fn hostile_provider_text_cannot_change_policy() {
    let original = fs::read(bundle_path()).unwrap();
    let scratch = Scratch::new();
    let bundle = write_bundle(scratch.path(), None, |document| {
        allow(
            document,
            "investigator",
            &["demo-service-a", "demo-hostile"],
        )
    });
    let before = load_bundle(&bundle).unwrap();
    let run = run_case(
        scratch.path(),
        &bundle,
        Script::HostileFollowup,
        investigator(false),
        event(
            "evt-hostile",
            "demo-hostile",
            "Synthetic diagnostic request.",
        ),
        None,
    );
    let after = load_bundle(&bundle).unwrap();
    assert_eq!(run.outcome.status, "succeeded");
    assert_eq!(run.calls, ["demo-hostile"]);
    let store = Store::open(&run.database, true).unwrap();
    let denied: i64 = store_count(
        &store,
        "SELECT COUNT(*) FROM decisions WHERE resource_alias='demo-service-b' AND allowed=0",
    );
    assert_eq!(denied, 1);
    assert_eq!(before.policy_version(), after.policy_version());
    assert_eq!(
        before.limits().max_provider_attempts,
        after.limits().max_provider_attempts
    );
    let report = fs::read_to_string(run.outcome.report_json_path.unwrap()).unwrap();
    assert!(report.contains("treated as untrusted data"));
    let document: serde_json::Value = serde_json::from_str(&report).unwrap();
    let observations = document["model_report"]["observations"].to_string();
    assert!(observations.contains("untrusted data"));
    assert!(!observations.contains("auditor"));
    assert_eq!(fs::read(bundle_path()).unwrap(), original);
}

#[test]
fn policy_change_applies_to_a_new_run_only() {
    let scratch = Scratch::new();
    let first = run_case(
        scratch.path(),
        &bundle_path(),
        Script::Investigate,
        investigator(false),
        event(
            "evt-policy-v1",
            "demo-service-a",
            "Synthetic diagnostic request.",
        ),
        None,
    );
    assert_eq!(first.outcome.status, "succeeded");
    let changed = write_bundle(&scratch.path().join("v2"), None, |document| {
        document["policy"]["policy_version"] = json!("2026.11.25.2");
        allow(document, "investigator", &["demo-service-b"]);
    });
    let second = run_case(
        &scratch.path().join("v2"),
        &changed,
        Script::Investigate,
        investigator(false),
        event(
            "evt-policy-v2",
            "demo-service-a",
            "Synthetic diagnostic request.",
        ),
        None,
    );
    assert!(second.calls.is_empty());
    assert_ne!(second.outcome.status, "succeeded");
    let store = Store::open(&second.database, true).unwrap();
    let (version, reason): (String, String) =
        store_query2(&store, "SELECT policy_version, reason FROM decisions");
    assert_eq!(version, "2026.11.25.2");
    assert_eq!(reason, "resource_not_allowed");
    assert_ne!(first.outcome.run_id, second.outcome.run_id);
}

#[test]
fn duplicate_event_is_reused_and_conflict_is_rejected() {
    let scratch = Scratch::new();
    let first = run_case(
        scratch.path(),
        &bundle_path(),
        Script::Investigate,
        investigator(false),
        event("evt-dup", "demo-service-a", "Synthetic diagnostic request."),
        None,
    );
    let mut store = Store::open(&first.database, false).unwrap();
    let bundle = load_bundle(bundle_path()).unwrap();
    let adapter = support::Counting::new(first.database.clone());
    let mut worker = company_ai::ScriptedWorker::new(Script::Investigate);
    let clock = company_ai::SystemClock::new();
    let mut runtime = company_ai::Runtime::new(
        &mut store,
        &bundle,
        &adapter,
        &mut worker,
        scratch.path().join("reports"),
        investigator(false),
        &clock,
    )
    .unwrap();
    let second = runtime
        .handle_event(event(
            "evt-dup",
            "demo-service-a",
            "Synthetic diagnostic request.",
        ))
        .unwrap();
    assert!(second.reused);
    assert_eq!(second.run_id, first.outcome.run_id);
    assert!(adapter.calls().is_empty());
    match runtime.handle_event(event(
        "evt-dup",
        "demo-service-a",
        "Different synthetic content.",
    )) {
        Err(error) => assert_eq!(error.code(), "conflict"),
        Ok(_) => panic!("expected a conflicting event to be rejected"),
    }
}

#[test]
fn malformed_unknown_invalid_and_oversized_fail() {
    let scratch = Scratch::new();
    let malformed = run_case(
        scratch.path(),
        &bundle_path(),
        Script::Malformed,
        investigator(false),
        event(
            "evt-malformed-model",
            "demo-service-a",
            "Synthetic diagnostic request.",
        ),
        None,
    );
    assert_eq!(
        malformed.outcome.failure_code.as_deref(),
        Some("malformed_model_output")
    );
    assert!(malformed.calls.is_empty());
    let unknown = run_case(
        &scratch.path().join("tool"),
        &bundle_path(),
        Script::UnknownTool,
        investigator(false),
        event(
            "evt-unknown-tool",
            "demo-service-a",
            "Synthetic diagnostic request.",
        ),
        None,
    );
    assert_eq!(
        unknown.outcome.failure_code.as_deref(),
        Some("unknown_tool")
    );
    assert!(unknown.calls.is_empty());
    let invalid = write_bundle(&scratch.path().join("policy"), None, |document| {
        document["policy"]["allow_all"] = json!(true);
    });
    assert!(load_bundle(invalid).is_err());
    let bundle = write_bundle(&scratch.path().join("docs"), None, |document| {
        allow(
            document,
            "investigator",
            &["demo-service-a", "demo-malformed", "demo-oversized"],
        );
    });
    let bad = run_case(
        &scratch.path().join("docs"),
        &bundle,
        Script::Investigate,
        investigator(false),
        event(
            "evt-bad-doc",
            "demo-malformed",
            "Synthetic diagnostic request.",
        ),
        None,
    );
    let big = run_case(
        &scratch.path().join("big"),
        &bundle,
        Script::Investigate,
        investigator(false),
        event(
            "evt-big-doc",
            "demo-oversized",
            "Synthetic diagnostic request.",
        ),
        None,
    );
    assert_eq!(bad.outcome.failure_code.as_deref(), Some("malformed"));
    assert_eq!(big.outcome.failure_code.as_deref(), Some("oversized"));
    assert_eq!(big.calls, ["demo-oversized"]);
    let store = Store::open(&big.database, true).unwrap();
    assert_eq!(store_count(&store, "SELECT COUNT(*) FROM evidence"), 0);
}

#[test]
fn timeout_denial_and_budget_do_not_succeed() {
    let scratch = Scratch::new();
    let bundle = write_bundle(scratch.path(), None, |document| {
        allow(document, "investigator", &["demo-timeout"])
    });
    let timed = run_case(
        scratch.path(),
        &bundle,
        Script::Investigate,
        investigator(false),
        event(
            "evt-timeout",
            "demo-timeout",
            "Synthetic diagnostic request.",
        ),
        None,
    );
    assert_eq!(timed.outcome.failure_code.as_deref(), Some("timeout"));
    assert_eq!(timed.calls, ["demo-timeout", "demo-timeout"]);
    let store = Store::open(&timed.database, true).unwrap();
    assert_eq!(store_count(&store, "SELECT COUNT(*) FROM evidence"), 0);
    let budget: String = query(&store, "SELECT budget_json FROM runs");
    assert!(budget.contains("\"provider_attempts\":2"));

    let denied = run_case(
        &scratch.path().join("budget"),
        &bundle_path(),
        Script::DenyLoop,
        investigator(false),
        event(
            "evt-budget",
            "demo-service-a",
            "Synthetic diagnostic request.",
        ),
        None,
    );
    assert_eq!(
        denied.outcome.failure_code.as_deref(),
        Some("budget_exhausted")
    );
    assert!(denied.calls.is_empty());

    let shared = Arc::new(ManualClock::default());
    let expired = run_case(
        &scratch.path().join("clock2"),
        &bundle_path(),
        Script::Expire(Arc::clone(&shared)),
        investigator(false),
        event(
            "evt-expire",
            "demo-service-a",
            "Synthetic diagnostic request.",
        ),
        Some(shared.as_ref()),
    );
    assert_eq!(
        expired.outcome.failure_code.as_deref(),
        Some("deadline_exceeded")
    );
    assert!(expired.calls.is_empty());
    let _ = expired;
}

#[test]
fn interrupted_work_is_inspectable_and_replay_is_local() {
    let scratch = Scratch::new();
    let database = scratch.path().join("state.sqlite");
    let store = Store::open(&database, false).unwrap();
    let bundle = load_bundle(bundle_path()).unwrap();
    let happened = event(
        "evt-interrupt",
        "demo-service-a",
        "Synthetic diagnostic request.",
    );
    drop(store);
    let mut store = Store::open(&database, false).unwrap();
    let plan = company_ai::AdmitPlan {
        workflow_id: bundle.document.workflow.workflow_id.clone(),
        mission_id: bundle.document.mission.mission_id.clone(),
        policy_version: bundle.policy_version().to_string(),
        bundle_id: bundle.document.bundle_id.clone(),
        bundle_version: bundle.document.bundle_version.clone(),
        bundle_fingerprint: bundle.fingerprint.clone(),
        worker_mode: "scripted".to_string(),
        service_mode: "fixture".to_string(),
        worker_adapter: "scripted".to_string(),
        service_adapter: "fixture".to_string(),
        model_id: None,
    };
    let budget = company_ai::Budget::new(bundle.limits().clone());
    let (run_id, reused) = store
        .admit(&happened, "investigator", &plan, &budget)
        .unwrap();
    assert!(!reused);
    store.start_step(&run_id, "provider_attempt", None).unwrap();
    drop(store);
    let mut inspect = Store::open(&database, false).unwrap();
    let history = company_ai::inspect_run(&mut inspect, &run_id).unwrap();
    assert_eq!(history["status"], "interrupted");
    assert!(history["evidence_ids"].as_array().unwrap().is_empty());
    drop(inspect);
    let replay = Store::open(&database, true).unwrap();
    let replayed = company_ai::replay_run(&replay, &run_id).unwrap();
    assert_eq!(replayed["external_calls"], 0);
    assert_eq!(replayed["status"], "interrupted");
    drop(replay);
    let mut again = Store::open(&database, false).unwrap();
    let adapter = support::Counting::new(database.clone());
    let mut worker = company_ai::ScriptedWorker::new(Script::Investigate);
    let clock = company_ai::SystemClock::new();
    let mut runtime = company_ai::Runtime::new(
        &mut again,
        &bundle,
        &adapter,
        &mut worker,
        scratch.path().join("reports"),
        investigator(false),
        &clock,
    )
    .unwrap();
    let outcome = runtime.handle_event(happened).unwrap();
    assert!(outcome.reused);
    assert_eq!(outcome.status, "interrupted");
    assert!(adapter.calls().is_empty());
}

#[test]
fn unknown_and_cross_run_evidence_cannot_complete() {
    let scratch = Scratch::new();
    let first = run_case(
        scratch.path(),
        &bundle_path(),
        Script::Investigate,
        investigator(false),
        event(
            "evt-evidence-source",
            "demo-service-a",
            "Synthetic diagnostic request.",
        ),
        None,
    );
    let store = Store::open(&first.database, true).unwrap();
    let evidence_id: String = query(&store, "SELECT evidence_id FROM evidence");
    drop(store);
    let cross = run_case(
        &scratch.path().join("cross"),
        &bundle_path(),
        Script::Cite(evidence_id.clone()),
        investigator(false),
        event(
            "evt-cross",
            "demo-service-a",
            "Synthetic diagnostic request.",
        ),
        None,
    );
    assert_eq!(
        cross.outcome.failure_code.as_deref(),
        Some("invalid_evidence")
    );
    assert!(cross.calls.is_empty());
    let missing = run_case(
        &scratch.path().join("missing"),
        &bundle_path(),
        Script::Cite("ev_missing".to_string()),
        investigator(false),
        event(
            "evt-missing-evidence",
            "demo-service-a",
            "Synthetic diagnostic request.",
        ),
        None,
    );
    assert_eq!(
        missing.outcome.failure_code.as_deref(),
        Some("invalid_evidence")
    );
    let mut same = Store::open(&first.database, false).unwrap();
    let bundle = load_bundle(bundle_path()).unwrap();
    let adapter = support::Counting::new(first.database.clone());
    let mut worker = company_ai::ScriptedWorker::new(Script::Cite(evidence_id));
    let clock = company_ai::SystemClock::new();
    let mut runtime = company_ai::Runtime::new(
        &mut same,
        &bundle,
        &adapter,
        &mut worker,
        scratch.path().join("reports2"),
        investigator(false),
        &clock,
    )
    .unwrap();
    let outcome = runtime
        .handle_event(event(
            "evt-cross-same-db",
            "demo-service-a",
            "Synthetic diagnostic request.",
        ))
        .unwrap();
    assert_eq!(outcome.failure_code.as_deref(), Some("invalid_evidence"));
}

#[test]
fn live_modes_report_missing_settings_and_do_not_fall_back() {
    let scratch = Scratch::new();
    let code = main_from([
        "company-ai",
        "run",
        "--config",
        root()
            .join("examples/config.example.toml")
            .to_str()
            .unwrap(),
        "--event",
        root()
            .join("examples/events/diagnostic-demo-service-a.json")
            .to_str()
            .unwrap(),
        "--service",
        "s3",
        "--bundle",
        bundle_path().to_str().unwrap(),
        "--database",
        scratch.path().join("state.sqlite").to_str().unwrap(),
        "--reports-dir",
        scratch.path().join("reports").to_str().unwrap(),
        "--fixture-dir",
        fixtures().to_str().unwrap(),
    ]);
    assert_eq!(code, 2);
    let bundle = load_bundle(bundle_path()).unwrap();
    let event = support::event(
        "evt-demo-service-a-001",
        "demo-service-a",
        "Synthetic diagnostic request for demo-service-a.",
    );
    let mut settings = example_settings(scratch.path(), true);
    settings.service_mode = "s3".to_string();
    let error = require_ready(&settings, &bundle, Some(&event)).unwrap_err();
    assert!(!error.missing().contains(&"--live".to_string()));
    assert!(error
        .missing()
        .iter()
        .any(|item| item == "service.s3.profile"));
    assert!(error
        .missing()
        .iter()
        .any(|item| item == "service.s3.region"));
    assert!(error
        .missing()
        .iter()
        .any(|item| item == "resources.demo-service-a.s3.bucket"));
    assert!(error.message().contains("Fixture mode was not substituted"));
    let mut ollama = example_settings(scratch.path(), false);
    ollama.worker_mode = "ollama".to_string();
    let error = require_ready(&ollama, &bundle, Some(&event)).unwrap_err();
    assert!(error.missing().contains(&"--live".to_string()));
    assert!(error.missing().contains(&"worker.model".to_string()));
}

#[test]
fn credentials_do_not_enter_worker_context_or_audit() {
    let scratch = Scratch::new();
    std::env::set_var("AWS_SECRET_ACCESS_KEY", SECRET);
    std::env::set_var("AWS_ACCESS_KEY_ID", ACCESS_KEY);
    let run = run_case(
        scratch.path(),
        &bundle_path(),
        Script::Investigate,
        investigator(false),
        event(
            "evt-secrets",
            "demo-service-a",
            "Synthetic diagnostic request.",
        ),
        None,
    );
    let prompts = serde_json::to_string(&run.prompts).unwrap();
    let report = fs::read_to_string(run.outcome.report_json_path.unwrap()).unwrap();
    let database = fs::read(&run.database).unwrap();
    for secret in [SECRET, ACCESS_KEY] {
        assert!(!prompts.contains(secret));
        assert!(!report.contains(secret));
        assert!(!database
            .windows(secret.len())
            .any(|window| window == secret.as_bytes()));
    }
    let bundle = load_bundle(bundle_path()).unwrap();
    let context = worker_context(
        &bundle,
        &event(
            "evt-context",
            "demo-service-a",
            "Synthetic diagnostic request.",
        ),
        &[],
    );
    let rendered = context.to_string();
    assert!(!rendered.contains("Prefer incremental changes"));
    assert!(!rendered.contains("policy_version"));
    assert!(!rendered.contains("YOUR_BUCKET"));
    let config = scratch.path().join("secret.toml");
    fs::write(
        &config,
        fs::read_to_string(root().join("examples/config.example.toml")).unwrap()
            + &format!("\nsecret_access_key = \"{SECRET}\"\n"),
    )
    .unwrap();
    let error = company_ai::load_config(&config).unwrap_err();
    assert!(!error.message().contains(SECRET));
}

#[test]
fn provenance_matches_the_hub_and_skill_text_is_loaded() {
    let bundle = load_bundle(bundle_path()).unwrap();
    assert_eq!(
        bundle.document.provenance.commit,
        "14b375bbc77c0dfb15d03cb26f1231c3b5d5d0da"
    );
    if !hub().is_dir() {
        return;
    }
    assert!(company_ai::verify_provenance(&bundle, &hub()).is_empty());
    let scratch = Scratch::new();
    let alpha = write_bundle(
        &scratch.path().join("alpha"),
        Some("ALPHA_SKILL_SENTINEL investigate the alias.\n"),
        |_| {},
    );
    let beta = write_bundle(
        &scratch.path().join("beta"),
        Some("BETA_SKILL_SENTINEL investigate the alias.\n"),
        |_| {},
    );
    let alpha_run = run_case(
        &scratch.path().join("alpha"),
        &alpha,
        Script::Investigate,
        investigator(false),
        event(
            "evt-skill-alpha",
            "demo-service-a",
            "Synthetic diagnostic request.",
        ),
        None,
    );
    let beta_run = run_case(
        &scratch.path().join("beta"),
        &beta,
        Script::Investigate,
        investigator(false),
        event(
            "evt-skill-beta",
            "demo-service-a",
            "Synthetic diagnostic request.",
        ),
        None,
    );
    let alpha_text = serde_json::to_string(&alpha_run.prompts).unwrap();
    let beta_text = serde_json::to_string(&beta_run.prompts).unwrap();
    assert!(
        alpha_text.contains("ALPHA_SKILL_SENTINEL") && !alpha_text.contains("BETA_SKILL_SENTINEL")
    );
    assert!(
        beta_text.contains("BETA_SKILL_SENTINEL") && !beta_text.contains("ALPHA_SKILL_SENTINEL")
    );
    assert!(!alpha_text.contains("Prefer incremental changes"));
    assert!(!alpha_text.contains("Present drafts before writing"));
}

#[test]
fn request_timeout_is_capped_by_remaining_duration() {
    let scratch = Scratch::new();
    let clock = ManualClock::default();
    clock.set(Duration::from_secs(28));
    let run = run_case(
        scratch.path(),
        &bundle_path(),
        Script::Investigate,
        investigator(false),
        event(
            "evt-timeout-cap",
            "demo-service-a",
            "Synthetic diagnostic request.",
        ),
        Some(&clock),
    );
    assert_eq!(run.outcome.status, "succeeded");
    assert!((run.timeouts[0] - 2.0).abs() < 0.01);
}

#[test]
fn bucket_override_is_rejected() {
    let scratch = Scratch::new();
    let run = run_case(
        scratch.path(),
        &bundle_path(),
        Script::BucketOverride,
        investigator(false),
        event(
            "evt-bucket",
            "demo-service-a",
            "Synthetic diagnostic request.",
        ),
        None,
    );
    assert_eq!(
        run.outcome.failure_code.as_deref(),
        Some("rejected_untrusted_field")
    );
    assert!(run.calls.is_empty());
}

#[test]
fn s3_denied_provider_access_fails_without_evidence() {
    let scratch = Scratch::new();
    let bundle = write_bundle(scratch.path(), None, |document| {
        document["resources"]["demo-service-a"]["s3"] =
            json!({"bucket": "demo-bucket", "key": "diagnostics/demo-service-a.json"});
    });
    struct Denied;
    impl ObjectReader for Denied {
        fn get(&self, bucket: &str, key: &str, _range: &str) -> Result<ObjectBody, ReaderFailure> {
            assert_eq!(bucket, "demo-bucket");
            assert_eq!(key, "diagnostics/demo-service-a.json");
            Err(ReaderFailure {
                code: Some("AccessDenied".to_string()),
                message: format!("AWS_SECRET_ACCESS_KEY={SECRET}"),
            })
        }
    }
    let loaded = load_bundle(&bundle).unwrap();
    let adapter = S3Diagnostics::injected(Arc::new(Denied));
    let mut store = Store::open(scratch.path().join("state.sqlite"), false).unwrap();
    let mut worker = company_ai::ScriptedWorker::new(Script::Investigate);
    let clock = company_ai::SystemClock::new();
    let mut plan = investigator(true);
    plan.service_mode = ServiceMode::Live;
    let mut runtime = company_ai::Runtime::new(
        &mut store,
        &loaded,
        &adapter,
        &mut worker,
        scratch.path().join("reports"),
        plan,
        &clock,
    )
    .unwrap();
    let outcome = runtime
        .handle_event(event(
            "evt-s3-denied",
            "demo-service-a",
            "Synthetic diagnostic request.",
        ))
        .unwrap();
    assert_eq!(outcome.failure_code.as_deref(), Some("provider_denied"));
    let bytes = fs::read(scratch.path().join("state.sqlite")).unwrap();
    assert!(!bytes
        .windows(SECRET.len())
        .any(|window| window == SECRET.as_bytes()));
    assert_eq!(
        store_count(
            &Store::open(scratch.path().join("state.sqlite"), true).unwrap(),
            "SELECT COUNT(*) FROM evidence"
        ),
        0
    );
}

#[test]
fn validate_command_checks_the_recorded_hub() {
    if !hub().is_dir() {
        return;
    }
    let code = main_from([
        "company-ai",
        "validate",
        "--bundle",
        bundle_path().to_str().unwrap(),
        "--fixture-dir",
        fixtures().to_str().unwrap(),
        "--config",
        root()
            .join("examples/config.example.toml")
            .to_str()
            .unwrap(),
        "--hub",
        hub().to_str().unwrap(),
    ]);
    assert_eq!(code, 0);
}

#[test]
fn live_s3_without_opt_in_is_rejected() {
    let scratch = Scratch::new();
    let bundle = load_bundle(bundle_path()).unwrap();
    let adapter = S3Diagnostics::injected(Arc::new(EmptyReader));
    let mut store = Store::open(scratch.path().join("state.sqlite"), false).unwrap();
    let mut worker = company_ai::ScriptedWorker::new(Script::Investigate);
    let clock = company_ai::SystemClock::new();
    let plan = RunPlan {
        identity: "investigator".to_string(),
        worker_mode: WorkerMode::Scripted,
        service_mode: ServiceMode::Live,
        live: false,
        model_id: None,
    };
    let error = match company_ai::Runtime::new(
        &mut store,
        &bundle,
        &adapter,
        &mut worker,
        scratch.path().join("reports"),
        plan,
        &clock,
    ) {
        Err(error) => error,
        Ok(_) => panic!("expected live opt-in to be required"),
    };
    assert_eq!(error.missing(), ["--live"]);
    assert!(error.message().contains("not substituted"));
}

#[test]
fn sdk_retry_configuration_disables_hidden_retries() {
    assert_eq!(s3_retry_attempts(), 1);
}

#[test]
fn example_config_live_check_selects_nothing_live() {
    let _guard = support::lock_dir();
    let original = std::env::current_dir().unwrap();
    std::env::set_current_dir(root()).unwrap();
    let code = main_from([
        "company-ai",
        "live",
        "check",
        "--config",
        "examples/config.example.toml",
    ]);
    std::env::set_current_dir(original).unwrap();
    assert_eq!(code, 0);
}

struct EmptyReader;
impl ObjectReader for EmptyReader {
    fn get(&self, _: &str, _: &str, _: &str) -> Result<ObjectBody, ReaderFailure> {
        Err(ReaderFailure {
            code: Some("provider_error".to_string()),
            message: "unused".to_string(),
        })
    }
}

fn example_settings(dir: &std::path::Path, live: bool) -> company_ai::Settings {
    company_ai::settings_from_config(
        company_ai::load_config(&root().join("examples/config.example.toml")).unwrap(),
        &root(),
        company_ai::Overrides {
            live,
            identity: None,
            bundle: Some(bundle_path().to_str().unwrap()),
            database: Some(dir.join("state.sqlite").to_str().unwrap()),
            reports_dir: Some(dir.join("reports").to_str().unwrap()),
            hub: None,
            worker: None,
            service: None,
            fixture_dir: Some(fixtures().to_str().unwrap()),
        },
    )
    .unwrap()
}

fn store_count(store: &Store, sql: &str) -> i64 {
    rusqlite::Connection::open(store.path())
        .unwrap()
        .query_row(sql, [], |row| row.get(0))
        .unwrap()
}

fn query(store: &Store, sql: &str) -> String {
    rusqlite::Connection::open(store.path())
        .unwrap()
        .query_row(sql, [], |row| row.get(0))
        .unwrap()
}

fn store_query2(store: &Store, sql: &str) -> (String, String) {
    rusqlite::Connection::open(store.path())
        .unwrap()
        .query_row(sql, [], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
}

fn store_query3(store: &Store, sql: &str) -> (i64, String, String) {
    rusqlite::Connection::open(store.path())
        .unwrap()
        .query_row(sql, [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
}
