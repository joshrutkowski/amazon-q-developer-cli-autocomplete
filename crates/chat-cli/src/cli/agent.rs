use std::collections::{
    HashMap,
    HashSet,
};
use std::ffi::OsStr;
use std::io::{
    self,
    Write,
};
use std::path::{
    Path,
    PathBuf,
};

use crossterm::{
    queue,
    style,
};
use regex::Regex;
use serde::{
    Deserialize,
    Serialize,
};
use tokio::fs::ReadDir;
use tracing::error;

use super::chat::tools::custom_tool::CustomToolConfig;
use crate::platform::Context;
use crate::util::directories;

// This is to mirror claude's config set up
#[derive(Clone, Serialize, Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase", transparent)]
pub struct McpServerConfig {
    pub mcp_servers: HashMap<String, CustomToolConfig>,
}

impl McpServerConfig {
    pub async fn load_from_file(ctx: &Context, path: impl AsRef<Path>) -> eyre::Result<Self> {
        let contents = ctx.fs().read_to_string(path.as_ref()).await?;
        Ok(serde_json::from_str(&contents)?)
    }

    pub async fn save_to_file(&self, ctx: &Context, path: impl AsRef<Path>) -> eyre::Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        ctx.fs().write(path.as_ref(), json).await?;
        Ok(())
    }

    fn from_slice(slice: &[u8], output: &mut impl Write, location: &str) -> eyre::Result<McpServerConfig> {
        match serde_json::from_slice::<Self>(slice) {
            Ok(config) => Ok(config),
            Err(e) => {
                queue!(
                    output,
                    style::SetForegroundColor(style::Color::Yellow),
                    style::Print("WARNING: "),
                    style::ResetColor,
                    style::Print(format!("Error reading {location} mcp config: {e}\n")),
                    style::Print("Please check to make sure config is correct. Discarding.\n"),
                )?;
                Ok(McpServerConfig::default())
            },
        }
    }
}

/// Externally this is known as "Persona"
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Agent {
    /// Agent or persona names are derived from the file name. Thus they are skipped for
    /// serializing
    #[serde(skip)]
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub mcp_servers: McpServerConfig,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub allowed_tools: HashSet<String>,
    #[serde(default)]
    pub included_files: Vec<String>,
    #[serde(default)]
    pub create_hooks: serde_json::Value,
    #[serde(default)]
    pub prompt_hooks: serde_json::Value,
    #[serde(default)]
    pub tools_settings: HashMap<String, serde_json::Value>,
    #[serde(skip)]
    pub path: Option<PathBuf>,
}

impl Default for Agent {
    fn default() -> Self {
        Self {
            name: "default".to_string(),
            description: Some("Default persona".to_string()),
            prompt: Default::default(),
            mcp_servers: Default::default(),
            tools: vec!["*".to_string()],
            allowed_tools: {
                let mut set = HashSet::<String>::new();
                set.insert("*".to_string());
                set
            },
            included_files: vec!["AmazonQ.md", "README.md", ".amazonq/rules/**/*.md"]
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>(),
            create_hooks: Default::default(),
            prompt_hooks: Default::default(),
            tools_settings: Default::default(),
            path: None,
        }
    }
}

pub enum PermissionEvalResult {
    Allow,
    Ask,
    Deny,
}

impl Agent {
    pub fn eval_perm(&self, candidate: &impl PermissionCandidate) -> PermissionEvalResult {
        if self.allowed_tools.len() == 1 && self.allowed_tools.contains("*") {
            return PermissionEvalResult::Allow;
        }

        candidate.eval(self)
    }
}

#[derive(Clone, Default, Debug)]
pub struct AgentCollection {
    pub agents: HashMap<String, Agent>,
    pub active_idx: String,
}

impl AgentCollection {
    pub fn get_active(&self) -> Option<&Agent> {
        self.agents.get(&self.active_idx)
    }

    pub fn get_active_mut(&mut self) -> Option<&mut Agent> {
        self.agents.get_mut(&self.active_idx)
    }

    pub fn switch(&mut self, name: &str) -> eyre::Result<&Agent> {
        self.agents
            .get(name)
            .ok_or(eyre::eyre!("No agent with name {name} found"))
    }

    pub async fn publish(&self, subscriber: &impl AgentSubscriber) -> eyre::Result<()> {
        if let Some(agent) = self.get_active() {
            subscriber.receive(agent.clone()).await;
            return Ok(());
        }

        eyre::bail!("No active agent. Agent not published");
    }

