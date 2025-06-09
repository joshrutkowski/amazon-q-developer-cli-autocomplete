// Import the submodules
use std::process::{ExitCode, Command};

use clap::{
    Args,
    Subcommand,
};
use eyre::Result;
use libproc::libproc::proc_pid;
use libproc::processes;
use serde::Serialize;
use tokio::io::{
    AsyncReadExt,
    AsyncWriteExt,
};
use tokio::net::UnixStream;

use crate::cli::OutputFormat;

// Arguments for agent command
#[derive(Debug, Args, PartialEq, Eq)]
pub struct AgentArgs {
    #[command(subcommand)]
    pub subcommand: Option<AgentSubcommand>,
}

// Define all possible enums for agent
#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum AgentSubcommand {
    List(ListArgs),
    Compare(CompareArgs),
}

// Define all possible arguments for list subcommand
#[derive(Debug, Args, PartialEq, Eq)]
pub struct ListArgs {
    /// Output format just says can be --f, -f, etc
    #[arg(long, short, value_enum, default_value_t)]
    pub format: OutputFormat,
}

#[derive(Debug, Args, PartialEq, Eq)]
pub struct CompareArgs {
    pub task_description: String,
    #[arg(long, value_delimiter = ',')]
    pub models: Vec<String>,
    #[arg(long, short, value_enum, default_value_t)]
    pub format: OutputFormat,
}

#[derive(Debug, Serialize)]
pub struct AgentInfo {
    pub pid: i32,
    pub profile: String,
    pub tokens_used: u64,
    pub context_window_percent: f32,
    pub running_time: u64,
    pub status: String,
}

// Lists all chat_cli instances metadata running in system
pub async fn list_agents() -> Result<ExitCode> {
    // Give processes time to create their sockets
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Ask all chat_cli instances for metadata using UDS
    let process_filter = processes::ProcFilter::All;
    let all_procs = processes::pids_by_type(process_filter)?;
    let mut agent_infos: Vec<_> = Vec::new();

    for curr_process in all_procs {
        let curr_pid = curr_process.try_into().unwrap();
        let curr_process_name = proc_pid::name(curr_pid).unwrap_or("Unknown process".to_string());
        if curr_process_name.contains("chat_cli") {
            let socket_path = format!("/tmp/qchat/{}", curr_pid);
            if !std::path::Path::new(&socket_path).exists() {
                continue;
            }
            match UnixStream::connect(&socket_path).await {
                Ok(mut stream) => {
                    // Send request
                    stream.write_all(b"GET_STATE").await?;
                    let mut buffer = [0u8; 1024];
                    // Read response metadata
                    let n = stream.read(&mut buffer).await?;
                    if n == 0 {
                        eprintln!("No response from server (EOF)");
                        continue;
                    }
                    let response_str = std::str::from_utf8(&buffer[..n]).unwrap_or("<invalid utf8>");

                    // Parse JSON response
                    match serde_json::from_str::<serde_json::Value>(&response_str) {
                        Ok(json) => {
                            // Extract values from JSON
                            let profile = json.get("profile").and_then(|v| v.as_str()).unwrap_or("unknown");

                            let tokens_used = json.get("tokens_used").and_then(|v| v.as_u64()).unwrap_or(0);

                            let context_window = json
                                .get("context_window")
                                .and_then(|v| v.as_f64())
                                .map(|v| v as f32)
                                .unwrap_or(0.0);

                            let total_time_sec = json
                                .get("duration_secs")
                                .and_then(|v| v.as_f64())
                                .map(|v| v as f32)
                                .unwrap_or(0.0);

                            let status = json.get("status").and_then(|v| v.as_str()).unwrap_or("unknown");

                            // Create AgentInfo
                            let info = AgentInfo {
                                pid: curr_pid,
                                profile: profile.to_string(),
                                tokens_used,
                                context_window_percent: context_window,
                                running_time: total_time_sec as u64,
                                status: status.to_string(),
                            };

                            agent_infos.push(info);
                        },
                        Err(e) => {
                            eprintln!("Failed to parse JSON from {}: {}", curr_pid, e);
                        },
                    }
                },
                Err(e) => {
                    eprintln!("Failed to connect to socket for PID {}: {}", curr_pid, e);
                },
            }
        }
    }

    // Print results
    use crossterm::style::{
        Attribute,
        Color,
    };
    use crossterm::{
        execute,
        style,
    };

    use crate::cli::chat::util::shared_writer::SharedWriter;

    let mut output = SharedWriter::stdout();
    execute!(
        output,
        style::SetForegroundColor(Color::Cyan),
        style::SetAttribute(Attribute::Bold),
        style::Print(format!("\nFound {} chat_cli instances\n", agent_infos.len())),
        style::SetAttribute(Attribute::Reset)
    )?;

    if !agent_infos.is_empty() {
        execute!(output, style::Print("▔".repeat(60)), style::Print("\n"))?;

        for info in agent_infos {
            execute!(
                output,
                style::SetForegroundColor(Color::Green),
                style::Print(format!("PID: {} ", info.pid)),
                style::SetForegroundColor(Color::Blue),
                style::Print(format!("Profile: {} ", info.profile)),
                style::SetForegroundColor(Color::Magenta),
                style::Print(format!("Tokens: {} ", info.tokens_used)),
                style::SetForegroundColor(Color::Yellow),
                style::Print(format!("Context Window: {:.1}% ", info.context_window_percent)),
                style::SetForegroundColor(Color::DarkCyan),
                style::Print(format!("Running: {}s ", info.running_time)),
                style::SetForegroundColor(Color::White),
                style::SetAttribute(Attribute::Bold),
                style::Print(format!("Status: {}", info.status)),
                style::SetAttribute(Attribute::Reset),
                style::SetForegroundColor(Color::Reset),
                style::Print("\n")
            )?;
        }

        execute!(output, style::Print("\n"))?;
    } else {
        execute!(
            output,
            style::SetForegroundColor(Color::DarkGrey),
            style::Print("No running instances found.\n\n"),
            style::SetForegroundColor(Color::Reset)
        )?;
    }

    Ok(ExitCode::SUCCESS)
}


