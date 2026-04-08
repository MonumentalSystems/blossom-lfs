use base64::Engine;
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
            let daemon_port = port.unwrap_or(31921);
            blossom_lfs::daemon::run_daemon(daemon_port).await
        }
        Commands::Setup => setup_repo(None),
        Commands::Install { service, port } => install(service, port),
        Commands::Clone { args } => clone_repo(&args),
    }
}

fn resolve_daemon_port() -> u16 {
    std::env::var("BLOSSOM_DAEMON_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(31921)
}

fn setup_repo(repo_path: Option<&std::path::Path>) -> anyhow::Result<()> {
    let daemon_port = resolve_daemon_port();

    let canonical = match repo_path {
        Some(p) => p
            .canonicalize()
            .map_err(|e| anyhow::anyhow!("Failed to canonicalize path: {}", e))?,
        None => {
            let cwd = std::env::current_dir()
                .map_err(|e| anyhow::anyhow!("Failed to get current directory: {}", e))?;
            cwd.canonicalize()
                .map_err(|e| anyhow::anyhow!("Failed to canonicalize path: {}", e))?
        }
    };
    let path_str = canonical.to_string_lossy();

    let repo_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(path_str.as_bytes());
    let base_url = format!("http://localhost:{}/lfs/{}", daemon_port, repo_b64);

    std::process::Command::new("git")
        .args(["config", "lfs.url", &base_url])
        .current_dir(&canonical)
        .status()
        .map_err(|e| anyhow::anyhow!("Failed to run git config: {}", e))?;

    std::process::Command::new("git")
        .args(["config", "lfs.locksurl", &format!("{}/locks", base_url)])
        .current_dir(&canonical)
        .status()
        .map_err(|e| anyhow::anyhow!("Failed to run git config: {}", e))?;

    std::process::Command::new("git")
        .args(["config", "lfs.locksverify", "true"])
        .current_dir(&canonical)
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
            .current_dir(&canonical)
            .status()
            .ok();
    }

    tracing::info!(
        lfs.url = %base_url,
        lfs.locksurl = %format!("{}/locks", base_url),
        lfs.locksverify = true,
        daemon.port = daemon_port,
        "configured git-lfs to use blossom-lfs daemon"
    );
    Ok(())
}

