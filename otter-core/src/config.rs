use std::env;
use std::path::PathBuf;

use anyhow::{Context, Result};

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub database_url: String,
    pub redis_url: String,
    pub listen_addr: String,
    pub cors_allowed_origins: Vec<String>,
    pub vibe_bin: String,
    pub vibe_base_home: PathBuf,
    pub vibe_model: Option<String>,
    pub vibe_provider: Option<String>,
    pub vibe_extra_env: Vec<(String, String)>,
    pub default_workspace_path: Option<PathBuf>,
    pub default_workspace_subdir: String,
    pub lavoix_url: String,
    pub otter_api_base_url: String,
    pub max_attempts: i32,
    pub worker_concurrency: usize,
    pub scheduling: SchedulingConfig,
    pub runtime: RuntimeConfig,
}

/// How the worker chooses which runnable job to claim next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchedulingStrategy {
    /// Oldest first. Ignores both priority and intensity.
    Fifo,
    /// Explicit queue position only, oldest first within a position.
    Priority,
    /// Explicit position first, then cheapest work first, with ageing so long
    /// jobs cannot be starved by a stream of short ones.
    Smart,
}

impl SchedulingStrategy {
    fn from_env_value(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "fifo" => Self::Fifo,
            "priority" => Self::Priority,
            _ => Self::Smart,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fifo => "fifo",
            Self::Priority => "priority",
            Self::Smart => "smart",
        }
    }
}

#[derive(Clone, Debug)]
pub struct SchedulingConfig {
    pub strategy: SchedulingStrategy,
    /// Every this many seconds spent waiting, a job sheds `aging_step` intensity.
    ///
    /// The defaults shed the full 0..=100 range over roughly three hours. Ageing
    /// much faster than that makes the queue effectively FIFO — a build job that
    /// runs for an hour would age past every newcomer before it ever gets picked
    /// — which throws away the throughput gain the scoring exists to provide.
    pub aging_interval_seconds: i64,
    /// Intensity shed per ageing interval.
    pub aging_step: i32,
}

#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    pub enabled: bool,
    pub docker_socket: String,
    pub network_name: String,
    pub container_name_prefix: String,
    pub image_tag_prefix: String,
    pub default_host: String,
    pub max_log_lines: usize,
}

impl AppConfig {
    pub fn from_env() -> Result<Self> {
        let _ = dotenvy::dotenv();
        let database_url = env::var("OTTER_DATABASE_URL")
            .context("missing OTTER_DATABASE_URL environment variable")?;
        let redis_url =
            env::var("OTTER_REDIS_URL").context("missing OTTER_REDIS_URL environment variable")?;

        Ok(Self {
            database_url,
            redis_url,
            listen_addr: env::var("OTTER_LISTEN_ADDR")
                .unwrap_or_else(|_| "0.0.0.0:8080".to_string()),
            cors_allowed_origins: env::var("OTTER_CORS_ALLOWED_ORIGINS")
                .ok()
                .map(|value| {
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|origin| !origin.is_empty())
                        .map(ToOwned::to_owned)
                        .collect::<Vec<_>>()
                })
                .filter(|origins| !origins.is_empty())
                .unwrap_or_else(|| vec!["http://localhost:5173".to_string()]),
            vibe_bin: env::var("OTTER_VIBE_BIN").unwrap_or_else(|_| "vibe".to_string()),
            vibe_base_home: PathBuf::from(
                env::var("OTTER_VIBE_HOME_BASE").unwrap_or_else(|_| "/var/lib/otter/vibe".into()),
            ),
            vibe_model: env::var("OTTER_VIBE_MODEL")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            vibe_provider: env::var("OTTER_VIBE_PROVIDER")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            vibe_extra_env: parse_vibe_extra_env(
                env::var("OTTER_VIBE_EXTRA_ENV")
                    .unwrap_or_default()
                    .as_str(),
            ),
            default_workspace_path: env::var("OTTER_DEFAULT_WORKSPACE_PATH")
                .ok()
                .map(PathBuf::from),
            default_workspace_subdir: env::var("OTTER_DEFAULT_WORKSPACE_SUBDIR")
                .unwrap_or_else(|_| "auto".to_string()),
            lavoix_url: env::var("OTTER_LAVOIX_URL")
                .unwrap_or_else(|_| "http://lavoix:8090".to_string()),
            otter_api_base_url: env::var("OTTER_API_BASE_URL")
                .unwrap_or_else(|_| "http://otter-server:8080".to_string()),
            max_attempts: env::var("OTTER_MAX_ATTEMPTS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(5),
            worker_concurrency: env::var("OTTER_WORKER_CONCURRENCY")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(1),
            scheduling: SchedulingConfig {
                strategy: env::var("OTTER_SCHEDULING_STRATEGY")
                    .ok()
                    .map(|value| SchedulingStrategy::from_env_value(&value))
                    .unwrap_or(SchedulingStrategy::Smart),
                aging_interval_seconds: env::var("OTTER_SCHEDULING_AGING_SECONDS")
                    .ok()
                    .and_then(|value| value.parse::<i64>().ok())
                    .filter(|value| *value > 0)
                    .unwrap_or(600),
                aging_step: env::var("OTTER_SCHEDULING_AGING_STEP")
                    .ok()
                    .and_then(|value| value.parse::<i32>().ok())
                    .filter(|value| *value > 0)
                    .unwrap_or(5),
            },
            runtime: RuntimeConfig {
                enabled: env::var("OTTER_RUNTIME_ENABLED")
                    .ok()
                    .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
                    .unwrap_or(false),
                docker_socket: env::var("OTTER_RUNTIME_DOCKER_SOCKET")
                    .unwrap_or_else(|_| "unix:///var/run/docker.sock".to_string()),
                network_name: env::var("OTTER_RUNTIME_NETWORK")
                    .unwrap_or_else(|_| "kymatics_default".to_string()),
                container_name_prefix: env::var("OTTER_RUNTIME_CONTAINER_PREFIX")
                    .unwrap_or_else(|_| "otter-ws".to_string()),
                image_tag_prefix: env::var("OTTER_RUNTIME_IMAGE_PREFIX")
                    .unwrap_or_else(|_| "otter/workspace".to_string()),
                default_host: env::var("OTTER_RUNTIME_DEFAULT_HOST")
                    .unwrap_or_else(|_| "http://localhost".to_string()),
                max_log_lines: env::var("OTTER_RUNTIME_MAX_LOG_LINES")
                    .ok()
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(2000),
            },
        })
    }
}

fn parse_vibe_extra_env(raw: &str) -> Vec<(String, String)> {
    raw.split(',')
        .filter_map(|pair| {
            let trimmed = pair.trim();
            if trimmed.is_empty() {
                return None;
            }
            let (key, value) = trimmed.split_once('=')?;
            let key = key.trim();
            let value = value.trim();
            if key.is_empty() || value.is_empty() {
                return None;
            }
            Some((key.to_string(), value.to_string()))
        })
        .collect()
}
