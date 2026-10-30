use clap::Parser;

const ABOUT: &str = "Unofficial Pushover Open Client for the Linux desktop.\n\
                     Not released or supported by Pushover.";

#[derive(Parser)]
#[command(version, about = ABOUT, long_about = None)]
struct Cli {}

fn main() -> anyhow::Result<()> {
    let _cli = Cli::parse();
    Ok(())
}
