use anyhow::Result;
use async_trait::async_trait;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueMessage {
    pub job_id: Uuid,
}

#[async_trait]
pub trait Queue: Send + Sync {
    async fn enqueue(&self, queue_name: &str, message: &QueueMessage) -> Result<()>;
    async fn dequeue(&self, queue_name: &str) -> Result<Option<QueueMessage>>;
}

#[derive(Clone)]
pub struct RedisQueue {
    connection: std::sync::Arc<Mutex<redis::aio::MultiplexedConnection>>,
}

impl RedisQueue {
    pub async fn connect(redis_url: &str) -> Result<Self> {
        let client = redis::Client::open(redis_url)?;
        let connection = client.get_multiplexed_async_connection().await?;
        Ok(Self {
            connection: std::sync::Arc::new(Mutex::new(connection)),
        })
    }
}

#[async_trait]
impl Queue for RedisQueue {
    async fn enqueue(&self, queue_name: &str, message: &QueueMessage) -> Result<()> {
        let payload = serde_json::to_string(message)?;
        let mut conn = self.connection.lock().await;
        conn.rpush::<_, _, ()>(queue_name, payload).await?;
        Ok(())
    }

    async fn dequeue(&self, queue_name: &str) -> Result<Option<QueueMessage>> {
        let mut conn = self.connection.lock().await;
        let value: Option<String> = conn.lpop(queue_name, None).await?;
        match value {
            Some(raw) => Ok(Some(serde_json::from_str(&raw)?)),
            None => Ok(None),
        }
    }
}
