"""SQLite run history for one local operator.

Same-event admission is transactional. Replay opens the file read-only.
A digest of stored JSON detects accidental inconsistency; it is not authentication
and it does not stop a person who can write the database file.
"""

from __future__ import annotations

import json
import os
import sqlite3
import stat
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from company_ai.contracts import canonical
from company_ai.errors import CompanyAIError, ConflictError
from company_ai.redact import redact

APPLICATION_ID = 0x43414950
FORMAT_VERSION = 1


def utc_now() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def _reject_special(path: Path) -> None:
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
        raise CompanyAIError("validation", "State database must be a regular, non-symlink, single-link file")


class Store:
    def __init__(self, connection: sqlite3.Connection, path: Path, readonly: bool):
        self.connection = connection
        self.path = path
        self.readonly = readonly

    @classmethod
    def open(cls, path: str | Path, *, readonly: bool = False) -> Store:
        target = Path(path)
        if readonly:
            if not target.exists():
                raise CompanyAIError("validation", "State database does not exist")
            _reject_special(target)
            connection = sqlite3.connect(f"{target.resolve().as_uri()}?mode=ro", uri=True, timeout=5)
            connection.row_factory = sqlite3.Row
            connection.execute("PRAGMA query_only=ON")
            connection.execute("PRAGMA trusted_schema=OFF")
            cls._check_header(connection)
            return cls(connection, target, True)
        target.parent.mkdir(parents=True, exist_ok=True)
        created = not target.exists()
        if created:
            descriptor = os.open(target, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
            os.close(descriptor)
        _reject_special(target)
        connection = sqlite3.connect(target, timeout=5, isolation_level=None)
        connection.row_factory = sqlite3.Row
        connection.execute("PRAGMA foreign_keys=ON")
        connection.execute("PRAGMA trusted_schema=OFF")
        connection.execute("PRAGMA busy_timeout=5000")
        if created:
            connection.execute("PRAGMA journal_mode=WAL")
            connection.execute("PRAGMA synchronous=FULL")
            connection.execute(f"PRAGMA application_id={APPLICATION_ID}")
            cls._create(connection)
        else:
            cls._check_header(connection)
        return cls(connection, target, False)

    @staticmethod
    def _check_header(connection: sqlite3.Connection) -> None:
        application_id = connection.execute("PRAGMA application_id").fetchone()[0]
        if application_id != APPLICATION_ID:
            connection.close()
            raise CompanyAIError("validation", "Refusing to open a database that is not a company-ai state file")
        row = connection.execute("SELECT format_version FROM meta WHERE id=1").fetchone()
        if row is None or row[0] != FORMAT_VERSION:
            connection.close()
            raise CompanyAIError("validation", "Unsupported state format; there is no automatic migration")

    @staticmethod
    def _create(connection: sqlite3.Connection) -> None:
        connection.executescript(
            """
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
            """
        )

    def close(self) -> None:
        self.connection.close()

    def _write(self, statement: str, parameters: tuple[Any, ...] = ()) -> sqlite3.Cursor:
        if self.readonly:
            raise CompanyAIError("validation", "Replay storage is read-only")
        return self.connection.execute(statement, parameters)

    def reconcile_interrupted(self) -> int:
        now = utc_now()
        self._write("BEGIN IMMEDIATE")
        try:
            runs = self._write(
                """
                UPDATE runs
                   SET status='interrupted', failure_code='interrupted',
                       failure_detail='Unfinished run marked interrupted on restart',
                       finished_at=?
                 WHERE status='running'
                """,
                (now,),
            )
            self._write(
                "UPDATE steps SET status='interrupted', finished_at=? WHERE status='started'",
                (now,),
            )
            self._write("COMMIT")
        except Exception:
            self.connection.execute("ROLLBACK")
            raise
        return int(runs.rowcount)

    def admit(
        self,
        *,
        event: dict[str, Any],
        event_fingerprint: str,
        identity: str,
        workflow_id: str,
        mission_id: str,
        policy_version: str,
        bundle_id: str,
        bundle_version: str,
        bundle_fingerprint: str,
        worker_mode: str,
        service_mode: str,
        worker_adapter: str,
        service_adapter: str,
        model_id: str | None,
        budget: dict[str, Any],
    ) -> tuple[str, bool]:
        payload = canonical(event)
        self._write("BEGIN IMMEDIATE")
        try:
            existing = self.connection.execute(
                "SELECT fingerprint FROM events WHERE event_id=?",
                (event["event_id"],),
            ).fetchone()
            if existing is not None:
                if existing["fingerprint"] != event_fingerprint:
                    raise ConflictError(
                        f"Event {event['event_id']} already exists with different content"
                    )
                run = self.connection.execute(
                    "SELECT run_id FROM runs WHERE event_id=?",
                    (event["event_id"],),
                ).fetchone()
                if run is None:
                    raise CompanyAIError("validation", "Event exists without a run; refusing to invent one")
                self._write("COMMIT")
                return str(run["run_id"]), True
            self._write(
                "INSERT INTO events (event_id, fingerprint, payload_json, received_at) VALUES (?,?,?,?)",
                (event["event_id"], event_fingerprint, payload, utc_now()),
            )
            run_id = "run_" + uuid.uuid4().hex
            self._write(
                """
                INSERT INTO runs (
                  run_id, event_id, status, identity, workflow_id, mission_id, policy_version,
                  bundle_id, bundle_version, bundle_fingerprint, worker_mode, service_mode,
                  worker_adapter, service_adapter, model_id, budget_json, started_at
                ) VALUES (?,?, 'running', ?,?,?,?,?,?,?,?,?,?,?,?,?,?)
                """,
                (
                    run_id,
                    event["event_id"],
                    identity,
                    workflow_id,
                    mission_id,
                    policy_version,
                    bundle_id,
                    bundle_version,
                    bundle_fingerprint,
                    worker_mode,
                    service_mode,
                    worker_adapter,
                    service_adapter,
                    model_id,
                    canonical(budget),
                    utc_now(),
                ),
            )
            self._write("COMMIT")
            return run_id, False
        except Exception:
            self.connection.execute("ROLLBACK")
            raise

    def start_step(self, run_id: str, kind: str, detail: dict[str, Any] | None = None) -> str:
        step_id = "step_" + uuid.uuid4().hex
        self._write("BEGIN IMMEDIATE")
        try:
            current = self.connection.execute("SELECT COALESCE(MAX(seq), 0) AS seq FROM steps WHERE run_id=?", (run_id,)).fetchone()
            seq = int(current["seq"]) + 1
            self._write(
                """
                INSERT INTO steps (step_id, run_id, seq, kind, status, started_at, detail_json)
                VALUES (?,?,?,?, 'started', ?, ?)
                """,
                (step_id, run_id, seq, kind, utc_now(), None if detail is None else canonical(detail)),
            )
            self._write("COMMIT")
        except Exception:
            self.connection.execute("ROLLBACK")
            raise
        return step_id

    def finish_step(self, step_id: str, status: str, detail: dict[str, Any]) -> None:
        stored = redact(canonical(detail))
        if len(stored) > 8_000:
            stored = stored[:8_000]
        self._write(
            "UPDATE steps SET status=?, finished_at=?, detail_json=? WHERE step_id=? AND status='started'",
            (status, utc_now(), stored, step_id),
        )

    def save_decision(self, run_id: str, decision: dict[str, Any]) -> None:
        target = decision.get("target")
        self._write(
            """
            INSERT INTO decisions (
              decision_id, run_id, allowed, reason, policy_version, operation, resource_alias,
              constraints_json, target_json, created_at
            ) VALUES (?,?,?,?,?,?,?,?,?,?)
            """,
            (
                decision["decision_id"],
                run_id,
                1 if decision["allowed"] else 0,
                decision["reason"],
                decision["policy_version"],
                decision["operation"],
                decision["resource_alias"],
                canonical(decision["constraints"]),
                None if target is None else canonical(target),
                utc_now(),
            ),
        )

    def update_budget(self, run_id: str, budget: dict[str, Any]) -> None:
        self._write("UPDATE runs SET budget_json=? WHERE run_id=?", (canonical(budget), run_id))

    def add_evidence(
        self,
        *,
        run_id: str,
        operation: str,
        resource_alias: str,
        source: dict[str, Any],
        payload: dict[str, Any],
        content_sha256: str,
    ) -> str:
        evidence_id = "ev_" + uuid.uuid4().hex
        self._write(
            """
            INSERT INTO evidence (
              evidence_id, run_id, operation, resource_alias, source_json, captured_at,
              payload_json, content_sha256
            ) VALUES (?,?,?,?,?,?,?,?)
            """,
            (
                evidence_id,
                run_id,
                operation,
                resource_alias,
                canonical(source),
                utc_now(),
                canonical(payload),
                content_sha256,
            ),
        )
        return evidence_id

    def finish_run(
        self,
        run_id: str,
        status: str,
        failure_code: str | None,
        failure_detail: str | None,
        report_json: str | None = None,
        report_md_path: str | None = None,
        report_json_path: str | None = None,
    ) -> None:
        current = self.get_run(run_id)
        if current["status"] != "running":
            return
        detail = None if failure_detail is None else redact(failure_detail)[:1_000]
        self._write(
            """
            UPDATE runs
               SET status=?, failure_code=?, failure_detail=?, finished_at=?,
                   report_json=?, report_md_path=?, report_json_path=?
             WHERE run_id=? AND status='running'
            """,
            (status, failure_code, detail, utc_now(), report_json, report_md_path, report_json_path, run_id),
        )

    def get_run(self, run_id: str) -> dict[str, Any]:
        row = self.connection.execute("SELECT * FROM runs WHERE run_id=?", (run_id,)).fetchone()
        if row is None:
            raise CompanyAIError("validation", f"Unknown run {run_id}")
        return dict(row)

    def steps(self, run_id: str) -> list[dict[str, Any]]:
        rows = self.connection.execute(
            "SELECT * FROM steps WHERE run_id=? ORDER BY seq",
            (run_id,),
        ).fetchall()
        return [dict(row) for row in rows]

    def decisions(self, run_id: str) -> list[dict[str, Any]]:
        rows = self.connection.execute(
            "SELECT * FROM decisions WHERE run_id=? ORDER BY created_at, decision_id",
            (run_id,),
        ).fetchall()
        return [dict(row) for row in rows]

    def evidence(self, run_id: str) -> list[dict[str, Any]]:
        rows = self.connection.execute(
            "SELECT * FROM evidence WHERE run_id=? ORDER BY captured_at, evidence_id",
            (run_id,),
        ).fetchall()
        return [dict(row) for row in rows]

    def find_evidence(self, evidence_id: str) -> dict[str, Any] | None:
        row = self.connection.execute(
            "SELECT * FROM evidence WHERE evidence_id=?",
            (evidence_id,),
        ).fetchone()
        return None if row is None else dict(row)
