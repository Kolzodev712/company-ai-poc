use std::fs::{self, OpenOptions};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use serde_json::Value;

use crate::error::{Error, Result};
use crate::model::{Event, Limits};
use crate::redact::redact;
use crate::util::{canonical, fingerprint, new_id, utc_now};

const APPLICATION_ID: i32 = 0x4341_4950;
const FORMAT_VERSION: i32 = 1;

#[derive(Debug, Clone)]
pub struct Budget {
    pub limits: Limits,
    pub used: Usage,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Usage {
    pub model_turns: u32,
    pub tool_proposals: u32,
    pub provider_attempts: u32,
}

impl Budget {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            used: Usage {
                model_turns: 0,
                tool_proposals: 0,
                provider_attempts: 0,
            },
        }
    }

    fn to_value(&self) -> Result<Value> {
        Ok(serde_json::json!({
            "limits": self.limits,
            "used": self.used,
        }))
    }
}

#[derive(Debug, Clone)]
pub struct RunRecord {
    pub run_id: String,
    pub event_id: String,
    pub status: String,
    pub failure_code: Option<String>,
    pub failure_detail: Option<String>,
    pub identity: String,
    pub workflow_id: String,
    pub mission_id: String,
    pub policy_version: String,
    pub bundle_id: String,
    pub bundle_version: String,
    pub bundle_fingerprint: String,
    pub worker_mode: String,
    pub service_mode: String,
    pub worker_adapter: String,
    pub service_adapter: String,
    pub model_id: Option<String>,
    pub budget_json: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub report_json: Option<String>,
    pub report_md_path: Option<String>,
    pub report_json_path: Option<String>,
}

pub struct Store {
    conn: Connection,
    readonly: bool,
    path: std::path::PathBuf,
}

