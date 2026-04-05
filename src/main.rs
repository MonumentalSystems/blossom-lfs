use blossom_lfs::{Agent, Config};
use clap::Parser;
use log::error;
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Set log file name
    #[arg(long, value_name = "PATH")]
    log_output: Option<std::path::PathBuf>,

    /// Set log level
    #[arg(long, value_name = "LEVEL", default_value = "info")]
    log_level: log::Level,

    /// List available configuration
    #[arg(long)]
    config_info: bool,
}

async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    if let Some(log_output) = cli.log_output {
        simplelog::WriteLogger::init(
            cli.log_level.to_level_filter(),
            simplelog::Config::default(),
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_output)?,
        )?;
    } else {
        simplelog::TermLogger::init(
            cli.log_level.to_level_filter(),
            simplelog::Config::default(),
            simplelog::TerminalMode::Stderr,
            simplelog::ColorChoice::Auto,
        )?;
    }

    if cli.config_info {
        println!("Blossom LFS Configuration:");
        println!("  Set in .lfsdalconfig or .git/config:");
        println!("    lfs-dal.server = <blossom_server_url>");
        println!("    lfs-dal.private-key = <nostr_private_key_or_nsec>");
        println!("    lfs-dal.chunk-size = 16777216 (default: 16MB)");
        println!("    lfs-dal.max-concurrent-uploads = 8 (default)");
        println!("    lfs-dal.max-concurrent-downloads = 8 (default)");
        println!("    lfs-dal.auth-expiration = 3600 (default: 1 hour)");
        println!();
        println!("  Or use environment variables:");
        println!("    BLOSSOM_SERVER_URL = <blossom_server_url>");
        println!("    NOSTR_PRIVATE_KEY = <nostr_private_key>");
        return Ok(());
    }

    let config = Config::from_git_config()
        .map_err(|e| anyhow::anyhow!("Failed to load configuration: {}", e))?;

    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let mut agent =
        Agent::new(config, tx).map_err(|e| anyhow::anyhow!("Failed to initialize agent: {}", e))?;

    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let mut stdout = io::stdout();
            if let Err(e) = stdout.write_all(format!("{}\n", msg).as_bytes()).await {
                eprintln!("Failed to write to stdout: {}", e);
            }
            if let Err(e) = stdout.flush().await {
                eprintln!("Failed to flush stdout: {}", e);
            }
        }
    });

    let stdin = io::stdin();
    let mut lines = io::BufReader::new(stdin).lines();

    while let Some(line) = lines.next_line().await? {
        if let Err(e) = agent.process(&line).await {
            error!("Error processing request: {}", e);
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("{}", e);
        error!("{}", e);
        std::process::exit(1);
    }
}
