use std::collections::HashMap;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use bollard::body_full;
use bollard::container::LogOutput;
use bollard::exec::{CreateExecOptions, StartExecOptions, StartExecResults};
use bollard::models::{ContainerCreateBody, HostConfig, PortBinding};
use bollard::query_parameters::{
    BuildImageOptionsBuilder, CreateContainerOptionsBuilder, InspectContainerOptions,
    ListContainersOptionsBuilder, LogsOptionsBuilder, RemoveContainerOptionsBuilder,
    RestartContainerOptionsBuilder, StartContainerOptions, StopContainerOptionsBuilder,
};
use bollard::{Docker, API_DEFAULT_VERSION};
use bytes::Bytes;
use futures_util::stream::{StreamExt, TryStreamExt};
use tar::Builder;
use tracing::debug;
use uuid::Uuid;

use crate::config::RuntimeConfig;
use crate::domain::{RuntimeContainerInfo, RuntimeContainerStatus, RuntimePortBinding};

const CWD_MARKER: &str = "__OTTER_CWD__:";

#[derive(Debug, Clone)]
pub struct RuntimeExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i64,
    pub cwd: String,
}

#[derive(Clone)]
pub struct DockerRuntimeManager {
    docker: Docker,
    config: RuntimeConfig,
}

impl DockerRuntimeManager {
    pub fn new(config: RuntimeConfig) -> Result<Self> {
        let docker = connect_docker(&config.docker_socket)?;
        Ok(Self { docker, config })
    }

    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    pub fn container_name_for(&self, workspace_id: Uuid) -> String {
        format!(
            "{}-{}",
            self.config.container_name_prefix,
            workspace_id.as_simple()
        )
    }

    pub fn image_tag_for(&self, workspace_id: Uuid) -> String {
        format!(
            "{}:{}",
            self.config.image_tag_prefix,
            workspace_id.as_simple()
        )
    }

