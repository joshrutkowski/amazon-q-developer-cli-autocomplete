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
    pub async fn load_config(output: &mut impl Write) -> eyre::Result<Self> {
        let mut cwd = std::env::current_dir()?;
        cwd.push(".amazonq/mcp.json");
        let expanded_path = shellexpand::tilde("~/.aws/amazonq/mcp.json");
        let global_path = PathBuf::from(expanded_path.as_ref() as &str);
        let global_buf = tokio::fs::read(global_path).await.ok();
        let local_buf = tokio::fs::read(cwd).await.ok();
        let conf = match (global_buf, local_buf) {
            (Some(global_buf), Some(local_buf)) => {
                let mut global_conf = Self::from_slice(&global_buf, output, "global")?;
                let local_conf = Self::from_slice(&local_buf, output, "local")?;
                for (server_name, config) in local_conf.mcp_servers {
                    if global_conf.mcp_servers.insert(server_name.clone(), config).is_some() {
                        queue!(
                            output,
                            style::SetForegroundColor(style::Color::Yellow),
                            style::Print("WARNING: "),
                            style::ResetColor,
                            style::Print("MCP config conflict for "),
                            style::SetForegroundColor(style::Color::Green),
                            style::Print(server_name),
                            style::ResetColor,
                            style::Print(". Using workspace version.\n")
                        )?;
                    }
                }
                global_conf
            },
            (None, Some(local_buf)) => Self::from_slice(&local_buf, output, "local")?,
            (Some(global_buf), None) => Self::from_slice(&global_buf, output, "global")?,
            _ => Default::default(),
        };
        output.flush()?;
        Ok(conf)
    }

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
            let default_agent = Agent::default();
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
                Ok(agent) => agent,
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
        let agent = serde_json::from_str::<Agent>(INPUT_1).expect("Deserializtion failed");
        assert!(agent.mcp_servers.mcp_servers.contains_key("fetch"));
        assert!(agent.mcp_servers.mcp_servers.contains_key("git"));
    }
}
