//! # lcw-config
//!
//! The Manifesto, encoded as machine-readable policy. The CLI and UI both load
//! a `livewalk.toml` that tells the engine which lenses to run, what quality
//! thresholds apply (Manifesto section 4), which optimization targets to weigh
//! (section 5), and how to parse.
//!
//! Every section derives `Default` with the *documented* defaults, and uses
//! `#[serde(default)]`, so a partial (or empty) config file is always valid.

use std::path::{Path, PathBuf};

use lcw_core::vertical::Vertical;
use serde::{Deserialize, Serialize};

/// Errors from loading or validating a config.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not read config {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid TOML in {path}: {source}")]
    Toml {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("invalid config: {0}")]
    Invalid(String),
}

/// The full configuration tree.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub project: ProjectConfig,
    pub adapter: AdapterConfig,
    pub lenses: LensesConfig,
    pub metrics: MetricsConfig,
    pub suggestions: SuggestionsConfig,
    pub telemetry: TelemetryConfig,
}

/// `[project]` — identity and the industry vertical (Manifesto section 5).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectConfig {
    pub name: Option<String>,
    /// `auto` (default) detects the vertical from the code; otherwise it is
    /// pinned to a specific one.
    pub vertical: VerticalChoice,
}

impl Default for ProjectConfig {
    fn default() -> Self {
        ProjectConfig {
            name: None,
            vertical: VerticalChoice::Auto,
        }
    }
}

/// Declared vertical, or `Auto` to let the suggestion engine detect it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VerticalChoice {
    Auto,
    Embedded,
    GameEngine,
    Backend,
    FullStack,
}

impl VerticalChoice {
    /// The fixed vertical, or `None` when set to `Auto`.
    pub fn fixed(self) -> Option<Vertical> {
        match self {
            VerticalChoice::Auto => None,
            VerticalChoice::Embedded => Some(Vertical::Embedded),
            VerticalChoice::GameEngine => Some(Vertical::GameEngine),
            VerticalChoice::Backend => Some(Vertical::Backend),
            VerticalChoice::FullStack => Some(Vertical::FullStack),
        }
    }
}

/// How Layer 1 parses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterMode {
    /// tree-sitter + heuristic resolution. Fast, scales to giant repos.
    Fast,
    /// rust-analyzer semantic resolution. Precise, heavier. (Feature-gated in
    /// the engine; falls back to `Fast` if unavailable.)
    Semantic,
}

/// `[adapter]` — parsing controls.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AdapterConfig {
    pub mode: AdapterMode,
    /// Extra glob patterns to exclude (in addition to the built-in ignores
    /// like `target/`).
    pub exclude: Vec<String>,
    pub follow_symlinks: bool,
}

impl Default for AdapterConfig {
    fn default() -> Self {
        AdapterConfig {
            mode: AdapterMode::Fast,
            exclude: Vec::new(),
            follow_symlinks: false,
        }
    }
}

/// `[lenses]` — which Layer 2 analyses to run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LensesConfig {
    pub complexity: bool,
    pub purity: bool,
    pub hazards: bool,
    pub layering: bool,
    pub paradigm: bool,
    /// Architecture styles to check layering against: any of `clean`, `onion`,
    /// `mvc`.
    pub architecture: Vec<String>,
}

impl Default for LensesConfig {
    fn default() -> Self {
        LensesConfig {
            complexity: true,
            purity: true,
            hazards: true,
            layering: true,
            paradigm: true,
            architecture: vec!["clean".to_string()],
        }
    }
}

/// `[metrics]` — quality thresholds (Manifesto section 4).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MetricsConfig {
    /// Cyclomatic complexity above which a function is flagged.
    pub cyclomatic_max: u32,
    pub max_parameters: u32,
    pub max_nesting: u32,
    pub max_function_loc: u32,
    /// Informational target for test coverage (0.0..=1.0).
    pub coverage_min: f64,
    /// Multiplier applied to heap-allocation heuristics (higher = stricter).
    pub heap_sensitivity: f64,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        MetricsConfig {
            cyclomatic_max: 10,
            max_parameters: 5,
            max_nesting: 4,
            max_function_loc: 60,
            coverage_min: 0.7,
            heap_sensitivity: 1.0,
        }
    }
}

/// `[suggestions]` — relative weight of each optimization target and a cap on
/// how many suggestions to emit.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SuggestionsConfig {
    pub scalability: f64,
    pub maintainability: f64,
    pub robustness: f64,
    pub latency: f64,
    pub performance: f64,
    pub max_suggestions: usize,
}

