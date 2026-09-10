use anyhow::Result;
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use std::{
    io::{self, Write},
    path::PathBuf,
};
use tasky_store::Store;

#[derive(Parser)]
#[command(version, about = "Local task graphs for agents")]
struct Cli {
    /// Directory containing graph.json and its writer lock
    #[arg(long, global = true, default_value = ".tasky")]
    store: PathBuf,
    /// Emit machine-readable JSON on stdout (including errors on stderr)
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create an empty graph; refuses to overwrite existing data
    Init,
    /// Add a pending task with a caller-chosen unique ID
    Add { id: String, title: String },
    /// Show all tasks
    List,
    /// Show a single task
    Show { id: String },
    /// Show pending tasks whose prerequisites are all done
    Ready,
    /// Require DEPENDENCY to finish before ID can be claimed
    Depend { id: String, dependency: String },
    /// Remove a prerequisite from a pending task
    Undepend { id: String, dependency: String },
    /// Atomically claim a ready task for an agent
    Claim {
        id: String,
        #[arg(long)]
        agent: String,
    },
    /// Mark a task done (requires the claiming agent)
    Complete {
        id: String,
        #[arg(long)]
        agent: String,
    },
    /// Mark a task failed (requires the claiming agent)
    Fail {
        id: String,
        #[arg(long)]
        agent: String,
        #[arg(long)]
        reason: String,
    },
    /// Return a failed task to pending
    Retry { id: String },
    /// Export the complete versioned graph snapshot
    Snapshot,
    /// Check graph invariants without modifying it
    Validate,
}

fn execute(cli: &Cli) -> Result<Value> {
    let store = Store::new(&cli.store);
    let graph = match &cli.command {
        Command::Init => store.init()?,
        Command::List => return Ok(json!(store.load()?.tasks().collect::<Vec<_>>())),
        Command::Show { id } => return Ok(json!(store.load()?.task(id)?)),
        Command::Ready => return Ok(json!(store.load()?.ready().collect::<Vec<_>>())),
        Command::Snapshot => store.load()?,
        Command::Validate => {
            store.load()?;
            return Ok(json!({"valid": true}));
        }
        command => store.update(|g| {
            match command {
                Command::Add { id, title } => g.add(id.clone(), title.clone())?,
                Command::Depend { id, dependency } => g.depend(id, dependency)?,
                Command::Undepend { id, dependency } => g.undepend(id, dependency)?,
                Command::Claim { id, agent } => g.claim(id, agent.clone())?,
                Command::Complete { id, agent } => g.finish(id, agent, None)?,
                Command::Fail { id, agent, reason } => g.finish(id, agent, Some(reason.clone()))?,
                Command::Retry { id } => g.retry(id)?,
                _ => unreachable!("queries handled above"),
            }
            Ok(())
        })?,
    };
    Ok(json!(graph))
}

fn main() {
    let cli = Cli::parse();
    let result = execute(&cli).and_then(|value| {
        let mut stdout = io::stdout().lock();
        if cli.json {
            serde_json::to_writer(&mut stdout, &value)?;
        } else {
            serde_json::to_writer_pretty(&mut stdout, &value)?;
        }
        writeln!(stdout)?;
        Ok(())
    });
    if let Err(error) = result {
        if cli.json {
            eprintln!("{}", json!({"error": {"message": format!("{error:#}")}}));
        } else {
            eprintln!("error: {error:#}");
        }
        std::process::exit(1);
    }
}
