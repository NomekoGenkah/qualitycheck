use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("No Jev API key found. Set JEV_API_KEY environment variable or run 'qualitycheck init --api-key <key>'.")]
    MissingApiKey,

    #[error("Failed to read configuration file at '{0}': {1}")]
    ReadError(PathBuf, #[source] std::io::Error),

    #[error("Failed to parse configuration file at '{0}': {1}")]
    ParseError(PathBuf, #[source] toml::de::Error),

    #[error("Failed to write configuration file at '{0}': {1}")]
    WriteError(PathBuf, #[source] std::io::Error),

    #[error("Failed to serialize configuration: {0}")]
    SerializeError(#[source] toml::ser::Error),

    #[error("Failed to determine user home or config directory")]
    HomeDirNotFound,
}

#[derive(Debug, Error)]
pub enum ProfileError {
    #[error("Profile '{0}' not found in installed profiles or built-in defaults")]
    NotFound(String),

    #[error("Failed to parse profile JSON for '{0}': {1}")]
    JsonError(String, #[source] serde_json::Error),

    #[error("Failed to read profile file at '{0}': {1}")]
    IoError(PathBuf, #[source] std::io::Error),

    #[error("Profile validation failed against JSON schema: {0}")]
    ValidationError(String),

    #[error("Profile '{0}' contains no metrics")]
    EmptyMetrics(String),

    #[error("Metric '{metric_id}' in profile '{profile}' has invalid configuration: {reason}")]
    InvalidMetric {
        profile: String,
        metric_id: String,
        reason: String,
    },
}

#[derive(Debug, Error)]
pub enum JevError {
    #[error("HTTP request to Jev API failed: {0}")]
    Network(#[from] reqwest::Error),

    #[error("Jev API returned HTTP {status}: {message}")]
    ApiStatus { status: u16, message: String },

    #[error("Authentication failed (HTTP 401): invalid or unauthorized Jev API key")]
    Unauthorized,

    #[error("Rate limit exceeded (HTTP 429) from Jev API")]
    RateLimited,

    #[error("Failed to parse Jev API response: {0}")]
    InvalidResponse(String),

    #[error("Missing answer in Jev response for metric '{0}'")]
    MissingMetricAnswer(String),
}

#[derive(Debug, Error)]
pub enum CacheError {
    #[error("Failed to read cache file at '{0}': {1}")]
    ReadError(PathBuf, #[source] std::io::Error),

    #[error("Failed to write cache file at '{0}': {1}")]
    WriteError(PathBuf, #[source] std::io::Error),

    #[error("Failed to parse cached result at '{0}': {1}")]
    ParseError(PathBuf, #[source] serde_json::Error),
}

#[derive(Debug, Error)]
pub enum GitError {
    #[error("Not a git repository: '{0}'")]
    NotARepository(PathBuf),

    #[error("Git error: {0}")]
    Git2(#[from] git2::Error),

    #[error("Branch or revision '{0}' not found")]
    RevisionNotFound(String),
}

#[derive(Debug, Error)]
pub enum ScanError {
    #[error("Target path does not exist: '{0}'")]
    PathNotFound(PathBuf),

    #[error("Failed to read file '{0}': {1}")]
    FileReadError(PathBuf, #[source] std::io::Error),

    #[error("Failed to scan directory: {0}")]
    WalkError(#[from] ignore::Error),

    #[error("Invalid glob pattern '{0}': {1}")]
    GlobError(String, #[source] globset::Error),

    #[error("No candidate files found to scan in '{0}'")]
    NoFilesFound(PathBuf),
}

#[derive(Debug, Error)]
pub enum QualityCheckError {
    #[error(transparent)]
    Config(#[from] ConfigError),

    #[error(transparent)]
    Profile(#[from] ProfileError),

    #[error(transparent)]
    Jev(#[from] JevError),

    #[error(transparent)]
    Cache(#[from] CacheError),

    #[error(transparent)]
    Git(#[from] GitError),

    #[error(transparent)]
    Scan(#[from] ScanError),

    #[error("Usage error: {0}")]
    Usage(String),

    #[error("Run threshold failure: {0}")]
    ThresholdFailure(String),

    #[error("General I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl QualityCheckError {
    pub fn exit_code(&self) -> i32 {
        match self {
            QualityCheckError::ThresholdFailure(_) => 1,
            _ => 2,
        }
    }
}
