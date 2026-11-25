use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use company_ai::{
    sha256_hex, AdapterResult, AuthorizedRead, DiagnosticsSource, Event, FixtureAdapter,
    ManualClock, RunOutcome, RunPlan, Runtime, Script, ScriptedWorker, ServiceMode, Store,
    SystemClock, WorkerMode,
};
use serde_json::{json, Value};

pub fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

pub fn bundle_path() -> PathBuf {
    root().join("governance/bundle.json")
}

pub fn fixtures() -> PathBuf {
    root().join("governance/fixtures")
}

pub fn hub() -> PathBuf {
    root().join("../cursor-hub")
}

pub struct Scratch(PathBuf);

#[allow(clippy::new_without_default)]
impl Scratch {
    pub fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "company-ai-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn event(id: &str, alias: &str, summary: &str) -> Event {
    Event {
        schema_version: 1,
        event_id: id.to_string(),
        kind: "diagnostic.requested".to_string(),
        resource_alias: alias.to_string(),
        observed_at: "2026-11-25T12:04:00Z".to_string(),
        summary: summary.to_string(),
    }
}

pub fn write_bundle(dir: &Path, skill: Option<&str>, mutate: impl FnOnce(&mut Value)) -> PathBuf {
    let mut document: Value =
        serde_json::from_str(&fs::read_to_string(bundle_path()).unwrap()).unwrap();
    let governance = dir.join("governance");
    fs::create_dir_all(governance.join("skills")).unwrap();
    let skill_path = governance.join("skills/diagnostic-investigation.md");
    if let Some(text) = skill {
        fs::write(&skill_path, text).unwrap();
        document["skill"]["sha256"] = json!(sha256_hex(text.as_bytes()));
    } else {
        fs::copy(
            root().join("governance/skills/diagnostic-investigation.md"),
            &skill_path,
        )
        .unwrap();
    }
    mutate(&mut document);
    let path = governance.join("bundle.json");
    fs::write(&path, serde_json::to_vec_pretty(&document).unwrap()).unwrap();
    path
}

pub fn allow(document: &mut Value, identity: &str, aliases: &[&str]) {
    document["policy"]["identities"][identity]["allow"] = json!([
        {"operation": "diagnostics.read", "resources": aliases}
    ]);
}

pub struct Counting {
    pub inner: FixtureAdapter,
    pub calls: Mutex<Vec<String>>,
    pub timeouts: Mutex<Vec<f64>>,
    pub database: PathBuf,
    pub saw_started: Mutex<bool>,
}

impl Counting {
    pub fn new(database: PathBuf) -> Self {
        Self {
            inner: FixtureAdapter::new(fixtures()),
            calls: Mutex::new(Vec::new()),
            timeouts: Mutex::new(Vec::new()),
            database,
            saw_started: Mutex::new(false),
        }
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl DiagnosticsSource for Counting {
    fn adapter_id(&self) -> &'static str {
        "fixture"
    }

    fn transport(&self) -> &'static str {
        "local_files"
    }

    fn read(&self, authorized: &AuthorizedRead, timeout: Duration) -> AdapterResult {
        self.calls
            .lock()
            .unwrap()
            .push(authorized.target().resource_alias.clone());
        self.timeouts.lock().unwrap().push(timeout.as_secs_f64());
        if let Ok(conn) = rusqlite::Connection::open(&self.database) {
            if let Ok(status) = conn.query_row(
                "SELECT status FROM steps WHERE kind='provider_attempt' ORDER BY seq DESC LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            ) {
                *self.saw_started.lock().unwrap() = status == "started";
            }
        }
        self.inner.read(authorized, timeout)
    }
}

pub struct Run {
    pub outcome: RunOutcome,
    pub prompts: Vec<Value>,
    pub calls: Vec<String>,
    pub saw_started: bool,
    pub timeouts: Vec<f64>,
    pub database: PathBuf,
}

pub fn run_case(
    dir: &Path,
    bundle: &Path,
    script: Script,
    plan: RunPlan,
    happened: Event,
    clock: Option<&ManualClock>,
) -> Run {
    let database = dir.join("state.sqlite");
    let mut store = Store::open(&database, false).unwrap();
    let loaded = company_ai::load_bundle(bundle).unwrap();
    let adapter = Counting::new(database.clone());
    let mut worker = ScriptedWorker::new(script);
    let system = SystemClock::new();
    let outcome = if let Some(manual) = clock {
        let mut runtime = Runtime::new(
            &mut store,
            &loaded,
            &adapter,
            &mut worker,
            dir.join("reports"),
            plan,
            manual,
        )
        .unwrap();
        runtime.handle_event(happened).unwrap()
    } else {
        let mut runtime = Runtime::new(
            &mut store,
            &loaded,
            &adapter,
            &mut worker,
            dir.join("reports"),
            plan,
            &system,
        )
        .unwrap();
        runtime.handle_event(happened).unwrap()
    };
    let saw_started = *adapter.saw_started.lock().unwrap();
    let timeouts = adapter.timeouts.lock().unwrap().clone();
    Run {
        outcome,
        prompts: worker.seen().to_vec(),
        calls: adapter.calls(),
        saw_started,
        timeouts,
        database,
    }
}

pub fn investigator(live: bool) -> RunPlan {
    RunPlan {
        identity: "investigator".to_string(),
        worker_mode: WorkerMode::Scripted,
        service_mode: if live {
            ServiceMode::Live
        } else {
            ServiceMode::Fixture
        },
        live,
        model_id: None,
    }
}

pub fn lock_dir() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap()
}
