mod app;
mod codex;
mod execution;
mod git_mod;
mod git_sync;
mod mailbox;
mod mod_sync;
mod packages;
mod plan;
mod router;
mod rpc;
mod sandbox;
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
    event::{DisableBracketedPaste, EnableBracketedPaste},
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
}

fn main() -> io::Result<()> {
    let cli = Cli::parse();
    if matches!(cli.command, Some(Command::Setup)) {
        return router::setup();
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
    execute!(io::stdout(), EnableBracketedPaste)?;
    app.run(&mut terminal)
}

struct TerminalCleanup;

impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableBracketedPaste);
        ratatui::restore();
    }
}
