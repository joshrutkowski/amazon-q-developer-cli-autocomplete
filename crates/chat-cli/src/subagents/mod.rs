// Import the submodules
use std::process::ExitCode;
use std::time::Duration;

use clap::{
    Args,
    Subcommand,
};
use eyre::Result;
use libproc::libproc::proc_pid;
use libproc::processes;
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use std::os::unix::fs::PermissionsExt;

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
}

// Define all possible arguments for list subcommand
#[derive(Debug, Args, PartialEq, Eq)]
pub struct ListArgs {
    /// Output format just says can be --f, -f, etc
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
            if let Ok(task_info) = proc_pid::pidinfo::<libproc::task_info::TaskInfo>(curr_pid, 0) {

                // Try to connect to the process's socket
                let socket_path = format!("/tmp/qchat/{}", curr_pid); 

                // Check if socket exists
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

                                let tokens_used = json
                                    .get("tokens_used")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0
                                );

                                let context_window = json
                                    .get("context_window")
                                    .and_then(|v| v.as_f64())
                                    .map(|v| v as f32)
                                    .unwrap_or(0.0
                                );

                                let total_time_sec = json
                                    .get("duration_secs")
                                    .and_then(|v| v.as_f64())
                                    .map(|v| v as f32)
                                    .unwrap_or(0.0
                                );
                                
                                
                                // Create AgentInfo
                                let info = AgentInfo {
                                    pid: curr_pid,
                                    profile: profile.to_string(),
                                    tokens_used,
                                    context_window_percent: context_window,
                                    running_time: total_time_sec as u64,
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
    }

    // Print results
    use crossterm::{style, execute};
    use crossterm::style::{Attribute, Color};
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
        execute!(
            output,
            style::Print("▔".repeat(60)),
            style::Print("\n")
        )?;
        
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
                style::Print(format!("Context: {:.1}% ", info.context_window_percent)),
                style::SetForegroundColor(Color::DarkCyan),
                style::Print(format!("Running: {}s", info.running_time)),
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

impl AgentArgs {
    pub async fn execute(self) -> Result<ExitCode> {
        match self.subcommand {
            Some(AgentSubcommand::List(_)) => list_agents().await,
            None => list_agents().await, // Default behavior if no subcommand
        }
    }
}
