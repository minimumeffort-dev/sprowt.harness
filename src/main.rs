mod agents;
mod app;
mod checks;
mod codex;
mod conflict;
mod desktop;
mod diagnostics;
mod engine;
mod environment;
mod execution;
mod git_mod;
mod git_sync;
mod mailbox;
mod mod_sync;
mod muse;
mod network;
mod packages;
mod plan;
mod pr_feedback;
mod publication;
mod repair;
mod review;
mod router;
mod rpc;
mod sandbox;
mod scheduler;
mod skills;
mod sprout;
mod store;
mod task_worktree;
mod tools;
mod ui;
mod worker;
mod workspace;

use std::io::{self, IsTerminal};

use clap::{Parser, Subcommand};
use ratatui::crossterm::{
    event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute,
};

use app::App;

#[derive(Parser)]
#[command(version, about = "Coordinate coding agents in your project")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// Disable animations.
    #[arg(long)]
    no_motion: bool,
    /// Prune closed worktrees after this many days; 0 keeps them indefinitely.
    #[arg(long, default_value_t = 30)]
    closed_worktree_days: u32,
}

#[derive(Subcommand)]
enum Command {
    /// Configure host-only Jev routing from .env.local.
    Setup,
    /// Run the native app's versioned JSONL transport over stdin/stdout.
    Desktop {
        #[arg(long)]
        project: std::path::PathBuf,
        #[arg(long)]
        data_dir: Option<std::path::PathBuf>,
    },
}

fn main() -> io::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Setup) => return router::setup(),
        Some(Command::Desktop { project, data_dir }) => {
            return desktop::run(project, data_dir, cli.closed_worktree_days);
        }
        None => {}
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(io::Error::other(
            "Interactive mode needs a terminal. Try --help.",
        ));
    }

    let mut app = App::new(std::env::current_dir()?, !cli.no_motion)?;
    app.prune_closed(cli.closed_worktree_days)
        .map_err(io::Error::other)?;
    let mut terminal = ratatui::init();
    let _cleanup = TerminalCleanup;
    execute!(io::stdout(), EnableBracketedPaste, EnableMouseCapture)?;
    app.run(&mut terminal)
}

struct TerminalCleanup;

impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableMouseCapture, DisableBracketedPaste);
        ratatui::restore();
    }
}