fn install(service: bool, port: u16) -> anyhow::Result<()> {
    let daemon_port = port;

    // Check git
    let git_ok = std::process::Command::new("git")
        .args(["--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if git_ok {
        let ver = std::process::Command::new("git")
            .args(["--version"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        eprintln!("[ok] {}", ver);
    } else {
        anyhow::bail!("git is not installed — install it from https://git-scm.com");
    }

    // Check git-lfs
    let lfs_ok = std::process::Command::new("git")
        .args(["lfs", "version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if lfs_ok {
        let ver = std::process::Command::new("git")
            .args(["lfs", "version"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        eprintln!("[ok] {}", ver);
    } else {
        eprintln!("[missing] git-lfs — attempting to install...");
        if !try_install_git_lfs()? {
            anyhow::bail!(
                "could not install git-lfs automatically\n\
                 Install manually: https://git-lfs.com"
            );
        }
    }

    // Run git lfs install (sets up hooks)
    let status = std::process::Command::new("git")
        .args(["lfs", "install"])
        .status()
        .map_err(|e| anyhow::anyhow!("git lfs install failed: {}", e))?;
    if status.success() {
        eprintln!("[ok] git lfs hooks installed");
    }

    // Check blossom-lfs binary is in PATH
    let self_path = std::env::current_exe().unwrap_or_default();
    eprintln!("[ok] blossom-lfs at {}", self_path.display());

    if service {
        install_service(daemon_port, &self_path)?;
    } else {
        eprintln!();
        eprintln!("To install the daemon as a background service, run:");
        eprintln!("  blossom-lfs install --service");
    }

    eprintln!();
    eprintln!("Installation complete. Next steps:");
    eprintln!("  1. Start the daemon:  blossom-lfs daemon");
    eprintln!("  2. Clone a repo:      blossom-lfs clone <url>");
    eprintln!("  3. Or setup existing: cd <repo> && blossom-lfs setup");

    Ok(())
}

fn try_install_git_lfs() -> anyhow::Result<bool> {
    // Try brew (macOS)
    if cfg!(target_os = "macos") {
        let status = std::process::Command::new("brew")
            .args(["install", "git-lfs"])
            .status();
        if let Ok(s) = status {
            if s.success() {
                eprintln!("[ok] git-lfs installed via brew");
                return Ok(true);
            }
        }
    }

    // Try apt (Debian/Ubuntu)
    if cfg!(target_os = "linux") {
        let status = std::process::Command::new("sudo")
            .args(["apt-get", "install", "-y", "git-lfs"])
            .status();
        if let Ok(s) = status {
            if s.success() {
                eprintln!("[ok] git-lfs installed via apt");
                return Ok(true);
            }
        }

        // Try dnf (Fedora/RHEL)
        let status = std::process::Command::new("sudo")
            .args(["dnf", "install", "-y", "git-lfs"])
            .status();
        if let Ok(s) = status {
            if s.success() {
                eprintln!("[ok] git-lfs installed via dnf");
                return Ok(true);
            }
        }
    }

    Ok(false)
}

fn install_service(daemon_port: u16, exe_path: &std::path::Path) -> anyhow::Result<()> {
    let exe = exe_path.to_string_lossy();

    if cfg!(target_os = "macos") {
        install_launchd_service(daemon_port, &exe)?;
    } else if cfg!(target_os = "linux") {
        install_systemd_service(daemon_port, &exe)?;
    } else {
        eprintln!("[skip] service installation not supported on this platform");
        eprintln!("       run the daemon manually: blossom-lfs daemon");
    }

    Ok(())
}

#[cfg(target_os = "macos")]
fn install_launchd_service(daemon_port: u16, exe: &str) -> anyhow::Result<()> {
    let label = "com.monumentalsystems.blossom-lfs";
    let home = std::env::var("HOME").map_err(|_| anyhow::anyhow!("HOME not set"))?;
    let plist_dir = format!("{}/Library/LaunchAgents", home);
    let plist_path = format!("{}/{}.plist", plist_dir, label);
    let log_path = format!("{}/Library/Logs/blossom-lfs.log", home);

    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>daemon</string>
        <string>--port</string>
        <string>{daemon_port}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{log_path}</string>
    <key>StandardErrorPath</key>
    <string>{log_path}</string>
</dict>
</plist>"#
    );

    std::fs::create_dir_all(&plist_dir)
        .map_err(|e| anyhow::anyhow!("create LaunchAgents dir: {}", e))?;
    std::fs::write(&plist_path, &plist).map_err(|e| anyhow::anyhow!("write plist: {}", e))?;

    // Get UID for launchctl domain target
    let uid = std::process::Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "501".to_string());

    // Unload existing if present (ignore errors)
    let _ = std::process::Command::new("launchctl")
        .args(["bootout", &format!("gui/{}", uid), &plist_path])
        .status();

    let status = std::process::Command::new("launchctl")
        .args(["bootstrap", &format!("gui/{}", uid), &plist_path])
        .status()
        .map_err(|e| anyhow::anyhow!("launchctl bootstrap failed: {}", e))?;

    if status.success() {
        eprintln!("[ok] launchd service installed and started");
        eprintln!("     plist: {}", plist_path);
        eprintln!("     logs:  {}", log_path);
        eprintln!("     stop:  launchctl bootout gui/{} {}", uid, plist_path);
    } else {
        anyhow::bail!("launchctl bootstrap failed");
    }

    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn install_launchd_service(_daemon_port: u16, _exe: &str) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(target_os = "linux")]
fn install_systemd_service(daemon_port: u16, exe: &str) -> anyhow::Result<()> {
    let home = std::env::var("HOME").map_err(|_| anyhow::anyhow!("HOME not set"))?;
    let unit_dir = format!("{}/.config/systemd/user", home);
    let unit_path = format!("{}/blossom-lfs.service", unit_dir);

    let unit = format!(
        r#"[Unit]
Description=blossom-lfs Git LFS daemon
After=network.target

[Service]
Type=simple
ExecStart={exe} daemon --port {daemon_port}
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
"#
    );

    std::fs::create_dir_all(&unit_dir)
        .map_err(|e| anyhow::anyhow!("create systemd user dir: {}", e))?;
    std::fs::write(&unit_path, &unit).map_err(|e| anyhow::anyhow!("write service file: {}", e))?;

    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();

    let _ = std::process::Command::new("systemctl")
        .args(["--user", "enable", "blossom-lfs.service"])
        .status();

    let status = std::process::Command::new("systemctl")
        .args(["--user", "start", "blossom-lfs.service"])
        .status()
        .map_err(|e| anyhow::anyhow!("systemctl start failed: {}", e))?;

    if status.success() {
        eprintln!("[ok] systemd user service installed and started");
        eprintln!("     unit:   {}", unit_path);
        eprintln!("     status: systemctl --user status blossom-lfs");
        eprintln!("     stop:   systemctl --user stop blossom-lfs");
        eprintln!("     logs:   journalctl --user -u blossom-lfs -f");
    } else {
        anyhow::bail!("systemctl start failed — check: systemctl --user status blossom-lfs");
    }

    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn install_systemd_service(_daemon_port: u16, _exe: &str) -> anyhow::Result<()> {
    Ok(())
}

fn clone_repo(args: &[String]) -> anyhow::Result<()> {
    let daemon_port = resolve_daemon_port();

    // Check if daemon is reachable
    let addr = format!("127.0.0.1:{}", daemon_port);
    match std::net::TcpStream::connect_timeout(
        &addr.parse().unwrap(),
        std::time::Duration::from_secs(2),
    ) {
        Ok(_) => tracing::info!(daemon.port = daemon_port, "daemon is reachable"),
        Err(_) => tracing::warn!(
            daemon.port = daemon_port,
            "daemon does not appear to be running — git lfs pull will fail unless it is started"
        ),
    }

    // Clone with LFS smudge disabled — pass all args straight to git clone
    tracing::info!("cloning repository (LFS objects deferred)");
    let output = std::process::Command::new("git")
        .arg("clone")
        .args(args)
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run git clone: {}", e))?;

    // Print stderr so the user sees git's progress output
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprint!("{}", stderr);

    if !output.status.success() {
        anyhow::bail!("git clone failed");
    }

    // Parse target directory from git's "Cloning into 'dirname'..." message
    let target_name = stderr
        .lines()
        .find_map(|line| {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("Cloning into '") {
                rest.strip_suffix("'...").map(String::from)
            } else if let Some(rest) = line.strip_prefix("Cloning into bare repository '") {
                rest.strip_suffix("'...").map(String::from)
            } else {
                None
            }
        })
        .ok_or_else(|| anyhow::anyhow!("could not determine cloned directory from git output"))?;

    let target_dir = std::path::PathBuf::from(&target_name)
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("cloned directory '{}' not found: {}", target_name, e))?;

    // Run setup
    tracing::info!(path = %target_dir.display(), "configuring blossom-lfs");
    setup_repo(Some(&target_dir))?;

    // Pull LFS objects through daemon
    tracing::info!("pulling LFS objects through blossom-lfs daemon");
    let status = std::process::Command::new("git")
        .args(["lfs", "pull"])
        .current_dir(&target_dir)
        .status()
        .map_err(|e| anyhow::anyhow!("failed to run git lfs pull: {}", e))?;

    if !status.success() {
        anyhow::bail!("git lfs pull failed");
    }

    tracing::info!(path = %target_dir.display(), "clone complete — repository ready");
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
