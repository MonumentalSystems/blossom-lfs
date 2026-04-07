use base64::Engine;
use blossom_lfs::Config;
use clap::{Parser, Subcommand};
use tracing_subscriber::{fmt, EnvFilter};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    #[command(flatten)]
    global: GlobalArgs,
}

#[derive(Parser)]
struct GlobalArgs {
    #[arg(long, value_name = "PATH")]
    log_output: Option<std::path::PathBuf>,

    #[arg(long, value_name = "LEVEL", default_value = "info")]
    log_level: String,

    #[arg(long)]
    log_json: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the LFS daemon (HTTP server for git-lfs operations).
    Daemon {
        /// Port to listen on (default: from config, or 31921)
        #[arg(long)]
        port: Option<u16>,
    },
    /// Configure git-lfs to use the daemon for this repository.
    Setup,
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("{}", e);
        std::process::exit(1);
    }
}

async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_tracing(&cli.global);

    match cli.command {
        Commands::Daemon { port } => {
            let config = Config::from_git_config()
                .map_err(|e| anyhow::anyhow!("Failed to load configuration: {}", e))?;
            let daemon_port = port.unwrap_or(config.daemon_port);
            blossom_lfs::daemon::run_daemon(daemon_port).await
        }
        Commands::Setup => setup(),
    }
}

fn setup() -> anyhow::Result<()> {
    let config = Config::from_git_config()
        .map_err(|e| anyhow::anyhow!("Failed to load configuration: {}", e))?;

    let repo_path = std::env::current_dir()
        .map_err(|e| anyhow::anyhow!("Failed to get current directory: {}", e))?;
    let canonical = repo_path
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("Failed to canonicalize path: {}", e))?;
    let path_str = canonical.to_string_lossy();

    let repo_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(path_str.as_bytes());
    let base_url = format!("http://localhost:{}/lfs/{}", config.daemon_port, repo_b64);

    std::process::Command::new("git")
        .args(["config", "lfs.url", &format!("{}/objects/batch", base_url)])
        .status()
        .map_err(|e| anyhow::anyhow!("Failed to run git config: {}", e))?;

    std::process::Command::new("git")
        .args(["config", "lfs.locksurl", &format!("{}/locks", base_url)])
        .status()
        .map_err(|e| anyhow::anyhow!("Failed to run git config: {}", e))?;

    std::process::Command::new("git")
        .args(["config", "lfs.locksverify", "true"])
        .status()
        .map_err(|e| anyhow::anyhow!("Failed to run git config: {}", e))?;

    // Clean up old custom transfer agent config if present
    for key in [
        "lfs.standalonetransferagent",
        "lfs.customtransfer.blossom-lfs.path",
        "lfs.customtransfer.blossom-lfs.args",
        "lfs.customtransfer.blossom-lfs.concurrent",
        "lfs.customtransfer.blossom-lfs.original",
    ] {
        std::process::Command::new("git")
            .args(["config", "--unset", key])
            .status()
            .ok();
    }

    println!("Configured git-lfs to use blossom-lfs daemon:");
    println!("  lfs.url        = {}/objects/batch", base_url);
    println!("  lfs.locksurl   = {}/locks", base_url);
    println!("  lfs.locksverify = true");
    println!();
    println!(
        "Make sure 'blossom-lfs daemon' is running on port {}.",
        config.daemon_port
    );
    Ok(())
}

fn init_tracing(args: &GlobalArgs) {
    let filter = EnvFilter::try_new(&args.log_level)
        .or_else(|_| EnvFilter::try_new("info"))
        .unwrap();

    if let Some(log_output) = &args.log_output {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_output)
            .unwrap();

        if args.log_json {
            fmt()
                .json()
                .with_env_filter(filter)
                .with_writer(file)
                .with_target(true)
                .with_span_events(fmt::format::FmtSpan::CLOSE)
                .init();
        } else {
            fmt()
                .with_env_filter(filter)
                .with_writer(file)
                .with_target(true)
                .with_ansi(false)
                .init();
        }
    } else if args.log_json {
        fmt()
            .json()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .with_target(true)
            .with_span_events(fmt::format::FmtSpan::CLOSE)
            .init();
    } else {
        fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .with_target(true)
            .init();
    }
}
