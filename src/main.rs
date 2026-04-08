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
        /// Port to listen on (default: 31921)
        #[arg(long)]
        port: Option<u16>,
    },
    /// Configure git-lfs to use the daemon for this repository.
    Setup,
    /// Check prerequisites, install git-lfs if needed, and optionally install
    /// the daemon as a background service (systemd on Linux, launchd on macOS).
    Install {
        /// Also install the daemon as a system service
        #[arg(long)]
        service: bool,
        /// Daemon port for the service (default: 31921)
        #[arg(long, default_value = "31921")]
        port: u16,
    },
    /// Remove the daemon background service (launchd on macOS, systemd on Linux).
    Uninstall,
    /// Clone a repository and configure git-lfs to use the blossom-lfs daemon.
    ///
    /// All arguments are passed directly to `git clone`. Supports the same
    /// flags as `git clone` (e.g., `--recurse-submodules`, `--depth 1`).
    #[command(trailing_var_arg = true, allow_hyphen_values = true)]
    Clone {
        /// Arguments passed to git clone (repo URL, directory, flags)
        #[arg(required = true)]
        args: Vec<String>,
    },
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
            let daemon_port = port.unwrap_or(blossom_lfs::DEFAULT_DAEMON_PORT);
            blossom_lfs::run_daemon(daemon_port).await
        }
        Commands::Setup => blossom_lfs::setup_repo(None),
        Commands::Install { service, port } => blossom_lfs::install(service, port),
        Commands::Uninstall => blossom_lfs::uninstall_service(),
        Commands::Clone { args } => blossom_lfs::clone_repo(&args),
    }
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
