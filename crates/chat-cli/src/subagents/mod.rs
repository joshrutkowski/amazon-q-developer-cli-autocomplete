// Import the submodules
use libproc::{libproc::proc_pid, proc_pid::ProcType};
use libproc::processes;
use clap::{Args, Subcommand};
use eyre::Result;
use serde::Serialize;
use std::process::ExitCode;

use crate::cli::OutputFormat;

/* arguments for agent command */
#[derive(Debug, Args, PartialEq, Eq)]
pub struct AgentArgs {
    #[command(subcommand)]
    pub subcommand: Option<AgentSubcommand>,
}

/* Define all possible enums for agent */
#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum AgentSubcommand {
    List(ListArgs),
}

/* Define all possible arguments for list subcommand */
#[derive(Debug, Args, PartialEq, Eq)]
pub struct ListArgs {
    /// Output format just says can be --f, -f, etc
    #[arg(long, short, value_enum, default_value_t)]
    pub format: OutputFormat,
}

#[derive(Debug, Serialize)]
pub struct AgentInfo {
    pub name: String,
    pub description: String,
    pub status: String,
}

pub async fn list_agents() -> Result<ExitCode> {
    // Store all q processes in terminal_process list
    let process_filter = processes::ProcFilter::All;
    let all_procs = processes::pids_by_type(process_filter)?;
    let mut terminal_process = Vec::new();
    for curr_process in all_procs {
        let curr_pid = curr_process.try_into().unwrap();
        let curr_process_name = proc_pid::name(curr_pid).unwrap_or("Unknown process".to_string());
        if curr_process_name.contains("zsh (qterm)") {
            terminal_process.push(curr_process);
            if let Ok(task_info) = proc_pid::pidinfo::<libproc::task_info::TaskInfo>(curr_pid, 0) {
                // Total CPU time in seconds (user + system time)
                let total_time_sec = (task_info.pti_total_user + task_info.pti_total_system) / 1_000_000_000;
            }
        }
    }
    // Send 
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