    pub async fn reload_personas(&mut self, ctx: &Context, output: &mut impl Write) -> eyre::Result<()> {
        let persona_name = self.get_active().map(|a| a.name.as_str());
        let mut new_self = Self::load(ctx, persona_name, output).await;
        std::mem::swap(self, &mut new_self);
        Ok(())
    }

    pub fn list_personas(&self) -> eyre::Result<Vec<String>> {
        Ok(self.agents.keys().cloned().collect::<Vec<_>>())
    }

    pub async fn save_persona(
        &mut self,
        ctx: &Context,
        subcribers: Vec<&dyn AgentSubscriber>,
    ) -> eyre::Result<PathBuf> {
        let agent = self.get_active_mut().ok_or(eyre::eyre!("No active persona selected"))?;
        for sub in subcribers {
            sub.upload(agent).await;
        }

        let path = agent
            .path
            .as_ref()
            .ok_or(eyre::eyre!("Persona path associated not found"))?;
        let contents =
            serde_json::to_string_pretty(agent).map_err(|e| eyre::eyre!("Error serializing persona: {:?}", e))?;
        ctx.fs()
            .write(path, &contents)
            .await
            .map_err(|e| eyre::eyre!("Error writing persona to file: {:?}", e))?;

        Ok(path.clone())
    }

    /// Migrated from [create_profile] from context.rs, which was creating profiles under the
    /// global directory. We shall preserve this implicit behavior for now until further notice.
    pub async fn create_persona(&self, ctx: &Context, name: &str) -> eyre::Result<()> {
        validate_persona_name(name)?;

        let persona_path = directories::chat_global_persona_path(ctx)?.join(format!("{name}.json"));
        if persona_path.exists() {
            return Err(eyre::eyre!("Profile '{}' already exists", name));
        }

        let config = Agent {
            path: persona_path.parent().map(PathBuf::from),
            ..Default::default()
        };
        let contents = serde_json::to_string_pretty(&config)
            .map_err(|e| eyre::eyre!("Failed to serialize profile configuration: {}", e))?;

        if let Some(parent) = persona_path.parent() {
            ctx.fs().create_dir_all(parent).await?;
        }
        ctx.fs().write(&persona_path, contents).await?;

        Ok(())
    }

    pub async fn delete_persona(&self, ctx: &Context, name: &str) -> eyre::Result<()> {
        if name == self.active_idx.as_str() {
            eyre::bail!("Cannot delete the active persona. Switch to another persona first");
        }

        let to_delete = self
            .agents
            .get(name)
            .ok_or(eyre::eyre!("Persona '{name}' does not exist"))?;
        match to_delete.path.as_ref() {
            Some(path) if path.exists() => {
                ctx.fs().remove_dir_all(path).await?;
            },
            _ => eyre::bail!("Persona {name} does not have an associated path"),
        }

        Ok(())
    }

    pub async fn load(ctx: &Context, persona_name: Option<&str>, output: &mut impl Write) -> Self {
        let mut local_agents = 'local: {
            let Ok(path) = directories::chat_local_persona_dir() else {
                break 'local Vec::<Agent>::new();
            };
            let Ok(files) = tokio::fs::read_dir(path).await else {
                break 'local Vec::<Agent>::new();
            };
            load_agents_from_entries(files).await
        };

        let mut global_agents = 'global: {
            let Ok(path) = directories::chat_global_persona_path(ctx) else {
                break 'global Vec::<Agent>::new();
            };
            let files = match tokio::fs::read_dir(&path).await {
                Ok(files) => files,
                Err(e) => {
                    if matches!(e.kind(), io::ErrorKind::NotFound) {
                        if let Err(e) = ctx.fs().create_dir_all(&path).await {
                            error!("Error creating global persona dir: {:?}", e);
                        }
                    }
                    break 'global Vec::<Agent>::new();
                },
            };
            load_agents_from_entries(files).await
        };

        let local_names = local_agents.iter().map(|a| a.name.as_str()).collect::<HashSet<&str>>();
        global_agents.retain(|a| {
            // If there is a naming conflict for agents, we would retain the local instance
            let name = a.name.as_str();
            if local_names.contains(name) {
                let _ = queue!(
                    output,
                    style::SetForegroundColor(style::Color::Yellow),
                    style::Print("WARNING: "),
                    style::ResetColor,
                    style::Print("Persona conflict for "),
                    style::SetForegroundColor(style::Color::Green),
                    style::Print(name),
                    style::ResetColor,
                    style::Print(". Using workspace version.\n")
                );
                false
            } else {
                true
            }
        });

        let _ = output.flush();
        local_agents.append(&mut global_agents);

        // Ensure that we always have a default persona under the global directory
        if !local_agents.iter().any(|a| a.name == "default") {
            let mut default_agent = Agent::default();
            default_agent.path = directories::chat_global_persona_path(ctx).ok();
            match serde_json::to_string_pretty(&default_agent) {
                Ok(content) => {
                    if let Ok(path) = directories::chat_global_persona_path(ctx) {
                        let default_path = path.join("default.json");
                        if let Err(e) = tokio::fs::write(default_path, &content).await {
                            error!("Error writing default persona to file: {:?}", e);
                        }
                    };
                },
                Err(e) => {
                    error!("Error serializing default persona: {:?}", e);
                },
            }

            local_agents.push(default_agent);
        }

        Self {
            agents: local_agents
                .into_iter()
                .map(|a| (a.name.clone(), a))
                .collect::<HashMap<_, _>>(),
            active_idx: persona_name.unwrap_or("default").to_string(),
        }
    }
}

