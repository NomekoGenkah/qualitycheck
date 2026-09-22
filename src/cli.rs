use std::path::PathBuf;
use clap::{Args, Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "qualitycheck",
    version,
    about = "Quantifiable, configurable code quality evaluation using TypeSafe AI Jev"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,

    #[command(flatten)]
    pub scan_args: ScanArgs,
}

#[derive(Subcommand, Debug, Clone)]
pub enum Commands {
    /// Initialize configuration, default profiles, and gitignore
    Init(InitArgs),

    /// Manage qualitycheck profiles
    Profiles {
        #[command(subcommand)]
        command: ProfilesCommand,
    },

    /// Output a machine-readable JSON manifest of commands and profiles
    Describe,

    /// Scan a directory or file for code quality
    Scan(ScanArgs),

    /// Git-aware scan: analyze files changed versus base branch or working tree
    Patch(PatchArgs),

    /// Triage view: show metrics/files failing profile threshold from a saved run
    Gaps(GapsArgs),

    /// Detailed breakdown for a single file from cache or history
    File(FileArgs),

    /// Compare two saved runs and report regressions or improvements
    Diff(DiffArgs),

    /// Manage saved run history
    Runs {
        #[command(subcommand)]
        command: RunsCommand,
    },
}

#[derive(Args, Debug, Clone, Default)]
pub struct ScanArgs {
    /// One or more target paths (files or directories) to scan [default: .]
    #[arg(default_value = ".")]
    pub paths: Vec<PathBuf>,

    /// One or more profiles to run, comma-separated (e.g. "quality,security")
    #[arg(long, default_value = "quality")]
    pub profile: String,

    /// Output format: table or json [default: table]
    #[arg(long, default_value = "table", value_parser = ["table", "json"])]
    pub format: String,

    /// Override the default persisted report location/name
    #[arg(long)]
    pub save: Option<PathBuf>,

    /// Skip writing to .qualitycheck/ entirely (pure stdout)
    #[arg(long)]
    pub no_persist: bool,

    /// Exit code 1 if any score falls below the profile's fail_below threshold
    #[arg(long)]
    pub strict: bool,

    /// Disable .gitignore respecting (default: respects it)
    #[arg(long)]
    pub no_ignore: bool,

    /// Additional inclusion glob pattern (can be repeated)
    #[arg(long = "include")]
    pub include: Vec<String>,

    /// Additional exclusion glob pattern (can be repeated)
    #[arg(long = "exclude")]
    pub exclude: Vec<String>,

    /// Skip files larger than this size in KB [default: 200]
    #[arg(long, default_value = "200")]
    pub max_file_size: u64,

    /// Parallel requests to Jev API [default: 10]
    #[arg(long, default_value = "10")]
    pub concurrency: usize,

    /// Force-disable colored output (auto-disabled when piped)
    #[arg(long)]
    pub no_color: bool,

    /// Preview the files to be evaluated, token consumption, and estimated cost without calling Jev API
    #[arg(long, alias = "dry-run")]
    pub preview: bool,

    /// Output full per-file breakdown in preview (default: compact summary to save LLM context)
    #[arg(long, alias = "verbose")]
    pub full: bool,
}

#[derive(Args, Debug, Clone)]
pub struct PatchArgs {
    /// Base branch or revision to compare against (default: uncommitted tree, or upstream)
    #[arg(long)]
    pub base: Option<String>,

    /// Target directory or repo root [default: .]
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// One or more profiles to run, comma-separated (e.g. "quality,security")
    #[arg(long, default_value = "quality")]
    pub profile: String,

    /// Output format: table or json [default: table]
    #[arg(long, default_value = "table", value_parser = ["table", "json"])]
    pub format: String,

    /// Override the default persisted report location/name
    #[arg(long)]
    pub save: Option<PathBuf>,

    /// Skip writing to .qualitycheck/ entirely (pure stdout)
    #[arg(long)]
    pub no_persist: bool,

    /// Exit code 1 if any score falls below the profile's fail_below threshold
    #[arg(long)]
    pub strict: bool,

    /// Also score each changed file as it was at the base and report per-file score changes
    #[arg(long)]
    pub delta: bool,

    /// Exit code 1 if any changed file's composite drops by more than POINTS versus the base, or
    /// a file with no base version scores below fail_below. Scores that were already low at the
    /// base don't fail. Implies --delta
    #[arg(long, value_name = "POINTS")]
    pub fail_on_regression: Option<f64>,

    /// Additional inclusion glob pattern
    #[arg(long = "include")]
    pub include: Vec<String>,

    /// Additional exclusion glob pattern
    #[arg(long = "exclude")]
    pub exclude: Vec<String>,

    /// Skip files larger than this size in KB [default: 200]
    #[arg(long, default_value = "200")]
    pub max_file_size: u64,

    /// Parallel requests to Jev API [default: 10]
    #[arg(long, default_value = "10")]
    pub concurrency: usize,

    /// Force-disable colored output
    #[arg(long)]
    pub no_color: bool,

    /// Preview the files to be evaluated, token consumption, and estimated cost without calling Jev API
    #[arg(long, alias = "dry-run")]
    pub preview: bool,

    /// Output full per-file breakdown in preview (default: compact summary to save LLM context)
    #[arg(long, alias = "verbose")]
    pub full: bool,
}

#[derive(Args, Debug, Clone)]
pub struct InitArgs {
    /// Jev API key
    #[arg(long)]
    pub api_key: Option<String>,

    /// Never prompt for interactive input
    #[arg(long)]
    pub non_interactive: bool,
}

#[derive(Subcommand, Debug, Clone)]
pub enum ProfilesCommand {
    /// List available profiles (defaults + custom ones in ~/.config/qualitycheck/profiles/)
    List,
    /// Print a profile's JSON
    Show {
        /// Profile name
        name: String,
    },
}

#[derive(Args, Debug, Clone)]
pub struct GapsArgs {
    /// Run ID or file path to inspect (default: latest)
    #[arg(long)]
    pub run: Option<String>,

    /// Output format: table or json [default: table]
    #[arg(long, default_value = "table", value_parser = ["table", "json"])]
    pub format: String,

    /// Force-disable colored output
    #[arg(long)]
    pub no_color: bool,
}

#[derive(Args, Debug, Clone)]
pub struct FileArgs {
    /// Path to file
    pub path: PathBuf,

    /// Run ID to inspect (default: latest or re-scored)
    #[arg(long)]
    pub run: Option<String>,

    /// Output format: table or json [default: table]
    #[arg(long, default_value = "table", value_parser = ["table", "json"])]
    pub format: String,

    /// Force-disable colored output
    #[arg(long)]
    pub no_color: bool,
}

#[derive(Args, Debug, Clone)]
pub struct DiffArgs {
    /// First run ID or file path
    pub run_a: String,

    /// Second run ID or file path
    pub run_b: String,

    /// Output format: table or json [default: table]
    #[arg(long, default_value = "table", value_parser = ["table", "json"])]
    pub format: String,

    /// Force-disable colored output
    #[arg(long)]
    pub no_color: bool,
}

#[derive(Subcommand, Debug, Clone)]
pub enum RunsCommand {
    /// Lists saved run IDs under .qualitycheck/runs/
    List,
}
