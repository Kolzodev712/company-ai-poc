use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::adapters::{FixtureAdapter, S3Diagnostics};
use crate::config::{
    live_problems, load_config, load_event, require_ready, settings_from_config, Overrides,
    Settings,
};
use crate::error::{Error, Result};
use crate::gateway::DiagnosticsSource;
use crate::governance::{check_fixture_files, load_bundle, verify_provenance, LoadedBundle};
use crate::runtime::{
    inspect_run, replay_run, RunOutcome, RunPlan, Runtime, ServiceMode, SystemClock, WorkerMode,
};
use crate::store::Store;
use crate::workers::ChatTransport;
use crate::workers::{OllamaWorker, ReqwestChat, ScriptedWorker};

#[derive(Parser)]
#[command(
    name = "company-ai",
    version,
    about = "Local company AI workflow proof of concept"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validate a governance bundle and optional config.
    Validate {
        #[arg(long, default_value = "governance/bundle.json")]
        bundle: PathBuf,
        #[arg(long, default_value = "governance/fixtures")]
        fixture_dir: PathBuf,
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        hub: Option<PathBuf>,
    },
    /// Run the fixture investigation.
    Demo {
        #[arg(long, default_value = "var/demo")]
        output: PathBuf,
        #[arg(long, default_value = "governance/bundle.json")]
        bundle: PathBuf,
        #[arg(long, default_value = "examples/events/diagnostic-demo-service-a.json")]
        event: PathBuf,
        #[arg(long, default_value = "governance/fixtures")]
        fixture_dir: PathBuf,
    },
    /// Run one event with the selected worker and service.
    Run {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        event: PathBuf,
        #[arg(long)]
        live: bool,
        #[arg(long)]
        identity: Option<String>,
        #[arg(long)]
        bundle: Option<String>,
        #[arg(long)]
        database: Option<String>,
        #[arg(long)]
        reports_dir: Option<String>,
        #[arg(long)]
        hub: Option<String>,
        #[arg(long)]
        worker: Option<String>,
        #[arg(long)]
        service: Option<String>,
        #[arg(long)]
        fixture_dir: Option<String>,
    },
    /// Show a run and mark unfinished work interrupted.
    Inspect {
        #[arg(long)]
        database: PathBuf,
        #[arg(long)]
        run: String,
    },
    /// Read a run without provider or model calls.
    Replay {
        #[arg(long)]
        database: PathBuf,
        #[arg(long)]
        run: String,
    },
    /// Print the stored report paths.
    Export {
        #[arg(long)]
        database: PathBuf,
        #[arg(long)]
        run: String,
    },
    /// Check or smoke-test explicit live configuration.
    Live {
        #[command(subcommand)]
        live: LiveCommand,
    },
}

#[derive(Subcommand)]
enum LiveCommand {
    /// Report missing live settings without network calls.
    Check {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        event: Option<PathBuf>,
    },
    /// Perform one bounded live call.
    Smoke {
        #[arg(long)]
        config: PathBuf,
        #[arg(long, value_parser = ["ollama", "s3"])]
        target: String,
        #[arg(long, default_value = "demo-service-a")]
        resource: String,
    },
}

pub fn main_from<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .try_init();
    match Cli::try_parse_from(args) {
        Ok(cli) => match dispatch(cli) {
            Ok(code) => code,
            Err(error) => print_error(&error),
        },
        Err(error) => {
            eprintln!("{error}");
            if error.use_stderr() {
                2
            } else {
                0
            }
        }
    }
}

