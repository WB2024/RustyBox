use clap::{Parser, Subcommand};
use rustybox::{commands, perms};

#[derive(Parser)]
#[command(
    name = "rustybox",
    about = "Manage modded Xbox 360s: libraries, GOD conversion, console transfers, USB tools",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the web UI and API
    Serve(commands::serve::ServeArgs),
    /// Share a folder (for example an Xbox drive plugged into this PC) with a RustyBox elsewhere
    Agent(commands::agent::AgentArgs),
    /// Exit 0 if the local server answers (used by the Docker health check)
    Healthcheck(commands::healthcheck::HealthArgs),
}

fn main() {
    perms::init();
    let cli = Cli::parse();
    let result = match cli.command {
        Cmd::Serve(args) => commands::serve::run(args),
        Cmd::Agent(args) => commands::agent::run(args),
        Cmd::Healthcheck(args) => commands::healthcheck::run(args),
    };
    if let Err(e) = result {
        let err = e.to_box_error();
        eprintln!(
            "{}",
            serde_json::to_string_pretty(&err).unwrap_or_else(|_| e.to_string())
        );
        std::process::exit(1);
    }
}