/* Summon multiple models to do the same task and compare results amongst the different git worktrees */
pub async fn compare_agents(args: CompareArgs) -> Result<ExitCode> {
    // Check if we're in a git repo
    if !is_in_git_repo() {
        eprintln!("Error: Not in a git repository. Please run this command from a git repository.");
        return Ok(ExitCode::FAILURE);
    }

    // Create a new tmux session
    let main_pid: u32 = std::process::id();
    let session_name = format!("qagent-compare-{}", main_pid);
    let tmux_create = Command::new("tmux")
    .args(["new-session", "-d", "-s", &session_name])
    .output()?;
    
    if !tmux_create.status.success() {
        eprintln!("Failed to create tmux session: {}", String::from_utf8_lossy(&tmux_create.stderr));
        return Ok(ExitCode::FAILURE);
    }
    
    // Create a worktree for each model and start a chat session
    let home_dir = dirs::home_dir().expect("Could not find home directory");
    let downloads_dir = home_dir.join("Downloads").join(format!("qagent-compare-{}", main_pid));
    for (i, model) in args.models.iter().enumerate() {
        let worktree_dir = downloads_dir.join(format!("worktree-{}", model));
        let worktree_dir_str = worktree_dir.to_str().unwrap();
        // Create git worktree with detach flag (background task)
        let git_worktree = Command::new("git")
            .args(["worktree", "add", "--detach", &worktree_dir_str])
            .output()?;
        
        if !git_worktree.status.success() {
            eprintln!("Failed to create git worktree: {}", String::from_utf8_lossy(&git_worktree.stderr));
            continue;
        }
        
        // Create a new tmux window for this model with cwd set 
        let window_name = format!("{}", model);
        let window_index = i + 1; // Window indices start at 1 in tmux
        let tmux_window = Command::new("tmux")
            .args([
                "new-window", 
                "-d", 
                "-n", &window_name, 
                "-t", &format!("{}:{}", session_name, window_index),
                "-c", &worktree_dir_str
            ])
            .output()?;
        
        if !tmux_window.status.success() {
            eprintln!("Failed to create tmux window: {}", String::from_utf8_lossy(&tmux_window.stderr));
            continue;
        }
        
        // Start q chat with the specified model
        let trust_all_tools = "--trust-all-tools";
        let chat_command = format!(
            "q chat --model {} {} \"{}\"", 
            model, 
            trust_all_tools,
            args.task_description.replace("\"", "\\\"") // Escape quotes
        );
        
        // sends keys emulates typing in a window
        // We are routing prompt to appropriate model window
        let tmux_send = Command::new("tmux")
            .args([
                "send-keys", 
                "-t", &format!("{}:{}", session_name, window_index),
                &chat_command,
                "Enter"
            ])
            .output()?;
        
        if !tmux_send.status.success() {
            eprintln!("Failed to send command to tmux: {}", String::from_utf8_lossy(&tmux_send.stderr));
        }
    }
    
    // UI update
    println!("\nCreated tmux session '{}' with windows for models: {:?}", session_name, args.models);
    println!("Run the following command to attach to the session:");
    println!("  tmux attach-session -t {}\n", session_name);
    println!("Waiting for all models to complete their tasks...");
    println!("Once completed, you can compare the results in each worktree.");
    
    Ok(ExitCode::SUCCESS)
}


fn is_in_git_repo() -> bool {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--is-inside-work-tree")
        .output()
        .expect("Failed to execute git command");

    String::from_utf8_lossy(&output.stdout).trim() == "true"
}

impl AgentArgs {
    pub async fn execute(self) -> Result<ExitCode> {
        match self.subcommand {
            Some(AgentSubcommand::List(_)) => list_agents().await,
            Some(AgentSubcommand::Compare(args)) => compare_agents(args).await,
            None => list_agents().await, // Default behavior if no subcommand
        }
    }
}
