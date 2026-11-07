//! An overview of the coding agents running in your tmux panes, read from what each pane shows.
//!   agentic toggle   open the live board in a popup
//!   agentic status   counts for the status bar, once, or with --watch as they change
//!   agentic back     back to the agent you were on before the board last took you somewhere
//!   agentic bind     bind the keys config.toml names (the status watch does it again when the file changes)
//!   agentic config   write a commented config.toml with every setting, unless there is one
mod agents;
mod app;
mod config;
mod detect;
mod history;
mod input;
mod keymap;
mod popup;
mod rows;
mod sessions;
mod status;
mod text;
mod theme;
mod tmux;
mod ui;

use std::process::ExitCode;

fn main() -> ExitCode {
    let command = std::env::args().nth(1).unwrap_or_default();
    match command.as_str() {
        "toggle" => return ExitCode::from(popup::toggle() as u8),
        "inside" => return app::run(),
        "--version" => println!("{}", env!("CARGO_PKG_VERSION")),
        "status" if std::env::args().nth(2).as_deref() == Some("--watch") => status::watch(),
        "status" => {
            status::run(&agents::Registry::load());
        }
        "back" => status::back(),
        "bind" => popup::bind(),
        "config" => match config::write_template() {
            Ok(done) => println!("{done}"),
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::FAILURE;
            }
        },
        "finish-rename" => {
            let args: Vec<String> = std::env::args().skip(2).collect();
            if let [pane, tool] = &args[..] {
                app::finish_rename(pane, tool);
            }
        }
        _ => {
            eprintln!("usage: agentic toggle | status [--watch] | back | bind | config | --version");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}
