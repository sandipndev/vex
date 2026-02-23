use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortsConfig {
    #[serde(default = "default_rpc_port")]
    pub rpc: u16,
    #[serde(default = "default_http_port")]
    pub http: u16,
}

fn default_rpc_port() -> u16 {
    7422
}
fn default_http_port() -> u16 {
    7423
}

impl Default for PortsConfig {
    fn default() -> Self {
        Self {
            rpc: default_rpc_port(),
            http: default_http_port(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefaultsConfig {
    #[serde(default = "default_runtime")]
    pub runtime: String,
    #[serde(default = "default_shell")]
    pub shell: String,
}

fn default_runtime() -> String {
    "tmux".to_string()
}

fn default_shell() -> String {
    "zsh".to_string()
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        Self {
            runtime: default_runtime(),
            shell: default_shell(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkstreamEntry {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectConfig {
    pub repo: String,
    pub path: String,
    #[serde(default)]
    pub workstreams: Vec<WorkstreamEntry>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VexConfig {
    #[serde(default)]
    pub ports: PortsConfig,
    #[serde(default)]
    pub defaults: DefaultsConfig,
    #[serde(default)]
    pub projects: BTreeMap<String, ProjectConfig>,
}

pub struct ProjectStore {
    path: PathBuf,
    config: VexConfig,
}

impl ProjectStore {
    pub fn load(path: PathBuf) -> Result<Self> {
        if path.exists() {
            let data = std::fs::read_to_string(&path)?;
            let config: VexConfig = serde_yaml::from_str(&data)?;
            Ok(Self { path, config })
        } else {
            Ok(Self {
                path,
                config: VexConfig::default(),
            })
        }
    }

    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let data = serde_yaml::to_string(&self.config)?;
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, data)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    pub fn config(&self) -> &VexConfig {
        &self.config
    }

    pub fn register(&mut self, name: String, repo: String, path: String) -> Result<ProjectConfig> {
        if self.config.projects.contains_key(&name) {
            anyhow::bail!("project '{}' is already registered", name);
        }

        let resolved = std::path::Path::new(&path)
            .canonicalize()
            .map_err(|e| anyhow::anyhow!("path '{}': {}", path, e))?;
        if !resolved.is_dir() {
            anyhow::bail!("path '{}' is not a directory", path);
        }

        let entry = ProjectConfig {
            repo,
            path: resolved.to_string_lossy().to_string(),
            workstreams: vec![],
        };
        self.config.projects.insert(name, entry.clone());
        self.save()?;
        Ok(entry)
    }

    pub fn unregister(&mut self, name: &str) -> bool {
        let removed = self.config.projects.remove(name).is_some();
        if removed {
            let _ = self.save();
        }
        removed
    }

    pub fn list(&self) -> &BTreeMap<String, ProjectConfig> {
        &self.config.projects
    }

    pub fn create_workstream(&mut self, project_name: &str, ws_name: String) -> Result<()> {
        let project = self
            .config
            .projects
            .get_mut(project_name)
            .ok_or_else(|| anyhow::anyhow!("project '{}' not found", project_name))?;
        if project.workstreams.iter().any(|w| w.name == ws_name) {
            anyhow::bail!(
                "workstream '{}' already exists in project '{}'",
                ws_name,
                project_name
            );
        }
        project.workstreams.push(WorkstreamEntry { name: ws_name });
        self.save()?;
        Ok(())
    }

    pub fn list_workstreams(&self, project_name: &str) -> Result<&[WorkstreamEntry]> {
        let project = self
            .config
            .projects
            .get(project_name)
            .ok_or_else(|| anyhow::anyhow!("project '{}' not found", project_name))?;
        Ok(&project.workstreams)
    }

    pub fn delete_workstream(&mut self, project_name: &str, ws_name: &str) -> Result<bool> {
        let project = self
            .config
            .projects
            .get_mut(project_name)
            .ok_or_else(|| anyhow::anyhow!("project '{}' not found", project_name))?;
        let before = project.workstreams.len();
        project.workstreams.retain(|w| w.name != ws_name);
        let removed = project.workstreams.len() < before;
        if removed {
            self.save()?;
        }
        Ok(removed)
    }
}