impl Default for SuggestionsConfig {
    fn default() -> Self {
        SuggestionsConfig {
            scalability: 1.0,
            maintainability: 1.0,
            robustness: 1.0,
            latency: 1.0,
            performance: 1.0,
            max_suggestions: 50,
        }
    }
}

impl SuggestionsConfig {
    /// Weight for a given target.
    pub fn weight(&self, target: lcw_core::Target) -> f64 {
        use lcw_core::Target::*;
        match target {
            Scalability => self.scalability,
            Maintainability => self.maintainability,
            Robustness => self.robustness,
            Latency => self.latency,
            Performance => self.performance,
        }
    }
}

/// `[telemetry]` — observability controls (Principle III).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TelemetryConfig {
    pub level: String,
    /// Hard guarantee that no network exporter is ever enabled.
    pub no_network: bool,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        TelemetryConfig {
            level: "info".to_string(),
            no_network: true,
        }
    }
}

/// Default config file name searched for by [`find`].
pub const CONFIG_FILE_NAME: &str = "livewalk.toml";

impl Config {
    /// Parse a config from a TOML string.
    pub fn from_toml(text: &str, origin: &Path) -> Result<Config, ConfigError> {
        let cfg: Config = toml::from_str(text).map_err(|source| ConfigError::Toml {
            path: origin.to_path_buf(),
            source,
        })?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Load and validate a config from a file path.
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Config::from_toml(&text, path)
    }

    /// Resolve a config: use `explicit` if given, else search upward from
    /// `start` for a `livewalk.toml`, else fall back to defaults.
    pub fn resolve(explicit: Option<&Path>, start: &Path) -> Result<Config, ConfigError> {
        if let Some(path) = explicit {
            return Config::load(path);
        }
        match find(start) {
            Some(path) => Config::load(&path),
            None => Ok(Config::default()),
        }
    }

    /// Check invariants that serde alone can't express.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let m = &self.metrics;
        if !(0.0..=1.0).contains(&m.coverage_min) {
            return Err(ConfigError::Invalid(
                "metrics.coverage_min must be within 0.0..=1.0".into(),
            ));
        }
        if m.heap_sensitivity < 0.0 {
            return Err(ConfigError::Invalid(
                "metrics.heap_sensitivity must be >= 0".into(),
            ));
        }
        for arch in &self.lenses.architecture {
            if !matches!(arch.as_str(), "clean" | "onion" | "mvc") {
                return Err(ConfigError::Invalid(format!(
                    "unknown architecture style '{arch}' (expected clean|onion|mvc)"
                )));
            }
        }
        let s = &self.suggestions;
        for (name, w) in [
            ("scalability", s.scalability),
            ("maintainability", s.maintainability),
            ("robustness", s.robustness),
            ("latency", s.latency),
            ("performance", s.performance),
        ] {
            if w < 0.0 {
                return Err(ConfigError::Invalid(format!(
                    "suggestions.{name} weight must be >= 0"
                )));
            }
        }
        Ok(())
    }
}

/// Search `start` and its ancestors for a [`CONFIG_FILE_NAME`].
pub fn find(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let candidate = d.join(CONFIG_FILE_NAME);
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = d.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_uses_documented_defaults() {
        let cfg = Config::from_toml("", Path::new("livewalk.toml")).unwrap();
        assert_eq!(cfg.metrics.cyclomatic_max, 10);
        assert_eq!(cfg.adapter.mode, AdapterMode::Fast);
        assert!(cfg.telemetry.no_network);
        assert_eq!(cfg.project.vertical, VerticalChoice::Auto);
        assert_eq!(cfg.lenses.architecture, vec!["clean".to_string()]);
    }

    #[test]
    fn partial_override_keeps_other_defaults() {
        let toml = r#"
            [metrics]
            cyclomatic_max = 20

            [project]
            vertical = "game-engine"
        "#;
        let cfg = Config::from_toml(toml, Path::new("livewalk.toml")).unwrap();
        assert_eq!(cfg.metrics.cyclomatic_max, 20);
        assert_eq!(cfg.metrics.max_parameters, 5); // default preserved
        assert_eq!(
            cfg.project.vertical.fixed(),
            Some(lcw_core::Vertical::GameEngine)
        );
    }

    #[test]
    fn rejects_out_of_range_coverage() {
        let toml = "[metrics]\ncoverage_min = 2.0\n";
        assert!(Config::from_toml(toml, Path::new("x.toml")).is_err());
    }

    #[test]
    fn rejects_unknown_architecture() {
        let toml = "[lenses]\narchitecture = [\"hexagonal\"]\n";
        assert!(Config::from_toml(toml, Path::new("x.toml")).is_err());
    }
}