fn dispatch(cli: Cli) -> Result<i32> {
    match cli.command {
        Command::Validate {
            bundle,
            fixture_dir,
            config,
            hub,
        } => {
            let loaded = load_bundle(&bundle)?;
            check_fixture_files(&loaded, &fixture_dir)?;
            println!(
                "bundle: ok {} {}",
                loaded.document.bundle_id, loaded.document.bundle_version
            );
            println!("policy_version: {}", loaded.policy_version());
            println!("fingerprint: {}", loaded.fingerprint);
            println!("skill: {}", loaded.document.skill.skill_id);
            if let Some(config) = config {
                load_config(&config)?;
                println!("config: ok {}", config.display());
            }
            if let Some(hub) = hub {
                let errors = verify_provenance(&loaded, &hub);
                if !errors.is_empty() {
                    println!("provenance: mismatch");
                    for error in errors {
                        println!("- {error}");
                    }
                    return Ok(2);
                }
                println!(
                    "provenance: ok {} ({} sources)",
                    loaded.document.provenance.commit,
                    loaded.document.provenance.sources.len()
                );
            } else {
                println!("provenance: not checked (pass --hub to verify cursor-hub hashes)");
            }
            Ok(0)
        }
        Command::Demo {
            output,
            bundle,
            event,
            fixture_dir,
        } => {
            let loaded = load_bundle(&bundle)?;
            check_fixture_files(&loaded, &fixture_dir)?;
            let event = load_event(&event)?;
            std::fs::create_dir_all(&output)?;
            let mut store = Store::open(output.join("state.sqlite"), false)?;
            store.reconcile_interrupted()?;
            let adapter = FixtureAdapter::new(fixture_dir);
            let mut worker = ScriptedWorker::new(crate::workers::Script::Investigate);
            let clock = SystemClock::new();
            let mut runtime = Runtime::new(
                &mut store,
                &loaded,
                &adapter,
                &mut worker,
                output.join("reports"),
                RunPlan {
                    identity: "investigator".to_string(),
                    worker_mode: WorkerMode::Scripted,
                    service_mode: ServiceMode::Fixture,
                    live: false,
                    model_id: None,
                },
                &clock,
            )?;
            let outcome = runtime.handle_event(event)?;
            print_outcome(&outcome);
            Ok(status_code(&outcome.status))
        }
        Command::Run {
            config,
            event,
            live,
            identity,
            bundle,
            database,
            reports_dir,
            hub: _,
            worker,
            service,
            fixture_dir,
        } => {
            let settings = settings_from_file(
                &config,
                Overrides {
                    live,
                    identity: identity.as_deref(),
                    bundle: bundle.as_deref(),
                    database: database.as_deref(),
                    reports_dir: reports_dir.as_deref(),
                    hub: None,
                    worker: worker.as_deref(),
                    service: service.as_deref(),
                    fixture_dir: fixture_dir.as_deref(),
                },
            )?;
            let loaded = load_bundle(&settings.bundle_path)?;
            let event = load_event(&event)?;
            execute(&settings, &loaded, event)
        }
        Command::Inspect { database, run } => {
            let mut store = Store::open(database, false)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&inspect_run(&mut store, &run)?)?
            );
            Ok(0)
        }
        Command::Replay { database, run } => {
            let store = Store::open(database, true)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&replay_run(&store, &run)?)?
            );
            Ok(0)
        }
        Command::Export { database, run } => {
            let store = Store::open(database, true)?;
            let record = store.get_run(&run)?;
            println!("status: {}", record.status);
            println!(
                "report_json: {}",
                record.report_json_path.as_deref().unwrap_or("none")
            );
            println!(
                "report_md: {}",
                record.report_md_path.as_deref().unwrap_or("none")
            );
            Ok(if record.report_json_path.is_some() {
                0
            } else {
                1
            })
        }
        Command::Live { live } => match live {
            LiveCommand::Check { config, event } => {
                let settings = settings_from_file(&config, live_overrides())?;
                let loaded = load_bundle(&settings.bundle_path)?;
                let event = event.as_ref().map(|path| load_event(path)).transpose()?;
                if settings.worker_mode != "ollama" && settings.service_mode != "s3" {
                    println!("live_check: no live adapter selected");
                    return Ok(0);
                }
                let problems = live_problems(&settings, &loaded, event.as_ref())
                    .into_iter()
                    .filter(|item| item != "--live")
                    .collect::<Vec<_>>();
                if !problems.is_empty() {
                    println!("live_check: incomplete. Fixture mode was not substituted.");
                    for item in problems {
                        println!("missing: {item}");
                    }
                    return Ok(2);
                }
                println!("live_check: ready");
                println!("worker: {}", settings.worker_mode);
                println!("service: {}", settings.service_mode);
                Ok(0)
            }
            LiveCommand::Smoke {
                config,
                target,
                resource,
            } => {
                let settings = settings_from_file(&config, live_overrides())?;
                let loaded = load_bundle(&settings.bundle_path)?;
                if target == "ollama" {
                    smoke_ollama(&settings, &loaded)
                } else {
                    smoke_s3(&settings, &loaded, &resource)
                }
            }
        },
    }
}

fn execute(settings: &Settings, bundle: &LoadedBundle, event: crate::model::Event) -> Result<i32> {
    require_ready(settings, bundle, Some(&event))?;
    if settings.service_mode == "fixture" {
        check_fixture_files(bundle, &settings.fixture_dir)?;
    }
    let worker_mode = if settings.worker_mode == "ollama" {
        WorkerMode::RealModel
    } else {
        WorkerMode::Scripted
    };
    let service_mode = if settings.service_mode == "s3" {
        ServiceMode::Live
    } else {
        ServiceMode::Fixture
    };
    let num_predict = settings.num_predict.min(bundle.limits().max_output_tokens);
    let mut worker: Box<dyn crate::workers::Worker> = if settings.worker_mode == "ollama" {
        Box::new(OllamaWorker::new(
            settings.endpoint.clone(),
            settings.model.clone(),
            num_predict,
            ReqwestChat,
            false,
        ))
    } else {
        Box::new(ScriptedWorker::new(ScriptedWorker::parse(
            &settings.script,
        )?))
    };
    let adapter: Box<dyn crate::gateway::DiagnosticsSource> = if settings.service_mode == "s3" {
        Box::new(S3Diagnostics::live(
            settings.aws_profile.clone(),
            settings.aws_region.clone(),
            settings.expected_bucket_owner.clone(),
        ))
    } else {
        Box::new(FixtureAdapter::new(settings.fixture_dir.clone()))
    };
    let mut store = Store::open(&settings.database, false)?;
    store.reconcile_interrupted()?;
    let clock = SystemClock::new();
    let mut runtime = Runtime::new(
        &mut store,
        bundle,
        adapter.as_ref(),
        worker.as_mut(),
        &settings.reports_dir,
        RunPlan {
            identity: settings.identity.clone(),
            worker_mode,
            service_mode,
            live: settings.live,
            model_id: if settings.worker_mode == "ollama" {
                Some(settings.model.clone())
            } else {
                None
            },
        },
        &clock,
    )?;
    let outcome = runtime.handle_event(event)?;
    print_outcome(&outcome);
    Ok(status_code(&outcome.status))
}

