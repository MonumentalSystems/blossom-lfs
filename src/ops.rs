//! High-level operations — setup, clone, install, uninstall.
//!
//! These are the library-accessible versions of the CLI commands.

use base64::Engine;
use std::path::{Path, PathBuf};

/// Default daemon port.
pub const DEFAULT_DAEMON_PORT: u16 = 31921;

/// Resolve the daemon port from environment or default.
pub fn resolve_daemon_port() -> u16 {
    std::env::var("BLOSSOM_DAEMON_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_DAEMON_PORT)
}

/// Configure a git repository to use the blossom-lfs daemon.
///
/// Sets `lfs.url`, `lfs.locksurl`, and `lfs.locksverify` in `.git/config`.
/// If `repo_path` is `None`, uses the current working directory.
pub fn setup_repo(repo_path: Option<&Path>) -> anyhow::Result<()> {
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

/// Clone a repository and configure git-lfs to use the blossom-lfs daemon.
///
/// All `args` are passed directly to `git clone`. After cloning, runs
/// `setup_repo` and `git lfs pull`.
pub fn clone_repo(args: &[String]) -> anyhow::Result<()> {
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

    // Clone with LFS smudge disabled
    tracing::info!("cloning repository (LFS objects deferred)");
    let output = std::process::Command::new("git")
        .arg("clone")
        .args(args)
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run git clone: {}", e))?;

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

    let target_dir = PathBuf::from(&target_name)
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("cloned directory '{}' not found: {}", target_name, e))?;

    // Run setup
    tracing::info!(path = %target_dir.display(), "configuring blossom-lfs");
    setup_repo(Some(&target_dir))?;

    // Checkout the default branch — clone may leave HEAD on a nonexistent
    // ref (e.g., 'master') when the actual branch is 'main'.
    // Find the first remote branch and check it out properly.
    let branch_output = std::process::Command::new("git")
        .args(["branch", "-r", "--format=%(refname:short)"])
        .current_dir(&target_dir)
        .output()
        .ok();
    if let Some(output) = branch_output {
        let branches = String::from_utf8_lossy(&output.stdout);
        if let Some(branch) = branches
            .lines()
            .find(|b| !b.contains("HEAD"))
            .or_else(|| branches.lines().next())
        {
            let local = branch.strip_prefix("origin/").unwrap_or(branch);
            let checkout = std::process::Command::new("git")
                .args(["checkout", "-f", "-B", local, branch])
                .current_dir(&target_dir)
                .env("GIT_LFS_SKIP_SMUDGE", "1")
                .output();
            if let Ok(out) = &checkout {
                if !out.status.success() {
                    tracing::warn!(
                        branch = local,
                        stderr = %String::from_utf8_lossy(&out.stderr),
                        "failed to checkout branch"
                    );
                }
            }
        }
    }

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

/// Check prerequisites and optionally install the daemon as a service.
pub fn install(service: bool, port: u16) -> anyhow::Result<()> {
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

    // Run git lfs install
    let status = std::process::Command::new("git")
        .args(["lfs", "install"])
        .status()
        .map_err(|e| anyhow::anyhow!("git lfs install failed: {}", e))?;
    if status.success() {
        eprintln!("[ok] git lfs hooks installed");
    }

    let self_path = std::env::current_exe().unwrap_or_default();
    eprintln!("[ok] blossom-lfs at {}", self_path.display());

    if service {
        install_service(port, &self_path)?;
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

/// Remove the daemon background service.
pub fn uninstall_service() -> anyhow::Result<()> {
    if cfg!(target_os = "macos") {
        let home = std::env::var("HOME").map_err(|_| anyhow::anyhow!("HOME not set"))?;
        let plist_path = format!(
            "{}/Library/LaunchAgents/com.monumentalsystems.blossom-lfs.plist",
            home
        );

        if !Path::new(&plist_path).exists() {
            anyhow::bail!("service not installed (no plist at {})", plist_path);
        }

        let _ = std::process::Command::new("launchctl")
            .args(["unload", &plist_path])
            .status();

        std::fs::remove_file(&plist_path)
            .map_err(|e| anyhow::anyhow!("failed to remove plist: {}", e))?;

        eprintln!("[ok] launchd service stopped and removed");
        eprintln!("     removed: {}", plist_path);
    } else if cfg!(target_os = "linux") {
        let home = std::env::var("HOME").map_err(|_| anyhow::anyhow!("HOME not set"))?;
        let unit_path = format!("{}/.config/systemd/user/blossom-lfs.service", home);

        if !Path::new(&unit_path).exists() {
            anyhow::bail!("service not installed (no unit at {})", unit_path);
        }

        let _ = std::process::Command::new("systemctl")
            .args(["--user", "stop", "blossom-lfs.service"])
            .status();

        let _ = std::process::Command::new("systemctl")
            .args(["--user", "disable", "blossom-lfs.service"])
            .status();

        std::fs::remove_file(&unit_path)
            .map_err(|e| anyhow::anyhow!("failed to remove unit file: {}", e))?;

        let _ = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status();

        eprintln!("[ok] systemd service stopped, disabled, and removed");
        eprintln!("     removed: {}", unit_path);
    } else {
        anyhow::bail!("service uninstall not supported on this platform");
    }

    Ok(())
}

fn try_install_git_lfs() -> anyhow::Result<bool> {
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

fn install_service(daemon_port: u16, exe_path: &Path) -> anyhow::Result<()> {
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

    let _ = std::process::Command::new("launchctl")
        .args(["unload", &plist_path])
        .status();

    let status = std::process::Command::new("launchctl")
        .args(["load", "-w", &plist_path])
        .status()
        .map_err(|e| anyhow::anyhow!("launchctl load failed: {}", e))?;

    if status.success() {
        eprintln!("[ok] launchd service installed and started");
        eprintln!("     plist: {}", plist_path);
        eprintln!("     logs:  {}", log_path);
        eprintln!("     stop:  launchctl unload {}", plist_path);
    } else {
        anyhow::bail!("launchctl load failed");
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
