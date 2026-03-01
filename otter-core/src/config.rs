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
    pub max_attempts: i32,
    pub worker_concurrency: usize,
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
            max_attempts: env::var("OTTER_MAX_ATTEMPTS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(5),
            worker_concurrency: env::var("OTTER_WORKER_CONCURRENCY")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(1),
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