    pub async fn ensure_workspace_container(
        &self,
        workspace_id: Uuid,
        workspace_root: &Path,
    ) -> Result<RuntimeContainerInfo> {
        let container_name = self.container_name_for(workspace_id);
        let image_tag = self.image_tag_for(workspace_id);
        self.build_workspace_image(workspace_root, &image_tag)
            .await?;

        if self
            .inspect_container_by_name(&container_name)
            .await?
            .is_some()
        {
            let remove_opts = RemoveContainerOptionsBuilder::default()
                .force(true)
                .v(true)
                .build();
            self.docker
                .remove_container(&container_name, Some(remove_opts))
                .await
                .with_context(|| format!("failed to remove previous container {container_name}"))?;
        }

        let create_opts = CreateContainerOptionsBuilder::default()
            .name(&container_name)
            .build();
        self.docker
            .create_container(
                Some(create_opts),
                ContainerCreateBody {
                    image: Some(image_tag.clone()),
                    tty: Some(true),
                    host_config: Some(HostConfig {
                        publish_all_ports: Some(true),
                        network_mode: Some(self.config.network_name.clone()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .with_context(|| format!("failed to create runtime container {container_name}"))?;

        self.docker
            .start_container(&container_name, None::<StartContainerOptions>)
            .await
            .with_context(|| format!("failed to start runtime container {container_name}"))?;

        self.runtime_status(workspace_id).await
    }

    pub async fn runtime_status(&self, workspace_id: Uuid) -> Result<RuntimeContainerInfo> {
        let container_name = self.container_name_for(workspace_id);
        let image_tag = self.image_tag_for(workspace_id);
        let inspect = self.inspect_container_by_name(&container_name).await?;

        let Some(container) = inspect else {
            return Ok(RuntimeContainerInfo {
                workspace_id,
                container_name,
                image_tag,
                container_id: None,
                status: RuntimeContainerStatus::Missing,
                ports: vec![],
                preferred_url: None,
            });
        };

        let status = container
            .state
            .and_then(|state| state.status)
            .map(|status| match status {
                bollard::models::ContainerStateStatusEnum::RUNNING => {
                    RuntimeContainerStatus::Running
                }
                _ => RuntimeContainerStatus::Stopped,
            })
            .unwrap_or(RuntimeContainerStatus::Stopped);

        let ports = extract_port_bindings(
            container
                .network_settings
                .and_then(|settings| settings.ports)
                .unwrap_or_default(),
        );
        let preferred_url = ports
            .iter()
            .find(|binding| binding.host_port > 0)
            .map(|binding| {
                format!(
                    "{}:{}",
                    self.config.default_host.trim_end_matches('/'),
                    binding.host_port
                )
            });

        Ok(RuntimeContainerInfo {
            workspace_id,
            container_name,
            image_tag,
            container_id: container.id,
            status,
            ports,
            preferred_url,
        })
    }

    pub async fn start_workspace_container(
        &self,
        workspace_id: Uuid,
    ) -> Result<RuntimeContainerInfo> {
        let container_name = self.container_name_for(workspace_id);
        self.docker
            .start_container(&container_name, None::<StartContainerOptions>)
            .await
            .with_context(|| format!("failed to start container {container_name}"))?;
        self.runtime_status(workspace_id).await
    }

    pub async fn stop_workspace_container(
        &self,
        workspace_id: Uuid,
    ) -> Result<RuntimeContainerInfo> {
        let container_name = self.container_name_for(workspace_id);
        let stop_opts = StopContainerOptionsBuilder::default().t(10).build();
        self.docker
            .stop_container(&container_name, Some(stop_opts))
            .await
            .with_context(|| format!("failed to stop container {container_name}"))?;
        self.runtime_status(workspace_id).await
    }

    pub async fn restart_workspace_container(
        &self,
        workspace_id: Uuid,
    ) -> Result<RuntimeContainerInfo> {
        let container_name = self.container_name_for(workspace_id);
        let restart_opts = RestartContainerOptionsBuilder::default().t(10).build();
        self.docker
            .restart_container(&container_name, Some(restart_opts))
            .await
            .with_context(|| format!("failed to restart container {container_name}"))?;
        self.runtime_status(workspace_id).await
    }

    pub async fn logs_for_workspace(&self, workspace_id: Uuid, tail: usize) -> Result<String> {
        let container_name = self.container_name_for(workspace_id);
        let logs_opts = LogsOptionsBuilder::default()
            .stdout(true)
            .stderr(true)
            .follow(false)
            .tail(&tail.to_string())
            .build();
        let mut stream = self.docker.logs(&container_name, Some(logs_opts));

        let mut chunks = Vec::new();
        while let Some(item) = stream.next().await {
            let item =
                item.with_context(|| format!("failed reading logs from {container_name}"))?;
            let text = match item {
                LogOutput::StdOut { message } => String::from_utf8_lossy(&message).to_string(),
                LogOutput::StdErr { message } => String::from_utf8_lossy(&message).to_string(),
                LogOutput::StdIn { message } => String::from_utf8_lossy(&message).to_string(),
                LogOutput::Console { message } => String::from_utf8_lossy(&message).to_string(),
            };
            chunks.push(text);
        }
        Ok(chunks.join(""))
    }

    pub async fn exec_in_workspace(
        &self,
        workspace_id: Uuid,
        command: &str,
        cwd: Option<&str>,
    ) -> Result<RuntimeExecResult> {
        let container_name = self.container_name_for(workspace_id);
        let wrapped_command = format!(
            "({command}); __otter_exit=$?; printf '\\n{CWD_MARKER}%s\\n' \"$PWD\"; exit $__otter_exit"
        );
        let exec = self
            .docker
            .create_exec(
                &container_name,
                CreateExecOptions {
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    cmd: Some(vec!["bash", "-lc", wrapped_command.as_str()]),
                    working_dir: cwd,
                    ..Default::default()
                },
            )
            .await
            .with_context(|| format!("failed to create exec in container {container_name}"))?;

        let mut stdout = String::new();
        let mut stderr = String::new();
        if let StartExecResults::Attached { mut output, .. } = self
            .docker
            .start_exec(&exec.id, None::<StartExecOptions>)
            .await
            .with_context(|| format!("failed to start exec for container {container_name}"))?
        {
            while let Some(chunk) = output.next().await {
                let chunk = chunk.context("failed reading exec stream")?;
                match chunk {
                    LogOutput::StdOut { message } | LogOutput::Console { message } => {
                        stdout.push_str(&String::from_utf8_lossy(&message));
                    }
                    LogOutput::StdErr { message } => {
                        stderr.push_str(&String::from_utf8_lossy(&message));
                    }
                    LogOutput::StdIn { .. } => {}
                }
            }
        }

        let inspect = self
            .docker
            .inspect_exec(&exec.id)
            .await
            .context("failed to inspect exec")?;
        let exit_code = inspect.exit_code.unwrap_or(-1);
        let (stdout, resolved_cwd) = split_stdout_and_cwd(&stdout, cwd.unwrap_or("/workspace"));

        Ok(RuntimeExecResult {
            stdout,
            stderr,
            exit_code,
            cwd: resolved_cwd,
        })
    }

    async fn build_workspace_image(&self, workspace_root: &Path, tag: &str) -> Result<()> {
        let dockerfile = workspace_root.join("Dockerfile");
        if !dockerfile.exists() {
            return Err(anyhow!(
                "workspace {} does not include a Dockerfile for runtime build",
                workspace_root.display()
            ));
        }

        let context = tar_workspace(workspace_root)?;
        let options = BuildImageOptionsBuilder::default()
            .dockerfile("Dockerfile")
            .t(tag)
            .rm(true)
            .build();

        let mut stream = self
            .docker
            .build_image(options, None, Some(body_full(Bytes::from(context))));
        while let Some(chunk) = stream
            .try_next()
            .await
            .context("failed docker image build stream")?
        {
            if let Some(error_detail) = chunk.error_detail {
                let msg = error_detail
                    .message
                    .unwrap_or_else(|| "unknown build error".to_string());
                return Err(anyhow!("docker image build failed: {msg}"));
            }
            if let Some(message) = chunk.stream {
                debug!(image = %tag, output = %message.trim_end(), "docker build output");
            }
        }
        Ok(())
    }

    async fn inspect_container_by_name(
        &self,
        container_name: &str,
    ) -> Result<Option<bollard::models::ContainerInspectResponse>> {
        match self
            .docker
            .inspect_container(container_name, None::<InspectContainerOptions>)
            .await
        {
            Ok(response) => Ok(Some(response)),
            Err(bollard::errors::Error::DockerResponseServerError { status_code, .. })
                if status_code == 404 =>
            {
                Ok(None)
            }
            Err(error) => Err(error.into()),
        }
    }

    pub async fn prune_stale_containers(&self) -> Result<()> {
        let list_opts = ListContainersOptionsBuilder::default()
            .all(true)
            .build();
        let containers = self.docker.list_containers(Some(list_opts)).await?;
        for container in containers {
            let Some(names) = container.names else {
                continue;
            };
            let should_remove = names.iter().any(|name| {
                name.trim_start_matches('/')
                    .starts_with(&self.config.container_name_prefix)
            });
            if !should_remove {
                continue;
            }
            if let Some(id) = container.id {
                let remove_opts = RemoveContainerOptionsBuilder::default()
                    .force(true)
                    .v(true)
                    .build();
                let _ = self
                    .docker
                    .remove_container(&id, Some(remove_opts))
                    .await;
            }
        }
        Ok(())
    }
}

fn connect_docker(socket: &str) -> Result<Docker> {
    if let Some(path) = socket.strip_prefix("unix://") {
        Docker::connect_with_unix(path, 120, API_DEFAULT_VERSION)
            .with_context(|| format!("failed to connect docker on unix socket {socket}"))
    } else {
        Docker::connect_with_local_defaults()
            .context("failed to connect docker with local defaults")
    }
}

fn tar_workspace(workspace_root: &Path) -> Result<Vec<u8>> {
    let mut archive = Vec::new();
    let mut builder = Builder::new(&mut archive);
    builder.append_dir_all(".", workspace_root)?;
    builder.finish()?;
    drop(builder);
    Ok(archive)
}

fn extract_port_bindings(
    raw_ports: HashMap<String, Option<Vec<PortBinding>>>,
) -> Vec<RuntimePortBinding> {
    let mut parsed = Vec::new();
    for (container_port_proto, host_bindings) in raw_ports {
        let Some(container_port) = container_port_proto.split('/').next() else {
            continue;
        };
        let Ok(container_port_number) = container_port.parse::<u16>() else {
            continue;
        };
        let Some(host_bindings) = host_bindings else {
            continue;
        };
        for binding in host_bindings {
            let host_ip = binding.host_ip.unwrap_or_else(|| "0.0.0.0".to_string());
            let host_port = binding
                .host_port
                .and_then(|value| value.parse::<u16>().ok())
                .unwrap_or_default();
            parsed.push(RuntimePortBinding {
                container_port: container_port_number,
                host_ip,
                host_port,
            });
        }
    }
    parsed.sort_by_key(|binding| (binding.container_port, binding.host_port));
    parsed
}

pub fn split_stdout_and_cwd(stdout: &str, fallback_cwd: &str) -> (String, String) {
    let mut cleaned_lines = Vec::new();
    let mut resolved_cwd = fallback_cwd.to_string();
    for raw_line in stdout.lines() {
        if let Some((_, cwd)) = raw_line.split_once(CWD_MARKER) {
            let candidate = cwd.trim();
            if !candidate.is_empty() {
                resolved_cwd = candidate.to_string();
            }
            continue;
        }
        cleaned_lines.push(raw_line);
    }
    let mut cleaned_stdout = cleaned_lines.join("\n");
    if stdout.ends_with('\n') && !cleaned_stdout.is_empty() {
        cleaned_stdout.push('\n');
    }
    (cleaned_stdout, resolved_cwd)
}

#[cfg(test)]
mod tests {
    use super::{extract_port_bindings, split_stdout_and_cwd};
    use bollard::models::PortBinding;
    use std::collections::HashMap;

    #[test]
    fn split_stdout_extracts_cwd() {
        let raw = "line one\n__OTTER_CWD__:/workspace/demo\n";
        let (stdout, cwd) = split_stdout_and_cwd(raw, "/workspace");
        assert_eq!(stdout, "line one\n");
        assert_eq!(cwd, "/workspace/demo".to_string());
    }

    #[test]
    fn extract_ports_parses_bindings() {
        let mut raw = HashMap::new();
        raw.insert(
            "3000/tcp".to_string(),
            Some(vec![PortBinding {
                host_ip: Some("0.0.0.0".to_string()),
                host_port: Some("49162".to_string()),
            }]),
        );
        let parsed = extract_port_bindings(raw);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].container_port, 3000);
        assert_eq!(parsed[0].host_port, 49162);
    }
}
