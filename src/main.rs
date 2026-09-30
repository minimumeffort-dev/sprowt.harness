mod app;
mod codex;
mod sprout;
mod store;
mod ui;
mod worker;

use std::io::{self, IsTerminal};

use clap::Parser;
use ratatui::crossterm::{
    event::{DisableBracketedPaste, EnableBracketedPaste},
    execute,
};

use app::App;

#[derive(Parser)]
#[command(version, about = "Coordinate coding agents in your project")]
struct Cli {
    /// Disable animations.
    #[arg(long)]
    no_motion: bool,
}

fn main() -> io::Result<()> {
    let cli = Cli::parse();
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(io::Error::other(
            "Interactive mode needs a terminal. Try --help.",
        ));
    }

    let mut app = App::new(std::env::current_dir()?, !cli.no_motion)?;
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
