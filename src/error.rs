use thiserror::Error;

/// Library error. The code is stable for operators and tests.
#[derive(Debug, Error)]
pub enum Error {
    #[error("{message}")]
    App {
        code: &'static str,
        message: String,
        missing: Vec<String>,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl Error {
    pub fn app(code: &'static str, message: impl Into<String>) -> Self {
        Self::App {
            code,
            message: message.into(),
            missing: Vec::new(),
        }
    }

    pub fn setup(message: impl Into<String>, missing: Vec<String>) -> Self {
        Self::App {
            code: "setup",
            message: message.into(),
            missing,
        }
    }

    pub fn code(&self) -> &str {
        match self {
            Self::App { code, .. } => code,
            Self::Io(_) => "io",
            Self::Sqlite(_) => "store",
            Self::Json(_) => "validation",
        }
    }

    pub fn missing(&self) -> &[String] {
        match self {
            Self::App { missing, .. } => missing,
            _ => &[],
        }
    }

    pub fn message(&self) -> String {
        self.to_string()
    }
}

pub type Result<T> = std::result::Result<T, Error>;
