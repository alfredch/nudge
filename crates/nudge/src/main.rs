mod backoff;
mod credentials;
mod daemon;
mod history;
mod instance;
mod login;
mod notify;
mod paths;
mod wake;

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use crate::paths::Paths;

const ABOUT: &str = "Unofficial Pushover Open Client for the Linux desktop.\n\
                     Not released or supported by Pushover.";

/// Exit code when the user has to log in again (EX_CONFIG from sysexits.h).
const EXIT_NEEDS_LOGIN: u8 = 78;

#[derive(Parser)]
#[command(version, about = ABOUT, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Log in to Pushover and register this computer as a device.
    Login {
        /// Register a new device instead of reusing the stored one.
        #[arg(long)]
        new_device: bool,
    },
    /// Remove the stored credentials.
    Logout,
    /// Receive messages in the foreground (used by the systemd service).
    Daemon,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging();
    match run(cli).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("Error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    let paths = Paths::new()?;
    let user_agent = nudge_api::user_agent("nudge", env!("CARGO_PKG_VERSION"));
    match cli.command {
        Command::Login { new_device } => login::login(&paths, &user_agent, new_device).await?,
        Command::Logout => login::logout(&paths).await?,
        Command::Daemon => match daemon::run(&paths, user_agent).await? {
            daemon::Exit::Shutdown => {}
            daemon::Exit::NeedsLogin(reason) => {
                eprintln!("{reason}");
                return Ok(ExitCode::from(EXIT_NEEDS_LOGIN));
            }
        },
    }
    Ok(ExitCode::SUCCESS)
}

fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    // Under systemd, journald adds its own timestamps.
    let under_systemd = stderr_is_journal();
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(!under_systemd);
    if under_systemd {
        builder.without_time().init();
    } else {
        builder.init();
    }
}

/// Whether stderr is connected to journald. `JOURNAL_STREAM` alone is not
/// enough: terminals started by a systemd user unit inherit it, so compare
/// its `device:inode` with the actual stderr (see systemd.exec(5)).
fn stderr_is_journal() -> bool {
    use std::os::unix::fs::MetadataExt;

    let Some(value) = std::env::var_os("JOURNAL_STREAM") else {
        return false;
    };
    let Some((dev, ino)) = value.to_str().and_then(|v| v.split_once(':')) else {
        return false;
    };
    let Ok(meta) = std::fs::metadata("/proc/self/fd/2") else {
        return false;
    };
    dev.parse() == Ok(meta.dev()) && ino.parse() == Ok(meta.ino())
}
