use std::path::Path;
use std::process::Stdio;
use std::{future::Future, string::String};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration, MissedTickBehavior};

#[derive(Debug, Clone)]
pub struct VibeExecutor {
    pub vibe_bin: String,
    pub otter_api_base_url: String,
    pub vibe_model: Option<String>,
    pub vibe_provider: Option<String>,
    pub vibe_extra_env: Vec<(String, String)>,
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
    pub fn new(
        vibe_bin: String,
        otter_api_base_url: String,
        vibe_model: Option<String>,
        vibe_provider: Option<String>,
        vibe_extra_env: Vec<(String, String)>,
    ) -> Self {
        Self {
            vibe_bin,
            otter_api_base_url,
            vibe_model,
            vibe_provider,
            vibe_extra_env,
        }
    }

    fn apply_model_and_provider_env(&self, command: &mut Command) {
        if let Some(model) = &self.vibe_model {
            // Set both keys for compatibility with different Vibe/Mistral env conventions.
            command.env("VIBE_MODEL", model);
            command.env("MISTRAL_MODEL", model);
        }
        if let Some(provider) = &self.vibe_provider {
            command.env("VIBE_PROVIDER", provider);
        }
        for (key, value) in &self.vibe_extra_env {
            command.env(key, value);
        }
    }