fn smoke_ollama(settings: &Settings, bundle: &LoadedBundle) -> Result<i32> {
    require_ready(settings, bundle, None)?;
    if settings.worker_mode != "ollama" {
        return Err(Error::setup(
            "Ollama smoke requires worker.mode = ollama",
            vec!["worker.mode".to_string()],
        ));
    }
    let payload = serde_json::json!({
        "model": settings.model,
        "stream": false,
        "think": false,
        "messages": [{"role": "user", "content": "Reply with the single word ok."}],
        "options": {"num_predict": 32.min(bundle.limits().max_output_tokens)}
    });
    match ReqwestChat.chat(
        &format!("{}/api/chat", settings.endpoint.trim_end_matches('/')),
        &payload,
        std::time::Duration::from_secs(bundle.limits().request_timeout_seconds),
    ) {
        Ok(body) => {
            let content = body["message"]["content"].as_str().unwrap_or("");
            println!("ollama_smoke: ok model={}", settings.model);
            println!("content_chars: {}", content.chars().count());
            Ok(0)
        }
        Err(error) => {
            println!("ollama_smoke: {}: {}", error.code(), error.message());
            Ok(1)
        }
    }
}

fn smoke_s3(settings: &Settings, bundle: &LoadedBundle, alias: &str) -> Result<i32> {
    let event = crate::model::Event {
        schema_version: 1,
        event_id: "evt-live-smoke".to_string(),
        kind: "diagnostic.requested".to_string(),
        resource_alias: alias.to_string(),
        observed_at: "2026-11-25T00:00:00Z".to_string(),
        summary: "Bounded S3 smoke read.".to_string(),
    };
    require_ready(settings, bundle, Some(&event))?;
    if settings.service_mode != "s3" {
        return Err(Error::setup(
            "S3 smoke requires service.mode = s3",
            vec!["service.mode".to_string()],
        ));
    }
    let resource = bundle
        .document
        .resources
        .get(alias)
        .ok_or_else(|| Error::app("validation", format!("Unknown resource {alias}")))?;
    let adapter = S3Diagnostics::live(
        settings.aws_profile.clone(),
        settings.aws_region.clone(),
        settings.expected_bucket_owner.clone(),
    );
    let decision = crate::broker::SecurityBroker::new(bundle).evaluate(
        &settings.identity,
        "diagnostics.read",
        alias,
        crate::broker::ServiceProvider::S3,
    )?;
    let Some(authorized) = decision.authorized else {
        println!("s3_smoke: {}", decision.reason);
        return Ok(1);
    };
    let _ = resource;
    let result = adapter.read(
        &authorized,
        std::time::Duration::from_secs(bundle.limits().request_timeout_seconds),
    );
    println!(
        "s3_smoke: {}",
        if result.ok {
            "ok".to_string()
        } else {
            result.error_code.unwrap_or_else(|| "error".to_string())
        }
    );
    println!("bytes: {}", result.byte_length);
    println!(
        "content_sha256: {}",
        result.content_sha256.as_deref().unwrap_or("none")
    );
    if !result.ok {
        println!("message: {}", result.message);
    }
    Ok(if result.ok { 0 } else { 1 })
}

fn settings_from_file(path: &std::path::Path, overrides: Overrides<'_>) -> Result<Settings> {
    let document = load_config(path)?;
    settings_from_config(document, &std::env::current_dir()?, overrides)
}

fn live_overrides() -> Overrides<'static> {
    Overrides {
        live: true,
        identity: None,
        bundle: None,
        database: None,
        reports_dir: None,
        hub: None,
        worker: None,
        service: None,
        fixture_dir: None,
    }
}

fn print_outcome(outcome: &RunOutcome) {
    println!("run_id: {}", outcome.run_id);
    println!("event_id: {}", outcome.event_id);
    println!("status: {}", outcome.status);
    println!("reused: {}", outcome.reused);
    println!(
        "failure_code: {}",
        outcome.failure_code.as_deref().unwrap_or("none")
    );
    println!(
        "report_json: {}",
        outcome.report_json_path.as_deref().unwrap_or("none")
    );
    println!(
        "report_md: {}",
        outcome.report_md_path.as_deref().unwrap_or("none")
    );
}

fn status_code(status: &str) -> i32 {
    if status == "succeeded" {
        0
    } else {
        1
    }
}

fn print_error(error: &Error) -> i32 {
    println!("{}: {}", error.code(), error.message());
    for item in error.missing() {
        println!("missing: {item}");
    }
    2
}