impl Store {
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Store {
    pub fn open(path: impl AsRef<Path>, readonly: bool) -> Result<Self> {
        let path = path.as_ref();
        if readonly {
            if !path.exists() {
                return Err(Error::app("validation", "State database does not exist"));
            }
            reject_special(path)?;
            let conn = Connection::open_with_flags(
                path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                    | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            conn.pragma_update(None, "query_only", "ON")?;
            conn.pragma_update(None, "trusted_schema", "OFF")?;
            check_header(&conn)?;
            return Ok(Self {
                conn,
                readonly: true,
                path: path.to_path_buf(),
            });
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let created = !path.exists();
        if created {
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)?;
            drop(file);
        }
        reject_special(path)?;
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "trusted_schema", "OFF")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        if created {
            conn.pragma_update(None, "journal_mode", "WAL")?;
            conn.pragma_update(None, "synchronous", "FULL")?;
            conn.pragma_update(None, "application_id", APPLICATION_ID)?;
            create_schema(&conn)?;
        } else {
            check_header(&conn)?;
        }
        Ok(Self {
            conn,
            readonly,
            path: path.to_path_buf(),
        })
    }

    pub fn reconcile_interrupted(&mut self) -> Result<usize> {
        self.write_only()?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = utc_now();
        let runs = tx.execute(
            "UPDATE runs SET status='interrupted', failure_code='interrupted', failure_detail='Unfinished run marked interrupted on restart', finished_at=?1 WHERE status='running'",
            [&now],
        )?;
        tx.execute(
            "UPDATE steps SET status='interrupted', finished_at=?1 WHERE status='started'",
            [&now],
        )?;
        tx.commit()?;
        Ok(runs)
    }

    pub fn admit(
        &mut self,
        event: &Event,
        identity: &str,
        plan: &AdmitPlan,
        budget: &Budget,
    ) -> Result<(String, bool)> {
        self.write_only()?;
        let payload = canonical(&serde_json::to_value(event)?)?;
        let event_fingerprint = fingerprint(&serde_json::to_value(event)?)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT fingerprint FROM events WHERE event_id=?1",
                [&event.event_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(old) = existing {
            if old != event_fingerprint {
                return Err(Error::app(
                    "conflict",
                    format!(
                        "Event {} already exists with different content",
                        event.event_id
                    ),
                ));
            }
            let run_id: String = tx.query_row(
                "SELECT run_id FROM runs WHERE event_id=?1",
                [&event.event_id],
                |row| row.get(0),
            )?;
            tx.commit()?;
            return Ok((run_id, true));
        }
        let now = utc_now();
        tx.execute(
            "INSERT INTO events (event_id, fingerprint, payload_json, received_at) VALUES (?1,?2,?3,?4)",
            (&event.event_id, &event_fingerprint, &payload, &now),
        )?;
        let run_id = new_id("run_");
        let budget_json = canonical(&budget.to_value()?)?;
        tx.execute(
            "INSERT INTO runs (run_id, event_id, status, identity, workflow_id, mission_id, policy_version, bundle_id, bundle_version, bundle_fingerprint, worker_mode, service_mode, worker_adapter, service_adapter, model_id, budget_json, started_at) VALUES (?1,?2,'running',?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
            (
                &run_id,
                &event.event_id,
                identity,
                &plan.workflow_id,
                &plan.mission_id,
                &plan.policy_version,
                &plan.bundle_id,
                &plan.bundle_version,
                &plan.bundle_fingerprint,
                &plan.worker_mode,
                &plan.service_mode,
                &plan.worker_adapter,
                &plan.service_adapter,
                &plan.model_id,
                &budget_json,
                &now,
            ),
        )?;
        tx.commit()?;
        Ok((run_id, false))
    }

    pub fn start_step(
        &mut self,
        run_id: &str,
        kind: &str,
        detail: Option<&Value>,
    ) -> Result<String> {
        self.write_only()?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM steps WHERE run_id=?1",
            [run_id],
            |row| row.get(0),
        )?;
        let step_id = new_id("step_");
        let detail_json = detail.map(canonical).transpose()?;
        let now = utc_now();
        tx.execute(
            "INSERT INTO steps (step_id, run_id, seq, kind, status, started_at, detail_json) VALUES (?1,?2,?3,?4,'started',?5,?6)",
            (&step_id, run_id, seq + 1, kind, &now, &detail_json),
        )?;
        tx.commit()?;
        Ok(step_id)
    }

    pub fn finish_step(&mut self, step_id: &str, status: &str, detail: &Value) -> Result<()> {
        self.write_only()?;
        let mut stored = redact(&canonical(detail)?);
        stored.truncate(8_000);
        self.conn.execute(
            "UPDATE steps SET status=?1, finished_at=?2, detail_json=?3 WHERE step_id=?4 AND status='started'",
            (status, utc_now(), stored, step_id),
        )?;
        Ok(())
    }

    pub fn save_decision(
        &mut self,
        run_id: &str,
        decision: &crate::broker::Decision,
    ) -> Result<()> {
        self.write_only()?;
        let constraints = canonical(&serde_json::to_value(&decision.limits)?)?;
        let target = decision.target_audit.as_ref().map(canonical).transpose()?;
        self.conn.execute(
            "INSERT INTO decisions (decision_id, run_id, allowed, reason, policy_version, operation, resource_alias, constraints_json, target_json, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            (
                &decision.decision_id,
                run_id,
                decision.allowed as i64,
                &decision.reason,
                &decision.policy_version,
                &decision.operation,
                &decision.resource_alias,
                constraints,
                target,
                utc_now(),
            ),
        )?;
        Ok(())
    }

    pub fn update_budget(&mut self, run_id: &str, budget: &Budget) -> Result<()> {
        self.write_only()?;
        let json = canonical(&budget.to_value()?)?;
        self.conn.execute(
            "UPDATE runs SET budget_json=?1 WHERE run_id=?2",
            (json, run_id),
        )?;
        Ok(())
    }

