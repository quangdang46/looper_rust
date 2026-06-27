use thiserror::Error;

/// Top-level config error type.
#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("config file not found at {0}")]
    FileNotFound(String),

    #[error("failed to read config file {path}: {source}")]
    ReadError {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse {path}: {detail}")]
    ParseError { path: String, detail: String },

    #[error("validation failed on {path}: {message}")]
    Validation { path: String, message: String },

    #[error("config merge conflict: {0}")]
    Merge(String),

    #[error("environment variable error: {0}")]
    Env(String),

    #[error("disclosure error: {0}")]
    Disclosure(String),

    #[error("{0}")]
    Other(String),
}

/// Severity of a validation issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationSeverity {
    Error,
    Warning,
}

/// A single validation finding.
#[derive(Debug, Clone)]
pub struct ValidationIssue {
    /// Dot-separated config path (e.g. `"server.host"`).
    pub path: String,
    /// Human-readable description.
    pub message: String,
    /// Severity level.
    pub severity: ValidationSeverity,
}

impl ValidationIssue {
    pub fn error(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self { path: path.into(), message: message.into(), severity: ValidationSeverity::Error }
    }

    pub fn warning(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self { path: path.into(), message: message.into(), severity: ValidationSeverity::Warning }
    }
}

/// Aggregated validation result.
#[derive(Debug, Default)]
pub struct ConfigValidation {
    pub issues: Vec<ValidationIssue>,
}

impl ConfigValidation {
    pub fn new() -> Self {
        Self { issues: Vec::new() }
    }

    pub fn add(&mut self, issue: ValidationIssue) {
        self.issues.push(issue);
    }

    /// Convenience: push an error-severity issue.
    pub fn error(&mut self, path: impl Into<String>, message: impl Into<String>) {
        self.issues.push(ValidationIssue::error(path, message));
    }

    /// Convenience: push a warning-severity issue.
    pub fn warn(&mut self, path: impl Into<String>, message: impl Into<String>) {
        self.issues.push(ValidationIssue::warning(path, message));
    }

    /// True if any error-severity issues exist.
    pub fn has_errors(&self) -> bool {
        !self.is_valid()
    }

    pub fn is_valid(&self) -> bool {
        self.issues.iter().all(|i| i.severity != ValidationSeverity::Error)
    }

    pub fn errors(&self) -> Vec<&ValidationIssue> {
        self.issues.iter().filter(|i| i.severity == ValidationSeverity::Error).collect()
    }

    pub fn warnings(&self) -> Vec<&ValidationIssue> {
        self.issues.iter().filter(|i| i.severity == ValidationSeverity::Warning).collect()
    }

    /// Consume self and return an error if any `Error`-severity issues exist.
    pub fn into_result(self) -> Result<(), ConfigError> {
        if self.is_valid() {
            Ok(())
        } else {
            let errors: Vec<String> = self
                .issues
                .into_iter()
                .filter(|i| i.severity == ValidationSeverity::Error)
                .map(|i| format!("{}: {}", i.path, i.message))
                .collect();
            Err(ConfigError::Validation { path: "root".into(), message: errors.join("; ") })
        }
    }
}