    pub async fn run_prompt(
        &self,
        prompt: &str,
        job_id: uuid::Uuid,
        workspace_path: &Path,
        isolated_vibe_home: &Path,
        project_path: Option<&str>,
    ) -> Result<VibeExecutionResult> {
        let effective_prompt =
            compose_vibe_prompt(prompt, job_id, &self.otter_api_base_url, project_path);
        let mut cmd = Command::new(&self.vibe_bin);
        cmd.arg("--prompt")
            .arg(&effective_prompt)
            .arg("--output")
            .arg("streaming")
            .arg("--workdir")
            .arg(workspace_path.as_os_str())
            .env("VIBE_HOME", isolated_vibe_home.as_os_str())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        self.apply_model_and_provider_env(&mut cmd);
        let output = cmd
            .output()
            .await
            .with_context(|| format!("failed to execute {}", self.vibe_bin))?;

        let exit_code = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8(output.stdout).context("invalid UTF-8 on vibe stdout")?;
        let stderr = String::from_utf8(output.stderr).context("invalid UTF-8 on vibe stderr")?;

        if !output.status.success() {
            bail!("vibe exited with code {exit_code}: {stderr}");
        }

        let raw_json = parse_streaming_json_lines(&stdout);
        let assistant_output = extract_latest_assistant_message(&raw_json).unwrap_or_default();

        Ok(VibeExecutionResult {
            assistant_output,
            raw_json,
            stderr,
            exit_code,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn run_prompt_streaming<F, Fut, C, CFut>(
        &self,
        prompt: &str,
        job_id: uuid::Uuid,
        workspace_path: &Path,
        isolated_vibe_home: &Path,
        project_path: Option<&str>,
        mut on_chunk: F,
        mut should_cancel: C,
    ) -> Result<VibeExecutionResult>
    where
        F: FnMut(VibeOutputChunk) -> Fut,
        Fut: Future<Output = Result<()>>,
        C: FnMut() -> CFut,
        CFut: Future<Output = Result<bool>>,
    {
        let effective_prompt =
            compose_vibe_prompt(prompt, job_id, &self.otter_api_base_url, project_path);
        let mut cmd = Command::new(&self.vibe_bin);
        cmd.arg("--prompt")
            .arg(&effective_prompt)
            .arg("--output")
            .arg("streaming")
            .arg("--workdir")
            .arg(workspace_path.as_os_str())
            .env("VIBE_HOME", isolated_vibe_home.as_os_str())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        self.apply_model_and_provider_env(&mut cmd);
        let mut child = cmd
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
        let mut cancel_poll = interval(Duration::from_millis(400));
        cancel_poll.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let mut stdout_buffer = String::new();
        let mut stderr_buffer = String::new();

        loop {
            tokio::select! {
                maybe_chunk = rx.recv() => {
                    let Some(chunk) = maybe_chunk else {
                        break;
                    };
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
                    if let Err(error) = on_chunk(chunk).await {
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        return Err(error);
                    }
                }
                _ = cancel_poll.tick() => {
                    if should_cancel().await? {
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        bail!("job cancelled");
                    }
                }
            }
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

        let raw_json = parse_streaming_json_lines(&stdout_buffer);
        let assistant_output = extract_latest_assistant_message(&raw_json).unwrap_or_default();

        Ok(VibeExecutionResult {
            assistant_output,
            raw_json,
            stderr: stderr_buffer,
            exit_code,
        })
    }
}

fn compose_vibe_prompt(
    user_prompt: &str,
    job_id: uuid::Uuid,
    otter_api_base_url: &str,
    project_path: Option<&str>,
) -> String {
    let project_path_instruction = match project_path.map(str::trim).filter(|value| !value.is_empty())
    {
        Some(path) => format!(
            "- Use this project path inside the workspace root: `{path}`. Do not write outside this path.\n"
        ),
        None => String::new(),
    };
    format!(
        r#"SYSTEM REQUIREMENTS (ALWAYS APPLY):
- Work in a project-specific subfolder under the current workspace. Never develop directly in workspace root, even documentation.
- Consequently, the first thing you need to do is create a project-specific subfolder under the current workspace.
- If a project path is provided, use it.
{project_path_instruction}
- ONLY build & run using DOCKER. NEVER run directly on host process as the main path.
- Always create a production-ready Dockerfile at the project root and use it as the primary run path.
- Build and run the generated app/service inside Docker (do not run directly on host process as the main path).
- Verify the container is running and expose the app on a reachable port from the current environment.
- When implementation is complete, start the app/service in background and verify it runs.
- Only send a real reachable HTTP/HTTPS URL as preview_url.
- Multiple Agents are running in parallel and will run specific dockers on specific ports. Make sure to use the correct port when setting the preview URL.
- At the end, print clear run instructions: docker build command, docker run command, docker stop/remove command, and where the project lives.
- Always include where to access the running app (URL/host port) in the final output.
- Use this exact Job ID when setting demo URL: `{job_id}`.
- ONLY AT THE END, AFTER EVERYTHING ELSE: after you determine the runnable app URL, call this API to set preview URL for this job:
  `curl -sS -X POST "{otter_api_base_url}/v1/jobs/{job_id}/preview-url" -H "Content-Type: application/json" -d '{{"preview_url":"http://<host>:<port>"}}'`
- Also set runtime launch commands for the UI:
  `curl -sS -X POST "{otter_api_base_url}/v1/jobs/{job_id}/runtime-launch" -H "Content-Type: application/json" -d '{{"start_command":"docker compose up -d","stop_command":"docker compose down","working_directory":"<relative/path/from_workspace>"}}'`
- You also MUST run the docker run command & check that you can ping the app on the port. Do not stop until the app is running.
- All apps you build must look modern and professional; the code should be clean and well-organized.

USER TASK:
{user_prompt}"#
    )
}

fn parse_streaming_json_lines(stdout: &str) -> serde_json::Value {
    let entries = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .unwrap_or_else(|_| serde_json::json!({ "type": "raw_line", "content": line }))
        })
        .collect::<Vec<_>>();
    serde_json::Value::Array(entries)
}

fn extract_latest_assistant_message(value: &serde_json::Value) -> Option<String> {
    let messages = value.as_array()?;
    messages
        .iter()
        .rev()
        .find_map(extract_assistant_content_from_entry)
}

fn extract_assistant_content_from_entry(entry: &serde_json::Value) -> Option<String> {
    let role = entry
        .get("role")
        .or_else(|| entry.pointer("/message/role"))
        .or_else(|| entry.pointer("/delta/role"))
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if role != "assistant" {
        return None;
    }
    value_to_text(
        entry
            .get("content")
            .or_else(|| entry.pointer("/message/content"))
            .or_else(|| entry.pointer("/delta/content"))?,
    )
}

fn value_to_text(value: &serde_json::Value) -> Option<String> {
    if let Some(content) = value.as_str() {
        return Some(content.to_string());
    }
    if let Some(array) = value.as_array() {
        let joined = array
            .iter()
            .filter_map(|item| {
                item.as_str().map(ToString::to_string).or_else(|| {
                    item.get("text")
                        .and_then(|v| v.as_str())
                        .map(ToString::to_string)
                })
            })
            .collect::<Vec<_>>()
            .join("");
        if !joined.is_empty() {
            return Some(joined);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{extract_latest_assistant_message, parse_streaming_json_lines};

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

    #[test]
    fn parses_streaming_json_lines_as_array() {
        let raw = r#"{"role":"assistant","content":"hello"}
{"role":"assistant","content":"world"}"#;
        let parsed = parse_streaming_json_lines(raw);
        assert_eq!(parsed.as_array().map(|v| v.len()), Some(2));
    }

    #[test]
    fn extracts_assistant_from_nested_message_shape() {
        let payload = serde_json::json!([
            {"type":"message","message":{"role":"assistant","content":"from-message"}},
            {"type":"delta","delta":{"role":"assistant","content":"from-delta"}}
        ]);
        let content = extract_latest_assistant_message(&payload);
        assert_eq!(content.as_deref(), Some("from-delta"));
    }
}
