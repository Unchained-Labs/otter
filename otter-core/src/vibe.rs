use std::path::Path;
use std::process::Stdio;
use std::{future::Future, string::String};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub struct VibeExecutor {
    pub vibe_bin: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VibeExecutionResult {
    pub assistant_output: String,
    pub raw_json: serde_json::Value,
    pub stderr: String,
    pub exit_code: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VibeStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VibeOutputChunk {
    pub stream: VibeStream,
    pub line: String,
}

impl VibeExecutor {
    pub fn new(vibe_bin: String) -> Self {
        Self { vibe_bin }
    }

    pub async fn run_prompt(
        &self,
        prompt: &str,
        workspace_path: &Path,
        isolated_vibe_home: &Path,
    ) -> Result<VibeExecutionResult> {
        let output = Command::new(&self.vibe_bin)
            .arg("--prompt")
            .arg(prompt)
            .arg("--output")
            .arg("json")
            .arg("--workdir")
            .arg(workspace_path.as_os_str())
            .env("VIBE_HOME", isolated_vibe_home.as_os_str())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .with_context(|| format!("failed to execute {}", self.vibe_bin))?;

        let exit_code = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8(output.stdout).context("invalid UTF-8 on vibe stdout")?;
        let stderr = String::from_utf8(output.stderr).context("invalid UTF-8 on vibe stderr")?;

        if !output.status.success() {
            bail!("vibe exited with code {exit_code}: {stderr}");
        }

        let raw_json: serde_json::Value =
            serde_json::from_str(&stdout).context("vibe output was not valid JSON")?;

        let assistant_output = extract_latest_assistant_message(&raw_json).unwrap_or_default();

        Ok(VibeExecutionResult {
            assistant_output,
            raw_json,
            stderr,
            exit_code,
        })
    }

    pub async fn run_prompt_streaming<F, Fut>(
        &self,
        prompt: &str,
        workspace_path: &Path,
        isolated_vibe_home: &Path,
        mut on_chunk: F,
    ) -> Result<VibeExecutionResult>
    where
        F: FnMut(VibeOutputChunk) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        let mut child = Command::new(&self.vibe_bin)
            .arg("--prompt")
            .arg(prompt)
            .arg("--output")
            .arg("json")
            .arg("--workdir")
            .arg(workspace_path.as_os_str())
            .env("VIBE_HOME", isolated_vibe_home.as_os_str())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to execute {}", self.vibe_bin))?;

        let stdout = child
            .stdout
            .take()
            .context("missing vibe stdout pipe for streaming")?;
        let stderr = child
            .stderr
            .take()
            .context("missing vibe stderr pipe for streaming")?;

        let (tx, mut rx) = mpsc::unbounded_channel::<VibeOutputChunk>();
        let tx_stdout = tx.clone();
        let stdout_task = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Some(line) = lines.next_line().await? {
                let _ = tx_stdout.send(VibeOutputChunk {
                    stream: VibeStream::Stdout,
                    line,
                });
            }
            Ok::<(), anyhow::Error>(())
        });
        let tx_stderr = tx.clone();
        let stderr_task = tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Some(line) = lines.next_line().await? {
                let _ = tx_stderr.send(VibeOutputChunk {
                    stream: VibeStream::Stderr,
                    line,
                });
            }
            Ok::<(), anyhow::Error>(())
        });
        drop(tx);

        let mut stdout_buffer = String::new();
        let mut stderr_buffer = String::new();

        while let Some(chunk) = rx.recv().await {
            match chunk.stream {
                VibeStream::Stdout => {
                    stdout_buffer.push_str(&chunk.line);
                    stdout_buffer.push('\n');
                }
                VibeStream::Stderr => {
                    stderr_buffer.push_str(&chunk.line);
                    stderr_buffer.push('\n');
                }
            }
            on_chunk(chunk).await?;
        }

        stdout_task
            .await
            .context("vibe stdout reader task panicked")??;
        stderr_task
            .await
            .context("vibe stderr reader task panicked")??;
        let status = child
            .wait()
            .await
            .context("failed to wait for vibe process")?;

        let exit_code = status.code().unwrap_or(-1);
        if !status.success() {
            bail!("vibe exited with code {exit_code}: {stderr_buffer}");
        }

        let raw_json: serde_json::Value =
            serde_json::from_str(&stdout_buffer).context("vibe output was not valid JSON")?;
        let assistant_output = extract_latest_assistant_message(&raw_json).unwrap_or_default();

        Ok(VibeExecutionResult {
            assistant_output,
            raw_json,
            stderr: stderr_buffer,
            exit_code,
        })
    }
}

fn extract_latest_assistant_message(value: &serde_json::Value) -> Option<String> {
    let messages = value.as_array()?;
    messages.iter().rev().find_map(|msg| {
        let role = msg.get("role")?.as_str()?;
        if role != "assistant" {
            return None;
        }
        msg.get("content")?.as_str().map(ToString::to_string)
    })
}

#[cfg(test)]
mod tests {
    use super::extract_latest_assistant_message;

    #[test]
    fn finds_last_assistant_message() {
        let payload = serde_json::json!([
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": "one"},
            {"role": "assistant", "content": "two"}
        ]);
        let content = extract_latest_assistant_message(&payload);
        assert_eq!(content.as_deref(), Some("two"));
    }
}