async fn load_agents_from_entries(mut files: ReadDir) -> Vec<Agent> {
    let mut res = Vec::<Agent>::new();
    while let Ok(Some(file)) = files.next_entry().await {
        let file_path = &file.path();
        if file_path
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|s| s == "json")
        {
            let content = match tokio::fs::read(file_path).await {
                Ok(content) => content,
                Err(e) => {
                    let file_path = file_path.to_string_lossy();
                    tracing::error!("Error reading persona file {file_path}: {:?}", e);
                    continue;
                },
            };
            let mut agent = match serde_json::from_slice::<Agent>(&content) {
                Ok(mut agent) => {
                    agent.path = Some(file_path.clone());
                    agent
                },
                Err(e) => {
                    let file_path = file_path.to_string_lossy();
                    tracing::error!("Error deserializing persona file {file_path}: {:?}", e);
                    continue;
                },
            };
            if let Some(name) = Path::new(&file.file_name()).file_stem() {
                agent.name = name.to_string_lossy().to_string();
                res.push(agent);
            } else {
                let file_path = file_path.to_string_lossy();
                tracing::error!("Unable to determine persona name from config file at {file_path}, skipping");
                continue;
            }
        }
    }
    res
}

fn validate_persona_name(name: &str) -> eyre::Result<()> {
    // Check if name is empty
    if name.is_empty() {
        eyre::bail!("Persona name cannot be empty");
    }

    // Check if name contains only allowed characters and starts with an alphanumeric character
    let re = Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9_-]*$")?;
    if !re.is_match(name) {
        eyre::bail!(
            "Persona name must start with an alphanumeric character and can only contain alphanumeric characters, hyphens, and underscores"
        );
    }

    Ok(())
}

/// To be implemented by tools
/// The intended workflow here is to utilize to the visitor pattern
/// - [Agent] accepts a PermissionCandidate
/// - it then passes a reference of itself to [PermissionCandidate::eval]
/// - it is then expected to look through the permissions hashmap to conclude
pub trait PermissionCandidate {
    fn eval(&self, agent: &Agent) -> PermissionEvalResult;
}

/// To be implemented by constructs that depend on agent configurations
#[async_trait::async_trait]
pub trait AgentSubscriber {
    async fn receive(&self, agent: Agent);
    async fn upload(&self, agent: &mut Agent);
}

#[cfg(test)]
mod tests {
    use super::*;

    const INPUT: &str = r#"
            {
              "description": "My developer agent is used for small development tasks like solving open issues.",
              "prompt": "You are a principal developer who uses multiple agents to accomplish difficult engineering tasks",
              "mcpServers": {
                "fetch": { "command": "fetch3.1", "args": [] },
                "git": { "command": "git-mcp", "args": [] }
              },
              "tools": [                                    
                "@git",                                     
                "@git.git_status",                         
                "fs_read"
              ],
              "allowedTools": [                           
                "fs_read",                               
                "@fetch",
                "@git/git_status"
              ],
              "includedFiles": [                        
                "~/my-genai-prompts/unittest.md"
              ],
              "createHooks": [                         
                "pwd && tree"
              ],
              "promptHooks": [                        
                "git status"
              ],
              "toolsSettings": {                     
                "fs_write": { "allowedPaths": ["~/**"] },
                "@git/git_status": { "git_user": "$GIT_USER" }
              }
            }
        "#;

    #[test]
    fn test_deser() {
        let agent = serde_json::from_str::<Agent>(INPUT).expect("Deserializtion failed");
        assert!(agent.mcp_servers.mcp_servers.contains_key("fetch"));
        assert!(agent.mcp_servers.mcp_servers.contains_key("git"));
    }
}
