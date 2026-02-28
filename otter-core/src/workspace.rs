use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use tokio::fs;
use uuid::Uuid;

pub struct WorkspaceManager {
    allowed_roots: Vec<PathBuf>,
    vibe_base_home: PathBuf,
}

impl WorkspaceManager {
    pub fn new(allowed_roots: Vec<PathBuf>, vibe_base_home: PathBuf) -> Self {
        Self {
            allowed_roots,
            vibe_base_home,
        }
    }

    pub fn validate_workspace_path(&self, workspace_path: &Path) -> Result<PathBuf> {
        let canonical = workspace_path.canonicalize().with_context(|| {
            format!(
                "workspace path does not exist: {}",
                workspace_path.display()
            )
        })?;
        if !canonical.is_dir() {
            bail!("workspace path is not a directory: {}", canonical.display());
        }
        if self.allowed_roots.is_empty() {
            return Ok(canonical);
        }
        let under_allowed_root = self
            .allowed_roots
            .iter()
            .filter_map(|root| root.canonicalize().ok())
            .any(|root| canonical.starts_with(root));
        if !under_allowed_root {
            bail!("workspace path is not under an allowed root");
        }
        Ok(canonical)
    }

    pub fn isolated_vibe_home_for_workspace(&self, workspace_id: Uuid) -> PathBuf {
        self.vibe_base_home.join(workspace_id.to_string())
    }

    pub async fn prepare_isolated_vibe_home(
        &self,
        workspace_id: Uuid,
        workspace_path: &Path,
    ) -> Result<PathBuf> {
        let isolated_home = self.isolated_vibe_home_for_workspace(workspace_id);
        fs::create_dir_all(&isolated_home).await?;
        fs::create_dir_all(isolated_home.join(".vibe")).await?;

        let trust_file = isolated_home.join("trusted_folders.toml");
        let trusted_folder_value = workspace_path.display().to_string();
        let trust_content = format!("trusted = [\"{trusted_folder_value}\"]\nuntrusted = []\n");
        fs::write(&trust_file, trust_content).await?;

        Ok(isolated_home)
    }
}

#[cfg(test)]
mod tests {
    use super::WorkspaceManager;

    #[test]
    fn isolated_home_uses_workspace_id() {
        let manager = WorkspaceManager::new(vec![], "/tmp/otter-vibe".into());
        let workspace_id =
            uuid::Uuid::parse_str("5a5ff4f9-b10e-4e89-b61f-9a1e27a5948f").expect("valid uuid");
        let path = manager.isolated_vibe_home_for_workspace(workspace_id);
        assert!(path.ends_with("5a5ff4f9-b10e-4e89-b61f-9a1e27a5948f"));
    }
}
