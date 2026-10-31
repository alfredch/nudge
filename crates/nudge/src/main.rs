mod actions;
mod backoff;
mod cache;
mod cli;
mod config;
mod control;
mod credentials;
mod daemon;
mod export;
mod history;
mod instance;
mod login;
mod notify;
mod paths;
mod service;
mod sound;
mod status;
mod tray;
mod wake;

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use crate::paths::Paths;

/// The API terms require this notice wherever the client identifies itself.
const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (unofficial Pushover Open Client, not released or supported by Pushover)"
);
const ABOUT: &str = "Unofficial Pushover Open Client for the Linux desktop.\n\
                     Not released or supported by Pushover.";

/// Exit code when the user has to log in again (EX_CONFIG from sysexits.h).
const EXIT_NEEDS_LOGIN: u8 = 78;
/// Exit code when another daemon is running (EX_UNAVAILABLE). Like 78, a
/// restart would not help; the systemd unit excludes both from restarting.
const EXIT_ALREADY_RUNNING: u8 = 69;

#[derive(Parser)]
#[command(version = VERSION, about = ABOUT, long_about = None)]
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
    /// Show whether the daemon is connected, paused, and the last message.
    Status,
    /// Pause notifications; emergencies (priority 2) still get through.
    Pause {
        /// How long, e.g. 30m, 2h, 1d. Without it: until `nudge resume`.
        duration: Option<String>,
    },
    /// End a pause and summarize the messages held back.
    Resume,
    /// Start nudge with the desktop session (systemd user service).
    InstallService,
    /// Stop the service and remove it.
    UninstallService,
    /// List received messages, oldest first.
    History {
        /// Only messages from this app.
        #[arg(long)]
        app: Option<String>,
        /// Only messages newer than this: a duration (2d, 3h) or a date (2026-10-30).
        #[arg(long)]
        since: Option<String>,
        /// Show at most this many (the newest).
        #[arg(long, short = 'n', default_value_t = 20)]
        limit: usize,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
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
        Command::Status => return cli::status(&paths).await,
        Command::Pause { duration } => return cli::pause(duration.as_deref()).await,
        Command::Resume => return cli::resume().await,
        Command::InstallService => service::install().await?,
        Command::UninstallService => service::uninstall()?,
        Command::History {
            app,
            since,
            limit,
            json,
        } => return cli::history(&paths, app, since.as_deref(), limit, json),
        Command::Daemon => match daemon::run(&paths, user_agent).await? {
            daemon::Exit::Shutdown => {}
            daemon::Exit::NeedsLogin(reason) => {
                eprintln!("{reason}");
                return Ok(ExitCode::from(EXIT_NEEDS_LOGIN));
            }
            daemon::Exit::AlreadyRunning => {
                eprintln!("Another nudge daemon is already running.");
                return Ok(ExitCode::from(EXIT_ALREADY_RUNNING));
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