    pub fn add_evidence(
        &mut self,
        run_id: &str,
        operation: &str,
        resource_alias: &str,
        source: &Value,
        payload: &Value,
        content_sha256: &str,
    ) -> Result<String> {
        self.write_only()?;
        let evidence_id = new_id("ev_");
        let now = utc_now();
        self.conn.execute(
            "INSERT INTO evidence (evidence_id, run_id, operation, resource_alias, source_json, captured_at, payload_json, content_sha256) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            (
                &evidence_id,
                run_id,
                operation,
                resource_alias,
                canonical(source)?,
                now,
                canonical(payload)?,
                content_sha256,
            ),
        )?;
        Ok(evidence_id)
    }

    pub fn finish_run(&mut self, run_id: &str, finish: RunFinish<'_>) -> Result<()> {
        self.write_only()?;
        let current = self.get_run(run_id)?;
        if current.status != "running" {
            return Ok(());
        }
        let detail = finish.failure_detail.map(|text| {
            let mut cleaned = redact(text);
            cleaned.truncate(1_000);
            cleaned
        });
        self.conn.execute(
            "UPDATE runs SET status=?1, failure_code=?2, failure_detail=?3, finished_at=?4, report_json=?5, report_md_path=?6, report_json_path=?7 WHERE run_id=?8 AND status='running'",
            (
                finish.status,
                finish.failure_code,
                detail,
                utc_now(),
                finish.report_json,
                finish.report_md_path,
                finish.report_json_path,
                run_id,
            ),
        )?;
        Ok(())
    }

    pub fn get_run(&self, run_id: &str) -> Result<RunRecord> {
        self.conn
            .query_row("SELECT * FROM runs WHERE run_id=?1", [run_id], map_run)
            .map_err(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => {
                    Error::app("validation", format!("Unknown run {run_id}"))
                }
                other => Error::from(other),
            })
    }

    pub fn decisions(&self, run_id: &str) -> Result<Vec<DecisionRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT decision_id, allowed, reason, policy_version, operation, resource_alias, target_json FROM decisions WHERE run_id=?1 ORDER BY created_at, decision_id",
        )?;
        let rows = stmt.query_map([run_id], |row| {
            Ok(DecisionRow {
                decision_id: row.get(0)?,
                allowed: row.get::<_, i64>(1)? != 0,
                reason: row.get(2)?,
                policy_version: row.get(3)?,
                operation: row.get(4)?,
                resource_alias: row.get(5)?,
                target_json: row.get(6)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Error::from)
    }

    pub fn evidence(&self, run_id: &str) -> Result<Vec<EvidenceRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT evidence_id, run_id, operation, resource_alias, source_json, captured_at, payload_json, content_sha256 FROM evidence WHERE run_id=?1 ORDER BY captured_at, evidence_id",
        )?;
        let rows = stmt.query_map([run_id], map_evidence)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Error::from)
    }

    pub fn find_evidence(&self, evidence_id: &str) -> Result<Option<EvidenceRow>> {
        self.conn
            .query_row(
                "SELECT evidence_id, run_id, operation, resource_alias, source_json, captured_at, payload_json, content_sha256 FROM evidence WHERE evidence_id=?1",
                [evidence_id],
                map_evidence,
            )
            .optional()
            .map_err(Error::from)
    }

    pub fn steps(&self, run_id: &str) -> Result<Vec<StepRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT step_id, seq, kind, status, started_at, finished_at FROM steps WHERE run_id=?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map([run_id], |row| {
            Ok(StepRow {
                step_id: row.get(0)?,
                seq: row.get(1)?,
                kind: row.get(2)?,
                status: row.get(3)?,
                started_at: row.get(4)?,
                finished_at: row.get(5)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Error::from)
    }

    fn write_only(&self) -> Result<()> {
        if self.readonly {
            Err(Error::app("validation", "Replay storage is read-only"))
        } else {
            Ok(())
        }
    }
}

pub struct RunFinish<'a> {
    pub status: &'a str,
    pub failure_code: Option<&'a str>,
    pub failure_detail: Option<&'a str>,
    pub report_json: Option<&'a str>,
    pub report_md_path: Option<&'a str>,
    pub report_json_path: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct AdmitPlan {
    pub workflow_id: String,
    pub mission_id: String,
    pub policy_version: String,
    pub bundle_id: String,
    pub bundle_version: String,
    pub bundle_fingerprint: String,
    pub worker_mode: String,
    pub service_mode: String,
    pub worker_adapter: String,
    pub service_adapter: String,
    pub model_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DecisionRow {
    pub decision_id: String,
    pub allowed: bool,
    pub reason: String,
    pub policy_version: String,
    pub operation: String,
    pub resource_alias: String,
    pub target_json: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EvidenceRow {
    pub evidence_id: String,
    pub run_id: String,
    pub operation: String,
    pub resource_alias: String,
    pub source_json: String,
    pub captured_at: String,
    pub payload_json: String,
    pub content_sha256: String,
}

#[derive(Debug, Clone)]
pub struct StepRow {
    pub step_id: String,
    pub seq: i64,
    pub kind: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
}

fn map_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<RunRecord> {
    Ok(RunRecord {
        run_id: row.get("run_id")?,
        event_id: row.get("event_id")?,
        status: row.get("status")?,
        failure_code: row.get("failure_code")?,
        failure_detail: row.get("failure_detail")?,
        identity: row.get("identity")?,
        workflow_id: row.get("workflow_id")?,
        mission_id: row.get("mission_id")?,
        policy_version: row.get("policy_version")?,
        bundle_id: row.get("bundle_id")?,
        bundle_version: row.get("bundle_version")?,
        bundle_fingerprint: row.get("bundle_fingerprint")?,
        worker_mode: row.get("worker_mode")?,
        service_mode: row.get("service_mode")?,
        worker_adapter: row.get("worker_adapter")?,
        service_adapter: row.get("service_adapter")?,
        model_id: row.get("model_id")?,
        budget_json: row.get("budget_json")?,
        started_at: row.get("started_at")?,
        finished_at: row.get("finished_at")?,
        report_json: row.get("report_json")?,
        report_md_path: row.get("report_md_path")?,
        report_json_path: row.get("report_json_path")?,
    })
}

fn map_evidence(row: &rusqlite::Row<'_>) -> rusqlite::Result<EvidenceRow> {
    Ok(EvidenceRow {
        evidence_id: row.get(0)?,
        run_id: row.get(1)?,
        operation: row.get(2)?,
        resource_alias: row.get(3)?,
        source_json: row.get(4)?,
        captured_at: row.get(5)?,
        payload_json: row.get(6)?,
        content_sha256: row.get(7)?,
    })
}

fn check_header(conn: &Connection) -> Result<()> {
    let application_id: i32 = conn.pragma_query_value(None, "application_id", |row| row.get(0))?;
    if application_id != APPLICATION_ID {
        return Err(Error::app(
            "validation",
            "Refusing to open a database that is not a company-ai state file",
        ));
    }
    let version: Option<i32> = conn
        .query_row("SELECT format_version FROM meta WHERE id=1", [], |row| {
            row.get(0)
        })
        .optional()?;
    if version != Some(FORMAT_VERSION) {
        return Err(Error::app(
            "validation",
            "Unsupported state format; there is no automatic migration",
        ));
    }
    Ok(())
}

fn create_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE meta (
          id INTEGER PRIMARY KEY CHECK (id = 1),
          format_version INTEGER NOT NULL
        );
        CREATE TABLE events (
          event_id TEXT PRIMARY KEY,
          fingerprint TEXT NOT NULL,
          payload_json TEXT NOT NULL,
          received_at TEXT NOT NULL
        );
        CREATE TABLE runs (
          run_id TEXT PRIMARY KEY,
          event_id TEXT NOT NULL UNIQUE,
          status TEXT NOT NULL,
          failure_code TEXT,
          failure_detail TEXT,
          identity TEXT NOT NULL,
          workflow_id TEXT NOT NULL,
          mission_id TEXT NOT NULL,
          policy_version TEXT NOT NULL,
          bundle_id TEXT NOT NULL,
          bundle_version TEXT NOT NULL,
          bundle_fingerprint TEXT NOT NULL,
          worker_mode TEXT NOT NULL,
          service_mode TEXT NOT NULL,
          worker_adapter TEXT NOT NULL,
          service_adapter TEXT NOT NULL,
          model_id TEXT,
          budget_json TEXT NOT NULL,
          started_at TEXT NOT NULL,
          finished_at TEXT,
          report_json TEXT,
          report_md_path TEXT,
          report_json_path TEXT,
          FOREIGN KEY (event_id) REFERENCES events(event_id)
        );
        CREATE TABLE steps (
          step_id TEXT PRIMARY KEY,
          run_id TEXT NOT NULL REFERENCES runs(run_id),
          seq INTEGER NOT NULL,
          kind TEXT NOT NULL,
          status TEXT NOT NULL,
          started_at TEXT NOT NULL,
          finished_at TEXT,
          detail_json TEXT,
          UNIQUE (run_id, seq)
        );
        CREATE TABLE decisions (
          decision_id TEXT PRIMARY KEY,
          run_id TEXT NOT NULL REFERENCES runs(run_id),
          allowed INTEGER NOT NULL,
          reason TEXT NOT NULL,
          policy_version TEXT NOT NULL,
          operation TEXT NOT NULL,
          resource_alias TEXT NOT NULL,
          constraints_json TEXT NOT NULL,
          target_json TEXT,
          created_at TEXT NOT NULL
        );
        CREATE TABLE evidence (
          evidence_id TEXT PRIMARY KEY,
          run_id TEXT NOT NULL REFERENCES runs(run_id),
          operation TEXT NOT NULL,
          resource_alias TEXT NOT NULL,
          source_json TEXT NOT NULL,
          captured_at TEXT NOT NULL,
          payload_json TEXT NOT NULL,
          content_sha256 TEXT NOT NULL
        );
        INSERT INTO meta (id, format_version) VALUES (1, 1);
        ",
    )?;
    Ok(())
}

fn reject_special(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() || !meta.is_file() || meta.nlink() != 1 {
        return Err(Error::app(
            "validation",
            "State database must be a regular, non-symlink, single-link file",
        ));
    }
    Ok(())
}
