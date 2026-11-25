use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::broker::ServiceProvider;
use crate::error::{Error, Result};
use crate::gateway::{DiagnosticsSource, ToolGateway};
use crate::governance::LoadedBundle;
use crate::model::{parse_worker_output, validate_event, Event, FinalReport, ValidatedTurn};
use crate::report::{write_reports, ReportInput};
use crate::store::{AdmitPlan, Budget, EvidenceRow, Store};
use crate::util::canonical;
use crate::workers::Worker;

pub trait Clock: Send + Sync {
    fn elapsed(&self) -> Duration;
}

pub struct SystemClock {
    start: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }
}

#[derive(Debug, Default)]
pub struct ManualClock {
    millis: AtomicU64,
}

impl ManualClock {
    pub fn set(&self, elapsed: Duration) {
        self.millis
            .store(elapsed.as_millis() as u64, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn elapsed(&self) -> Duration {
        Duration::from_millis(self.millis.load(Ordering::SeqCst))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerMode {
    Scripted,
    RealModel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceMode {
    Fixture,
    Live,
}

impl ServiceMode {
    fn provider(self) -> ServiceProvider {
        match self {
            Self::Fixture => ServiceProvider::Fixture,
            Self::Live => ServiceProvider::S3,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Fixture => "fixture",
            Self::Live => "live",
        }
    }
}

impl WorkerMode {
    fn label(self) -> &'static str {
        match self {
            Self::Scripted => "scripted",
            Self::RealModel => "real_model",
        }
    }
}

pub struct RunPlan {
    pub identity: String,
    pub worker_mode: WorkerMode,
    pub service_mode: ServiceMode,
    pub live: bool,
    pub model_id: Option<String>,
}

#[derive(Debug)]
pub struct RunOutcome {
    pub run_id: String,
    pub event_id: String,
    pub status: String,
    pub reused: bool,
    pub failure_code: Option<String>,
    pub report_json_path: Option<String>,
    pub report_md_path: Option<String>,
}

pub struct Runtime<'a> {
    store: &'a mut Store,
    bundle: &'a LoadedBundle,
    adapter: &'a dyn DiagnosticsSource,
    worker: &'a mut dyn Worker,
    reports_dir: std::path::PathBuf,
    plan: RunPlan,
    clock: &'a dyn Clock,
}

impl<'a> Runtime<'a> {
    pub fn new(
        store: &'a mut Store,
        bundle: &'a LoadedBundle,
        adapter: &'a dyn DiagnosticsSource,
        worker: &'a mut dyn Worker,
        reports_dir: impl Into<std::path::PathBuf>,
        plan: RunPlan,
        clock: &'a dyn Clock,
    ) -> Result<Self> {
        check_modes(&plan, adapter.adapter_id(), worker.adapter_id())?;
        Ok(Self {
            store,
            bundle,
            adapter,
            worker,
            reports_dir: reports_dir.into(),
            plan,
            clock,
        })
    }

    pub fn handle_event(&mut self, event: Event) -> Result<RunOutcome> {
        validate_event(&event)?;
        let budget = Budget::new(self.bundle.limits().clone());
        let plan = AdmitPlan {
            workflow_id: self.bundle.document.workflow.workflow_id.clone(),
            mission_id: self.bundle.document.mission.mission_id.clone(),
            policy_version: self.bundle.policy_version().to_string(),
            bundle_id: self.bundle.document.bundle_id.clone(),
            bundle_version: self.bundle.document.bundle_version.clone(),
            bundle_fingerprint: self.bundle.fingerprint.clone(),
            worker_mode: self.plan.worker_mode.label().to_string(),
            service_mode: self.plan.service_mode.label().to_string(),
            worker_adapter: self.worker.adapter_id().to_string(),
            service_adapter: self.adapter.adapter_id().to_string(),
            model_id: self.plan.model_id.clone(),
        };
        let (run_id, reused) = self
            .store
            .admit(&event, &self.plan.identity, &plan, &budget)?;
        if reused {
            tracing::info!(run_id, event_id = %event.event_id, "event reused");
            return self.outcome(&run_id, true);
        }
        if let Err(error) = self.execute(&run_id, &event, budget) {
            self.fail(&run_id, &event, error.code(), &error.message(), None)?;
        }
        self.outcome(&run_id, false)
    }

    fn execute(&mut self, run_id: &str, event: &Event, mut budget: Budget) -> Result<()> {
        let mut last_tool: Option<Value> = None;
        loop {
            if self.remaining().is_zero() {
                return self.fail(
                    run_id,
                    event,
                    "deadline_exceeded",
                    "Run duration budget is exhausted",
                    None,
                );
            }
            if budget.used.model_turns >= budget.limits.max_model_turns {
                return self.fail(
                    run_id,
                    event,
                    "budget_exhausted",
                    "Model turn budget is exhausted",
                    None,
                );
            }
            let context = worker_context(self.bundle, event, &self.store.evidence(run_id)?);
            if canonical(&context)?.len() > budget.limits.max_input_bytes as usize {
                return self.fail(
                    run_id,
                    event,
                    "input_too_large",
                    "Worker input exceeds max_input_bytes",
                    None,
                );
            }
            let step_id = self.store.start_step(run_id, "model_turn", None)?;
            budget.used.model_turns += 1;
            self.store.update_budget(run_id, &budget)?;
            let timeout = Duration::from_secs(budget.limits.request_timeout_seconds)
                .min(self.remaining().max(Duration::from_millis(100)));
            self.worker.set_request_timeout(timeout);
            let raw = match self.worker.turn(&context, last_tool.as_ref()) {
                Ok(raw) => raw,
                Err(error) => {
                    self.store.finish_step(
                        &step_id,
                        "failed",
                        &json!({"error_code": error.code()}),
                    )?;
                    return self.fail(run_id, event, error.code(), &error.message(), None);
                }
            };
            match parse_worker_output(&raw) {
                Ok(ValidatedTurn::Report(report)) => {
                    self.store.finish_step(
                        &step_id,
                        "succeeded",
                        &json!({"kind": "final_report"}),
                    )?;
                    return self.complete(run_id, event, &report);
                }
                Ok(ValidatedTurn::Proposal(_)) => {
                    if budget.used.tool_proposals >= budget.limits.max_tool_proposals {
                        self.store.finish_step(
                            &step_id,
                            "failed",
                            &json!({"error_code": "budget_exhausted"}),
                        )?;
                        return self.fail(
                            run_id,
                            event,
                            "budget_exhausted",
                            "Tool proposal budget is exhausted",
                            None,
                        );
                    }
                    budget.used.tool_proposals += 1;
                    self.store.update_budget(run_id, &budget)?;
                    let operation = raw
                        .get("operation")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown");
                    self.store.finish_step(
                        &step_id,
                        "succeeded",
                        &json!({"kind": "tool_proposal", "operation": operation}),
                    )?;
                    if self.remaining().is_zero() {
                        return self.fail(
                            run_id,
                            event,
                            "deadline_exceeded",
                            "Run duration budget is exhausted",
                            None,
                        );
                    }
                    let remaining = self.remaining();
                    let view = {
                        let mut gateway = ToolGateway::new(
                            self.bundle,
                            self.adapter,
                            self.store,
                            self.plan.service_mode.provider(),
                        );
                        gateway.invoke(run_id, &self.plan.identity, &raw, &mut budget, remaining)?
                    };
                    if view.status == "invalid" || view.status == "error" {
                        return self.fail(
                            run_id,
                            event,
                            view.error_code.as_deref().unwrap_or("provider_error"),
                            &view.message,
                            None,
                        );
                    }
                    last_tool = Some(json!({
                        "status": view.status,
                        "error_code": view.error_code,
                        "message": view.message,
                        "operation": view.operation,
                        "resource_alias": view.resource_alias,
                        "evidence_id": view.evidence_id,
                        "diagnostic": view.diagnostic,
                    }));
                }
                Err(error) => {
                    self.store.finish_step(
                        &step_id,
                        "failed",
                        &json!({"error_code": error.code()}),
                    )?;
                    return self.fail(run_id, event, error.code(), &error.message(), None);
                }
            }
        }
    }

    fn complete(&mut self, run_id: &str, event: &Event, report: &FinalReport) -> Result<()> {
        let completion = self.assess(run_id, event, report)?;
        if !completion.passed {
            return self.fail(
                run_id,
                event,
                completion
                    .failure_code
                    .as_deref()
                    .unwrap_or("missing_evidence"),
                completion
                    .failure_code
                    .as_deref()
                    .unwrap_or("missing_evidence"),
                Some(report),
            );
        }
        self.persist(run_id, "succeeded", None, None, Some(report), &completion)
    }

    fn fail(
        &mut self,
        run_id: &str,
        event: &Event,
        code: &str,
        detail: &str,
        report: Option<&FinalReport>,
    ) -> Result<()> {
        let completion = if let Some(report) = report {
            self.assess(run_id, event, report)?
        } else {
            Completion {
                passed: false,
                failure_code: Some(code.to_string()),
                checks: vec![
                    Check {
                        name: "same_run_evidence_references",
                        passed: false,
                    },
                    Check {
                        name: "required_diagnostics_read",
                        passed: false,
                    },
                ],
            }
        };
        self.persist(
            run_id,
            "failed",
            Some(code),
            Some(detail),
            report,
            &completion,
        )
    }

    fn persist(
        &mut self,
        run_id: &str,
        status: &str,
        failure_code: Option<&str>,
        failure_detail: Option<&str>,
        report: Option<&FinalReport>,
        completion: &Completion,
    ) -> Result<()> {
        let current = self.store.get_run(run_id)?;
        if current.status != "running" {
            return Ok(());
        }
        let input = ReportInput {
            run: &current,
            status,
            failure_code,
            failure_detail,
            decisions: &self.store.decisions(run_id)?,
            evidence: &self.store.evidence(run_id)?,
            provenance_repository: &self.bundle.document.provenance.repository,
            provenance_commit: &self.bundle.document.provenance.commit,
            completion,
            model_report: report,
            service_transport: self.adapter.transport(),
            worker_transport: self.worker.transport_name(),
        };
        let document = crate::report::build_report(&input)?;
        let (json_path, md_path) = write_reports(&self.reports_dir, run_id, &document)?;
        let report_json = document.to_string();
        let report_json_path = json_path.display().to_string();
        let report_md_path = md_path.display().to_string();
        self.store.finish_run(
            run_id,
            crate::store::RunFinish {
                status,
                failure_code,
                failure_detail,
                report_json: Some(&report_json),
                report_md_path: Some(&report_md_path),
                report_json_path: Some(&report_json_path),
            },
        )?;
        tracing::info!(
            run_id,
            status,
            failure = failure_code.unwrap_or("none"),
            "run finished"
        );
        Ok(())
    }

    fn assess(&self, run_id: &str, event: &Event, report: &FinalReport) -> Result<Completion> {
        let mut references_ok = true;
        for evidence_id in &report.evidence_ids {
            match self.store.find_evidence(evidence_id)? {
                Some(row) if row.run_id == run_id => {}
                _ => references_ok = false,
            }
        }
        let required_ok = self.store.evidence(run_id)?.iter().any(|row| {
            row.operation == "diagnostics.read"
                && row.resource_alias == event.resource_alias
                && report.evidence_ids.iter().any(|id| id == &row.evidence_id)
        });
        let failure = if !references_ok {
            Some("invalid_evidence".to_string())
        } else if !required_ok {
            Some("missing_evidence".to_string())
        } else {
            None
        };
        Ok(Completion {
            passed: failure.is_none(),
            failure_code: failure,
            checks: vec![
                Check {
                    name: "same_run_evidence_references",
                    passed: references_ok,
                },
                Check {
                    name: "required_diagnostics_read",
                    passed: required_ok,
                },
            ],
        })
    }

    fn remaining(&self) -> Duration {
        Duration::from_secs(self.bundle.limits().max_duration_seconds)
            .saturating_sub(self.clock.elapsed())
    }

    fn outcome(&self, run_id: &str, reused: bool) -> Result<RunOutcome> {
        let run = self.store.get_run(run_id)?;
        Ok(RunOutcome {
            run_id: run.run_id,
            event_id: run.event_id,
            status: run.status,
            reused,
            failure_code: run.failure_code,
            report_json_path: run.report_json_path,
            report_md_path: run.report_md_path,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Completion {
    pub passed: bool,
    pub failure_code: Option<String>,
    pub checks: Vec<Check>,
}

#[derive(Debug, Clone)]
pub struct Check {
    pub name: &'static str,
    pub passed: bool,
}

pub fn worker_context(bundle: &LoadedBundle, event: &Event, evidence: &[EvidenceRow]) -> Value {
    let operation = &bundle.document.operations.diagnostics_read;
    let evidence = evidence
        .iter()
        .filter_map(|row| {
            Some(json!({
                "evidence_id": row.evidence_id,
                "operation": row.operation,
                "resource_alias": row.resource_alias,
                "diagnostic": serde_json::from_str::<Value>(&row.payload_json).ok()?,
            }))
        })
        .collect::<Vec<_>>();
    json!({
        "mission_objective": bundle.document.mission.objective,
        "workflow_id": bundle.document.workflow.workflow_id,
        "skill_id": bundle.document.skill.skill_id,
        "skill_instructions": bundle.skill_text,
        "event": {
            "event_id": event.event_id,
            "kind": event.kind,
            "resource_alias": event.resource_alias,
            "observed_at": event.observed_at,
            "summary": event.summary,
        },
        "tools": [
            {
                "name": "diagnostics.read",
                "description": operation.description,
                "arguments": operation.arguments,
            },
            {
                "name": "submit_report",
                "description": "Submit observations, hypotheses, missing information, and evidence ids from this run.",
                "arguments": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["observations", "hypotheses", "missing", "evidence_ids"],
                    "properties": {
                        "observations": {"type": "array", "items": {"type": "string"}},
                        "hypotheses": {"type": "array", "items": {"type": "string"}},
                        "missing": {"type": "array", "items": {"type": "string"}},
                        "evidence_ids": {"type": "array", "items": {"type": "string"}}
                    }
                }
            }
        ],
        "evidence": evidence,
    })
}

fn check_modes(plan: &RunPlan, service_adapter: &str, worker_adapter: &str) -> Result<()> {
    if plan.service_mode == ServiceMode::Live && service_adapter != "s3" {
        return Err(Error::setup(
            "Live service mode cannot use the fixture adapter.",
            vec!["service.mode".to_string()],
        ));
    }
    if plan.service_mode == ServiceMode::Fixture && service_adapter != "fixture" {
        return Err(Error::setup(
            "Fixture service mode cannot use the S3 adapter.",
            vec!["service.mode".to_string()],
        ));
    }
    if plan.worker_mode == WorkerMode::RealModel && worker_adapter != "ollama" {
        return Err(Error::setup(
            "Real-model mode cannot use the scripted worker.",
            vec!["worker.mode".to_string()],
        ));
    }
    if plan.worker_mode == WorkerMode::Scripted && worker_adapter != "scripted" {
        return Err(Error::setup(
            "Scripted mode cannot use the Ollama worker.",
            vec!["worker.mode".to_string()],
        ));
    }
    if service_adapter == "s3" && !plan.live {
        return Err(Error::setup(
            "S3 was selected without --live. Fixture mode was not substituted.",
            vec!["--live".to_string()],
        ));
    }
    if worker_adapter == "ollama" && !plan.live {
        return Err(Error::setup(
            "Ollama was selected without --live. The scripted worker was not substituted.",
            vec!["--live".to_string()],
        ));
    }
    Ok(())
}

pub fn inspect_run(store: &mut Store, run_id: &str) -> Result<Value> {
    store.reconcile_interrupted()?;
    history(store, run_id, false)
}

pub fn replay_run(store: &Store, run_id: &str) -> Result<Value> {
    let mut history = history(store, run_id, true)?;
    if let Some(object) = history.as_object_mut() {
        object.insert("view".to_string(), json!("replay"));
        object.insert("external_calls".to_string(), json!(0));
        object.insert(
            "note".to_string(),
            json!("Replay reads recorded history and performs no provider or model calls. It does not reproduce nondeterministic model output. A status of running means the writer had not finished; inspect marks that run interrupted."),
        );
    }
    Ok(history)
}

fn history(store: &Store, run_id: &str, replay: bool) -> Result<Value> {
    let run = store.get_run(run_id)?;
    let steps = store.steps(run_id)?;
    let decisions = store.decisions(run_id)?;
    let evidence = store.evidence(run_id)?;
    Ok(json!({
        "view": if replay { "replay" } else { "inspect" },
        "run_id": run.run_id,
        "event_id": run.event_id,
        "status": run.status,
        "failure_code": run.failure_code,
        "failure_detail": run.failure_detail,
        "identity": run.identity,
        "worker_mode": run.worker_mode,
        "service_mode": run.service_mode,
        "model_id": run.model_id,
        "policy_version": run.policy_version,
        "bundle_fingerprint": run.bundle_fingerprint,
        "report_md_path": run.report_md_path,
        "report_json_path": run.report_json_path,
        "budget": serde_json::from_str::<Value>(&run.budget_json)?,
        "steps": steps.iter().map(|step| json!({
            "step_id": step.step_id,
            "seq": step.seq,
            "kind": step.kind,
            "status": step.status,
            "started_at": step.started_at,
            "finished_at": step.finished_at,
        })).collect::<Vec<_>>(),
        "decisions": decisions.iter().map(|decision| json!({
            "decision_id": decision.decision_id,
            "allowed": decision.allowed,
            "reason": decision.reason,
            "policy_version": decision.policy_version,
            "operation": decision.operation,
            "resource_alias": decision.resource_alias,
        })).collect::<Vec<_>>(),
        "evidence_ids": evidence.iter().map(|item| item.evidence_id.clone()).collect::<Vec<_>>(),
    }))
}
