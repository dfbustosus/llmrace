//! Named benchmark profiles loaded from `llmrace.toml`.
//!
//! Lookup order: `./llmrace.toml`, then `$XDG_CONFIG_HOME/llmrace/config.toml`
//! (falling back to `~/.config/llmrace/config.toml`). Values are merged with
//! CLI flags: explicit CLI flags win over the profile, the profile wins over
//! built-in defaults.

use crate::error::{Error, Result};
use serde::Deserialize;
use std::path::PathBuf;

fn de_opt_f64<'de, D>(deserializer: D) -> std::result::Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<toml::Value>::deserialize(deserializer)?;
    match value {
        None => Ok(None),
        Some(toml::Value::Float(f)) => Ok(Some(f)),
        Some(toml::Value::Integer(i)) => Ok(Some(i as f64)),
        Some(other) => Err(serde::de::Error::custom(format!(
            "expected a number, got {other}"
        ))),
    }
}

/// One named profile inside the config file's `[profiles.<name>]` table.
///
/// Unknown keys are rejected so typos (`run` instead of `runs`) fail loudly
/// instead of silently falling back to defaults.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// Providers to race (CLI ids).
    #[serde(default)]
    pub providers: Vec<String>,
    /// Model override for every provider.
    #[serde(default)]
    pub model: Option<String>,
    /// Prompt text.
    #[serde(default)]
    pub prompt: Option<String>,
    /// Measured rounds per provider.
    #[serde(default)]
    pub runs: Option<usize>,
    /// Warmup rounds excluded from statistics.
    #[serde(default)]
    pub warmup: Option<usize>,
    /// Concurrent requests per provider per round.
    #[serde(default)]
    pub concurrency: Option<usize>,
    /// Sampling cap passed as max_tokens.
    #[serde(default)]
    pub max_tokens: Option<u64>,
    /// Per-request timeout in seconds.
    #[serde(default)]
    pub timeout: Option<u64>,
    /// Additional attempts for failed rounds.
    #[serde(default)]
    pub retries: Option<usize>,
    /// Sampling temperature (`0.0` is valid; integers are accepted).
    #[serde(default, deserialize_with = "de_opt_f64")]
    pub temperature: Option<f64>,
    /// Nucleus sampling top-p.
    #[serde(default, deserialize_with = "de_opt_f64")]
    pub top_p: Option<f64>,
    /// Deterministic seed for providers that support one.
    #[serde(default)]
    pub seed: Option<u64>,
    /// System prompt prepended to every request.
    #[serde(default)]
    pub system: Option<String>,
}

/// Root of a `llmrace.toml` file.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Config {
    /// Named profiles.
    #[serde(default)]
    pub profiles: std::collections::BTreeMap<String, Profile>,
    /// Custom prompt variants for `--sweep` (name → prompt; override
    /// built-ins by name).
    #[serde(default)]
    pub variants: std::collections::BTreeMap<String, String>,
}

impl Config {
    /// Parses config TOML text.
    pub fn from_toml_str(raw: &str) -> Result<Self> {
        toml::from_str(raw).map_err(|e| Error::Config(format!("invalid llmrace.toml: {e}")))
    }

    /// Loads the first existing config file from the standard locations.
    ///
    /// Returns an empty config when no file exists.
    pub fn discover() -> Result<Self> {
        for path in candidate_paths() {
            if path.is_file() {
                let raw = std::fs::read_to_string(&path)?;
                return Self::from_toml_str(&raw);
            }
        }
        Ok(Config::default())
    }

    /// Returns a profile by name, erroring with the available names otherwise.
    pub fn profile(&self, name: &str) -> Result<&Profile> {
        self.profiles.get(name).ok_or_else(|| {
            Error::Config(format!(
                "profile '{name}' not found; available: {}",
                self.available()
            ))
        })
    }

    /// Comma-separated profile names (empty string when none).
    pub fn available(&self) -> String {
        self.profiles.keys().cloned().collect::<Vec<_>>().join(", ")
    }
}

fn candidate_paths() -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from("llmrace.toml")];
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        paths.push(PathBuf::from(dir).join("llmrace").join("config.toml"));
    } else if let Some(home) = std::env::var_os("HOME") {
        paths.push(
            PathBuf::from(home)
                .join(".config")
                .join("llmrace")
                .join("config.toml"),
        );
    }
    paths
}
