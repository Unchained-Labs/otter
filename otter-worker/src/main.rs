use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use otter_core::config::AppConfig;
use otter_core::db::Database;
use otter_core::queue::RedisQueue;
use otter_core::service::OtterService;
use tokio::task::JoinSet;
use tokio::time::sleep;
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = AppConfig::from_env()?;
    let database = connect_database_with_retry(&config.database_url).await?;
    migrate_with_retry(database.clone()).await?;
    let queue = connect_redis_with_retry(&config.redis_url).await?;
    let service = Arc::new(OtterService::new(&config, database, queue));

    info!(
        concurrency = config.worker_concurrency,
        "otter-worker started"
    );

    let mut workers = JoinSet::new();
    for worker_id in 0..config.worker_concurrency.max(1) {
        let service = service.clone();
        workers.spawn(async move {
            worker_loop(worker_id, service).await;
        });
    }

    while workers.join_next().await.is_some() {
        error!("a worker task exited unexpectedly");
    }

    Ok(())
}

async fn worker_loop(worker_id: usize, service: Arc<OtterService<RedisQueue>>) {
    let mut idle_ticks: u32 = 0;
    loop {
        match service.process_next_job().await {
            Ok(Some(job_id)) => {
                idle_ticks = 0;
                info!(%job_id, worker_id, "processed queue job");
            }
            Ok(None) => {
                idle_ticks = idle_ticks.saturating_add(1);
                if idle_ticks % 30 == 0 {
                    info!(
                        worker_id,
                        idle_seconds = idle_ticks,
                        "worker idle; waiting for jobs"
                    );
                }
                sleep(Duration::from_secs(1)).await
            }
            Err(err) => {
                error!(error = %err, worker_id, "worker loop error");
                sleep(Duration::from_secs(2)).await;
            }
        }
    }
}

async fn connect_database_with_retry(database_url: &str) -> Result<Arc<Database>> {
    let mut attempts: u32 = 0;
    loop {
        attempts += 1;
        match Database::connect(database_url).await {
            Ok(database) => {
                info!(attempts, "connected to postgres");
                return Ok(Arc::new(database));
            }
            Err(error) if attempts < 30 => {
                warn!(attempts, error = %error, "postgres connect failed; retrying");
                sleep(Duration::from_secs(2)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

async fn migrate_with_retry(database: Arc<Database>) -> Result<()> {
    let mut attempts: u32 = 0;
    loop {
        attempts += 1;
        match database.migrate().await {
            Ok(()) => {
                info!(attempts, "database migrations applied");
                return Ok(());
            }
            Err(error) if attempts < 20 => {
                warn!(attempts, error = %error, "database migration failed; retrying");
                sleep(Duration::from_secs(2)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

async fn connect_redis_with_retry(redis_url: &str) -> Result<Arc<RedisQueue>> {
    let mut attempts: u32 = 0;
    loop {
        attempts += 1;
        match RedisQueue::connect(redis_url).await {
            Ok(queue) => {
                info!(attempts, "connected to redis");
                return Ok(Arc::new(queue));
            }
            Err(error) if attempts < 30 => {
                warn!(attempts, error = %error, "redis connect failed; retrying");
                sleep(Duration::from_secs(2)).await;
            }
            Err(error) => return Err(error),
        }
    }
}
