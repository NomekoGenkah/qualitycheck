use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use directories::BaseDirs;
use is_terminal::IsTerminal;
use serde::{Deserialize, Serialize};

use crate::error::{ConfigError, QualityCheckError};

pub const DEFAULT_JEV_URL: &str = "https://api.typesafe.ai/v1/systemone";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jev_url: Option<String>,
}

pub fn get_config_dir() -> Result<PathBuf, ConfigError> {
    if let Ok(dir) = std::env::var("QUALITYCHECK_CONFIG_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let base_dirs = BaseDirs::new().ok_or(ConfigError::HomeDirNotFound)?;
    Ok(base_dirs.config_dir().join("qualitycheck"))
}

pub fn get_config_file_path() -> Result<PathBuf, ConfigError> {
    Ok(get_config_dir()?.join("config.toml"))
}

pub fn get_profiles_dir() -> Result<PathBuf, ConfigError> {
    Ok(get_config_dir()?.join("profiles"))
}

pub fn load_config() -> Result<Option<Config>, ConfigError> {
    let path = get_config_file_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&path).map_err(|e| ConfigError::ReadError(path.clone(), e))?;
    let config: Config = toml::from_str(&content).map_err(|e| ConfigError::ParseError(path, e))?;
    Ok(Some(config))
}

pub fn save_config(config: &Config) -> Result<PathBuf, ConfigError> {
    let config_dir = get_config_dir()?;
    fs::create_dir_all(&config_dir).map_err(|e| ConfigError::WriteError(config_dir.clone(), e))?;

    let path = config_dir.join("config.toml");
    let content = toml::to_string_pretty(config).map_err(ConfigError::SerializeError)?;
    fs::write(&path, content).map_err(|e| ConfigError::WriteError(path.clone(), e))?;
    Ok(path)
}

pub fn resolve_api_key(cli_key: Option<&str>) -> Result<String, ConfigError> {
    // 1. Explicit CLI argument
    if let Some(key) = cli_key {
        let trimmed = key.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }

    // 2. JEV_API_KEY environment variable
    if let Ok(key) = std::env::var("JEV_API_KEY") {
        let trimmed = key.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }

    // 3. Saved config.toml
    if let Ok(Some(config)) = load_config()
        && let Some(key) = config.api_key {
            let trimmed = key.trim();
            if !trimmed.is_empty() {
                return Ok(trimmed.to_string());
            }
        }

    Err(ConfigError::MissingApiKey)
}

pub fn resolve_jev_url() -> String {
    if let Ok(url) = std::env::var("JEV_API_URL") {
        let trimmed = url.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    if let Ok(Some(config)) = load_config()
        && let Some(url) = config.jev_url {
            let trimmed = url.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }

    DEFAULT_JEV_URL.to_string()
}

pub fn install_default_profiles() -> Result<usize, std::io::Error> {
    let profiles_dir = match get_profiles_dir() {
        Ok(dir) => dir,
        Err(_) => return Ok(0),
    };

    fs::create_dir_all(&profiles_dir)?;

    let mut installed_count = 0;
    for (name, content) in crate::profile::BUILTIN_PROFILES {
        let target = profiles_dir.join(format!("{name}.json"));
        // Refresh unedited copies of older built-ins; never touch profiles the user changed.
        let is_stale_copy = fs::read_to_string(&target)
            .is_ok_and(|existing| crate::profile::is_superseded_builtin(&existing));
        if !target.exists() || is_stale_copy {
            fs::write(&target, content)?;
            installed_count += 1;
        }
    }

    Ok(installed_count)
}

pub fn add_to_gitignore_if_needed(repo_root: &Path) -> Result<bool, std::io::Error> {
    let gitignore_path = repo_root.join(".gitignore");
    let entry = ".qualitycheck/";

    if gitignore_path.exists() {
        let content = fs::read_to_string(&gitignore_path)?;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed == entry || trimmed == ".qualitycheck" || trimmed == "/.qualitycheck/" {
                return Ok(false);
            }
        }

        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&gitignore_path)?;

        if !content.is_empty() && !content.ends_with('\n') {
            writeln!(file)?;
        }
        writeln!(file, "\n# qualitycheck cache and run reports\n{}", entry)?;
        Ok(true)
    } else {
        fs::write(&gitignore_path, format!("# qualitycheck cache and run reports\n{}\n", entry))?;
        Ok(true)
    }
}

pub fn execute_init(
    api_key_arg: Option<String>,
    non_interactive: bool,
    interactive_api_key_prompt: bool,
) -> Result<(), QualityCheckError> {
    let is_interactive = !non_interactive && io::stdin().is_terminal() && interactive_api_key_prompt;

    let api_key = if let Some(key) = api_key_arg {
        key
    } else if let Ok(key) = std::env::var("JEV_API_KEY") {
        key
    } else if is_interactive {
        println!("Welcome to qualitycheck. This tool evaluates your code's quality using AI.");
        print!("Enter your Jev (TypeSafe AI) API key: ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let trimmed = input.trim().to_string();
        if trimmed.is_empty() {
            return Err(ConfigError::MissingApiKey.into());
        }
        trimmed
    } else {
        return Err(ConfigError::MissingApiKey.into());
    };

    let mut current_config = load_config().unwrap_or(None).unwrap_or_default();
    current_config.api_key = Some(api_key);
    let saved_path = save_config(&current_config)?;

    println!("✔ Config saved to {}", saved_path.display());

    install_default_profiles().map_err(|e| {
        QualityCheckError::Profile(crate::error::ProfileError::IoError(
            get_profiles_dir().unwrap_or_default(),
            e,
        ))
    })?;
    println!(
        "✔ Default profiles installed to {}",
        get_profiles_dir().unwrap_or_default().display()
    );

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if cwd.join(".git").exists() {
        let added = add_to_gitignore_if_needed(&cwd)?;
        if added {
            println!("✔ Added .qualitycheck/ to .gitignore");
        }
    }

    Ok(())
}
