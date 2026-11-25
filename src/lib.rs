#![forbid(unsafe_code)]

mod adapters;
mod broker;
mod cli;
mod config;
mod error;
mod gateway;
mod governance;
mod model;
mod redact;
mod report;
mod runtime;
mod runtime_io;
mod store;
mod util;
mod workers;

pub use adapters::{
    classify_provider_error, s3_retry_attempts, FixtureAdapter, ObjectBody, ObjectReader,
    ReaderFailure, S3Diagnostics,
};
pub use broker::{
    AuthorizedRead, FixtureBehavior, ProviderTarget, ResolvedTarget, SecurityBroker,
    ServiceProvider,
};
pub use cli::main_from;
pub use config::{
    live_problems, load_config, require_ready, settings_from_config, Overrides, Settings,
};
pub use error::{Error, Result};
pub use gateway::{AdapterResult, DiagnosticsSource};
pub use governance::{load_bundle, verify_provenance, LoadedBundle};
pub use model::Event;
pub use runtime::{
    inspect_run, replay_run, worker_context, Clock, ManualClock, RunOutcome, RunPlan, Runtime,
    ServiceMode, SystemClock, WorkerMode,
};
pub use store::{AdmitPlan, Budget, Store};
pub use util::sha256_hex;
pub use workers::{ChatTransport, OllamaWorker, ReqwestChat, Script, ScriptedWorker, Worker};
