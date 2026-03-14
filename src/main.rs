use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use colored::Colorize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

mod box_mgr;
mod config;
mod error;
mod graph;
mod health;
mod ipc;
mod log;
mod proxy;
mod state;
mod supervisor;
mod ui;
mod watcher;

use config::DevConfig;
use error::{DevError, Result};
use ipc::{IpcRequest, IpcResponse};
use supervisor::Supervisor;

#[derive(Parser)]
#[command(
    name = "a3s",
    version,
    about = "a3s — local development orchestration for the A3S monorepo",
    allow_external_subcommands = true
)]
struct Cli {
    /// Path to A3sfile.hcl
    #[arg(short, long, default_value = "A3sfile.hcl")]
    file: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start all (or named) services in dependency order
    Up {
        /// Start only these services
        services: Vec<String>,
        /// Run as background daemon (detach from terminal)
        #[arg(short, long)]
        detach: bool,
        /// Disable the web UI (default: enabled on port 10350)
        #[arg(long)]
        no_ui: bool,
        /// Web UI port
        #[arg(long, default_value_t = ui::DEFAULT_UI_PORT)]
        ui_port: u16,
        /// Wait for all services to become healthy before returning (requires --detach)
        #[arg(long)]
        wait: bool,
        /// Timeout in seconds for --wait (default: 60)
        #[arg(long, default_value_t = 60)]
        wait_timeout: u64,
    },
    /// Stop all (or named) services
    Down {
        /// Stop only these services
        services: Vec<String>,
    },
    /// Restart a service
    Restart { service: String },
    /// Show service status (alias: ps)
    #[command(alias = "ps")]
    Status {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Tail logs (all services or one)
    Logs {
        /// Filter to a specific service
        service: Option<String>,
        /// Keep streaming
        #[arg(short, long, default_value_t = true)]
        follow: bool,
        /// Filter log lines by keyword (case-insensitive)
        #[arg(short, long)]
        grep: Option<String>,
        /// Number of historical lines to show (default: 200)
        #[arg(short = 'n', long, default_value_t = 200)]
        last: usize,
    },
    /// Generate a new A3sfile.hcl in the current directory
    Init,
    /// Validate A3sfile.hcl without starting anything
    Validate {
        /// Also check that service binaries exist on PATH and ports are not already in use
        #[arg(long)]
        strict: bool,
    },
    /// Show live CPU and memory usage per service (requires a running daemon)
    Top {
        /// Refresh interval in seconds
        #[arg(short, long, default_value_t = 2)]
        interval: u64,
    },
    /// Upgrade a3s to the latest version
    Upgrade,
    /// List all installed a3s ecosystem tools
    List,
    /// Update installed a3s ecosystem tools (all if no names given)
    Update {
        /// Tool name(s) to update: box, gateway, power (default: all)
        tools: Vec<String>,
    },
    /// Run a command in a service's environment and directory
    Exec {
        /// Service name to take env and dir from
        service: String,
        /// Command and arguments
        #[arg(trailing_var_arg = true, required = true)]
        cmd: Vec<String>,
    },
    /// Run a one-off command with the environment from A3sfile.hcl
    Run {
        /// Load env from a specific service (default: merge all services)
        #[arg(short, long)]
        service: Option<String>,
        /// Command and arguments to run
        #[arg(trailing_var_arg = true, required = true)]
        cmd: Vec<String>,
    },
    /// Reload A3sfile.hcl without restarting unchanged services
    Reload,
    /// Proxy to an a3s ecosystem tool (e.g. `a3s box`, `a3s gateway`)
    #[command(external_subcommand)]
    Tool(Vec<String>),
}

#[tokio::main]
async fn main() {
    // Parse CLI first so we can read log_level from A3sfile.hcl for `up`
    let cli = Cli::parse();

    let log_level = if matches!(cli.command, Commands::Up { .. }) {
        std::fs::read_to_string(&cli.file)
            .ok()
            .and_then(|s| hcl::from_str::<config::DevConfig>(&s).ok())
            .map(|c| c.dev.log_level)
            .unwrap_or_else(|| "info".into())
    } else {
        "warn".into()
    };

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(log_level)),
        )
        .without_time()
        .init();

    if let Err(e) = run(cli).await {
        eprintln!("{} {e}", "[a3s]".red().bold());
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    // Project-specific socket path — computed once and used by all IPC client commands.
    let sock = ipc::socket_path(&cli.file);

    match &cli.command {
        Commands::Up {
            services,
            detach,
            no_ui,
            ui_port,
            wait,
            wait_timeout,
        } => {
            if *detach {
                // Re-launch self as background daemon, dropping --detach flag
                let exe = std::env::current_exe()
                    .map_err(|e| DevError::Config(format!("cannot find self: {e}")))?;
                let mut args: Vec<String> =
                    vec!["--file".into(), cli.file.display().to_string(), "up".into()];
                if *no_ui {
                    args.push("--no-ui".into());
                }
                if *ui_port != ui::DEFAULT_UI_PORT {
                    args.push("--ui-port".into());
                    args.push(ui_port.to_string());
                }
                args.extend(services.iter().cloned());

                std::process::Command::new(&exe)
                    .args(&args)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .map_err(|e| DevError::Config(format!("failed to daemonize: {e}")))?;

                println!("{} a3s daemon started in background", "✓".green());
                if *wait {
                    println!("{} waiting for services to become healthy...", "→".cyan());
                    wait_for_healthy(&sock, *wait_timeout).await?;
                    println!("{} all services healthy", "✓".green());
                } else {
                    println!("  run {} to check status", "a3s status".cyan());
                    println!("  run {} to stop", "a3s down".cyan());
                }
                return Ok(());
            }

            let cfg = Arc::new(DevConfig::from_file(&cli.file)?);

            // Start proxy
            let proxy = Arc::new(proxy::ProxyRouter::new(cfg.dev.proxy_port));
            let proxy_port = cfg.dev.proxy_port;
            let proxy_run = proxy.clone();
            tokio::spawn(async move { proxy_run.run().await });
            println!("{} proxy  http://*.localhost:{}", "→".cyan(), proxy_port);

            let (sup, _) = Supervisor::new(cfg.clone(), proxy, cli.file.clone());
            let sup: Arc<Supervisor> = Arc::new(sup);

            tokio::spawn(supervisor::ipc::serve(sup.clone()));

            // Start web UI
            if !no_ui {
                let ui_port = *ui_port;
                let sup_ui = sup.clone();
                tokio::spawn(async move { ui::serve(sup_ui, ui_port).await });
                println!("{} ui     http://localhost:{}", "→".cyan(), ui_port);
                // Open browser after a short delay
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    let _ = std::process::Command::new("open")
                        .arg(format!("http://localhost:{ui_port}"))
                        .spawn();
                });
            }

            if services.is_empty() {
                sup.clone().start_all().await?;
            } else {
                sup.clone().start_named(services).await?;
            }

            // Wait for Ctrl+C, SIGTERM (shutdown) or SIGHUP (config reload).
            #[cfg(unix)]
            {
                use tokio::signal::unix::{signal, SignalKind};
                let mut sigterm = signal(SignalKind::terminate())
                    .unwrap_or_else(|_| signal(SignalKind::terminate()).unwrap());
                let mut sighup = signal(SignalKind::hangup())
                    .unwrap_or_else(|_| signal(SignalKind::hangup()).unwrap());
                loop {
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => break,
                        _ = sigterm.recv() => break,
                        _ = sighup.recv() => {
                            tracing::info!("SIGHUP received — reloading config");
                            if let Err(e) = sup.reload_from_disk().await {
                                tracing::error!("config reload failed: {e}");
                            }
                        }
                    }
                }
            }
            #[cfg(not(unix))]
            tokio::signal::ctrl_c().await.ok();

            println!("\n{} shutting down...", "→".yellow());
            sup.clone().stop_all().await;
            let _ = std::fs::remove_file(&sock);
        }

        Commands::Init => {
            let path = &cli.file;
            if path.exists() {
                return Err(DevError::Config(format!(
                    "{} already exists — delete it first or use a different path",
                    path.display()
                )));
            }
            let project_type = detect_project_type();
            let template = init_template(project_type);
            if project_type != "generic" {
                println!("{} detected {} project", "→".cyan(), project_type.cyan());
            }
            std::fs::write(path, template)
                .map_err(|e| DevError::Config(format!("write {}: {e}", path.display())))?;
            println!(
                "{} created {}",
                "✓".green(),
                path.display().to_string().cyan()
            );

            // Ask if user wants to git init
            if !std::path::Path::new(".git").exists() {
                print!("  initialize git repository? [Y/n] ");
                std::io::Write::flush(&mut std::io::stdout()).ok();
                let mut input = String::new();
                std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut input).ok();
                let answer = input.trim().to_lowercase();
                if answer.is_empty() || answer == "y" || answer == "yes" {
                    let status = std::process::Command::new("git")
                        .arg("init")
                        .status()
                        .map_err(|e| DevError::Config(format!("git init: {e}")))?;
                    if status.success() {
                        // Write .gitignore
                        let gitignore = ".gitignore";
                        if !std::path::Path::new(gitignore).exists() {
                            std::fs::write(gitignore, "target/\n.env\n*.sock\n")
                                .map_err(|e| DevError::Config(format!("write .gitignore: {e}")))?;
                            println!("{} created .gitignore", "✓".green());
                        }
                    }
                }
            }

            println!(
                "  edit {}, then run {} to start your services",
                path.display().to_string().cyan(),
                "a3s up".cyan()
            );
        }

        Commands::Validate { strict } => {
            let cfg = Arc::new(DevConfig::from_file(&cli.file)?);
            println!(
                "{} A3sfile.hcl is valid ({} services)",
                "✓".green(),
                cfg.service.len(),
            );
            for (name, svc) in &cfg.service {
                let deps = if svc.depends_on.is_empty() {
                    String::new()
                } else {
                    format!(" → depends on: {}", svc.depends_on.join(", "))
                };
                let sub = svc
                    .subdomain
                    .as_deref()
                    .map(|s| format!(" (http://{s}.localhost)"))
                    .unwrap_or_default();
                let port_str = if svc.port == 0 {
                    "auto".to_string()
                } else {
                    svc.port.to_string()
                };
                println!("  {} :{}{}{}", name.cyan(), port_str, sub, deps.dimmed());
            }
            graph::DependencyGraph::from_config(&cfg)?;
            println!("{} dependency graph OK", "✓".green());

            if *strict {
                println!("\n{} strict checks:", "→".cyan());
                let mut all_ok = true;
                for (name, svc) in &cfg.service {
                    if svc.disabled {
                        continue;
                    }
                    // Check that the binary in `cmd` exists on PATH.
                    let binary = svc.cmd.split_whitespace().next().unwrap_or("");
                    if which_binary(binary) {
                        println!("  {} [{name}] binary '{binary}' found", "✓".green());
                    } else {
                        println!(
                            "  {} [{name}] binary '{}' not found on PATH",
                            "✗".red(),
                            binary
                        );
                        all_ok = false;
                    }
                    // Check that a fixed port is not already bound.
                    if svc.port != 0 {
                        if port_available(svc.port) {
                            println!("  {} [{name}] port {} is available", "✓".green(), svc.port);
                        } else {
                            println!(
                                "  {} [{name}] port {} is already in use",
                                "✗".red(),
                                svc.port
                            );
                            all_ok = false;
                        }
                    }
                }
                if all_ok {
                    println!("{} all strict checks passed", "✓".green());
                } else {
                    return Err(DevError::Config("strict validation failed".into()));
                }
            }
        }

        Commands::Top { interval } => {
            loop {
                let resp = ipc_send(IpcRequest::Status, &sock).await;
                match resp {
                    Ok(IpcResponse::Status { rows }) => {
                        // Clear screen and move cursor to top-left.
                        print!("\x1b[2J\x1b[H");
                        println!(
                            "{:<16} {:<12} {:<8} {:<10} {}",
                            "SERVICE".bold(),
                            "STATE".bold(),
                            "PID".bold(),
                            "CPU%".bold(),
                            "MEM".bold(),
                        );
                        println!("{}", "─".repeat(56).dimmed());
                        for row in &rows {
                            let state_colored = match row.state.as_str() {
                                "running" => row.state.green().to_string(),
                                "starting" | "restarting" => row.state.yellow().to_string(),
                                "unhealthy" | "failed" => row.state.red().to_string(),
                                _ => row.state.dimmed().to_string(),
                            };
                            let (cpu_str, mem_str) = row
                                .pid
                                .and_then(query_process_stats)
                                .map(|(cpu, mem)| (format!("{cpu:.1}%"), format_bytes(mem)))
                                .unwrap_or_else(|| ("-".into(), "-".into()));
                            println!(
                                "{:<16} {:<20} {:<8} {:<10} {}",
                                row.name,
                                state_colored,
                                row.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
                                cpu_str,
                                mem_str,
                            );
                        }
                        println!(
                            "\n{} refresh every {}s — Ctrl+C to exit",
                            "·".dimmed(),
                            interval
                        );
                    }
                    Err(e) => {
                        eprintln!("{} {e}", "[a3s]".red().bold());
                        break;
                    }
                    _ => break,
                }
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => break,
                    _ = tokio::time::sleep(std::time::Duration::from_secs(*interval)) => {}
                }
            }
        }

        Commands::Status { json } => {
            let resp = ipc_send(IpcRequest::Status, &sock).await?;
            if let IpcResponse::Status { rows } = resp {
                if *json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&rows)
                            .map_err(|e| DevError::Config(format!("json: {e}")))?
                    );
                } else {
                    println!(
                        "{:<16} {:<12} {:<8} {:<6} {:<24} {}",
                        "SERVICE".bold(),
                        "STATE".bold(),
                        "PID".bold(),
                        "PORT".bold(),
                        "URL".bold(),
                        "UPTIME".bold(),
                    );
                    println!("{}", "─".repeat(72).dimmed());
                    for row in rows {
                        let state_colored = match row.state.as_str() {
                            "running" => row.state.green().to_string(),
                            "starting" | "restarting" => row.state.yellow().to_string(),
                            "unhealthy" | "failed" => row.state.red().to_string(),
                            _ => row.state.dimmed().to_string(),
                        };
                        let url = row
                            .subdomain
                            .map(|s| format!("http://{s}.localhost"))
                            .unwrap_or_default();
                        let uptime = row
                            .uptime_secs
                            .map(format_uptime)
                            .unwrap_or_else(|| "-".into());
                        println!(
                            "{:<16} {:<20} {:<8} {:<6} {:<24} {}",
                            row.name,
                            state_colored,
                            row.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
                            if row.port == 0 {
                                "auto".into()
                            } else {
                                row.port.to_string()
                            },
                            url.dimmed(),
                            uptime.dimmed(),
                        );
                    }
                }
            }
        }

        Commands::Down { services } => {
            ipc_send(
                IpcRequest::Stop {
                    services: services.clone(),
                },
                &sock,
            )
            .await?;
            println!("{} stopped", "✓".green());
        }

        Commands::Restart { service } => {
            ipc_send(
                IpcRequest::Restart {
                    service: service.clone(),
                },
                &sock,
            )
            .await?;
            println!("{} restarted {}", "✓".green(), service.cyan());
        }

        Commands::Reload => {
            ipc_send(IpcRequest::Reload, &sock).await?;
            println!("{} config reloaded", "✓".green());
        }

        Commands::Logs {
            service,
            follow,
            grep,
            last,
        } => {
            stream_logs(service.clone(), *follow, grep.clone(), *last, &sock).await?;
        }

        Commands::Upgrade => {
            let config = a3s_updater::UpdateConfig {
                binary_name: "a3s",
                crate_name: "a3s",
                current_version: env!("CARGO_PKG_VERSION"),
                github_owner: "A3S-Lab",
                github_repo: "Dev",
            };
            a3s_updater::run_update(&config)
                .await
                .map_err(|e| DevError::Config(e.to_string()))?;
        }

        Commands::List => {
            // a3s ecosystem tools
            let tools = [
                ("box", "a3s-box", "A3S-Lab/Box"),
                ("gateway", "a3s-gateway", "A3S-Lab/Gateway"),
                ("power", "a3s-power", "A3S-Lab/Power"),
            ];

            println!(
                "{:<12} {:<16} {}",
                "TOOL".bold(),
                "BINARY".bold(),
                "STATUS".bold()
            );
            println!("{}", "─".repeat(44).dimmed());
            for (alias, binary, _repo) in &tools {
                let installed = which_binary(binary);
                let status = if installed {
                    "installed".green().to_string()
                } else {
                    "not installed".dimmed().to_string()
                };
                println!("{:<12} {:<16} {}", alias, binary, status);
            }
        }

        Commands::Update { tools: filter } => {
            let all_tools = [
                ("box", "a3s-box", "A3S-Lab", "Box"),
                ("gateway", "a3s-gateway", "A3S-Lab", "Gateway"),
                ("power", "a3s-power", "A3S-Lab", "Power"),
                ("a3s", "a3s", "A3S-Lab", "Dev"),
            ];
            let targets: Vec<_> = if filter.is_empty() {
                all_tools.iter().collect()
            } else {
                all_tools
                    .iter()
                    .filter(|(alias, binary, _, _)| {
                        filter.iter().any(|f| f == alias || f == binary)
                    })
                    .collect()
            };
            if targets.is_empty() {
                return Err(DevError::Config(format!(
                    "unknown tool(s): {} — available: box, gateway, power, a3s",
                    filter.join(", ")
                )));
            }
            for (_, binary, owner, repo) in targets {
                let current = if *binary == "a3s" {
                    env!("CARGO_PKG_VERSION")
                } else {
                    "0.0.0"
                };
                if *binary != "a3s" && !which_binary(binary) {
                    println!(
                        "  {} {} not installed, skipping",
                        "·".dimmed(),
                        binary.dimmed()
                    );
                    continue;
                }
                println!("{} updating {}...", "→".cyan(), binary.cyan());
                let config = a3s_updater::UpdateConfig {
                    binary_name: binary,
                    crate_name: binary,
                    current_version: current,
                    github_owner: owner,
                    github_repo: repo,
                };
                match a3s_updater::run_update(&config).await {
                    Ok(_) => println!("{} {} updated", "✓".green(), binary.cyan()),
                    Err(e) => println!("{} {} — {}", "✗".red(), binary.cyan(), e),
                }
            }
        }

        Commands::Tool(args) => {
            let tool = &args[0];
            let rest = &args[1..];
            proxy_tool(tool, rest).await?;
        }

        Commands::Run { service, cmd } => {
            let cfg = DevConfig::from_file(&cli.file)?;
            let env: std::collections::HashMap<String, String> = if let Some(svc_name) = service {
                let svc = cfg
                    .service
                    .get(svc_name.as_str())
                    .ok_or_else(|| DevError::Config(format!("unknown service '{svc_name}'")))?;
                svc.env.clone()
            } else {
                // Merge all non-disabled services' env (later services win on conflict)
                cfg.service
                    .values()
                    .filter(|s| !s.disabled)
                    .flat_map(|s| s.env.iter().map(|(k, v)| (k.clone(), v.clone())))
                    .collect()
            };

            use std::os::unix::process::CommandExt;
            let err = std::process::Command::new(&cmd[0])
                .args(&cmd[1..])
                .envs(&env)
                .exec();
            return Err(DevError::Process {
                service: cmd[0].clone(),
                msg: err.to_string(),
            });
        }

        Commands::Exec { service, cmd } => {
            let cfg = DevConfig::from_file(&cli.file)?;
            let svc = cfg
                .service
                .get(service.as_str())
                .ok_or_else(|| DevError::Config(format!("unknown service '{service}'")))?;
            use std::os::unix::process::CommandExt;
            let mut command = std::process::Command::new(&cmd[0]);
            command.args(&cmd[1..]).envs(&svc.env);
            if let Some(dir) = &svc.dir {
                command.current_dir(dir);
            }
            let err = command.exec();
            return Err(DevError::Process {
                service: cmd[0].clone(),
                msg: err.to_string(),
            });
        }
    }

    Ok(())
}

fn format_uptime(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    }
}
fn which_binary(name: &str) -> bool {
    std::process::Command::new("which")
        .arg(name)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Known a3s ecosystem tools: alias -> (binary, github_owner, github_repo)
fn ecosystem_tool(alias: &str) -> Option<(&'static str, &'static str, &'static str)> {
    match alias {
        "box" => Some(("a3s-box", "A3S-Lab", "Box")),
        "gateway" => Some(("a3s-gateway", "A3S-Lab", "Gateway")),
        "power" => Some(("a3s-power", "A3S-Lab", "Power")),
        _ => None,
    }
}

/// Proxy a command to an a3s ecosystem tool, auto-installing if missing.
async fn proxy_tool(alias: &str, args: &[String]) -> Result<()> {
    let (binary, owner, repo) = ecosystem_tool(alias).ok_or_else(|| {
        DevError::Config(format!(
            "unknown tool '{alias}' — run `a3s list` to see available tools"
        ))
    })?;

    if !which_binary(binary) {
        println!(
            "{} {} not found — installing from {}/{}...",
            "→".cyan(),
            binary.cyan(),
            owner,
            repo
        );
        let config = a3s_updater::UpdateConfig {
            binary_name: binary,
            crate_name: binary,
            current_version: "0.0.0", // force install
            github_owner: owner,
            github_repo: repo,
        };
        a3s_updater::run_update(&config)
            .await
            .map_err(|e| DevError::Config(format!("failed to install {binary}: {e}")))?;
    }

    // Replace current process with the tool
    use std::os::unix::process::CommandExt;
    let err = std::process::Command::new(binary).args(args).exec();
    Err(DevError::Process {
        service: binary.to_string(),
        msg: err.to_string(),
    })
}

/// Poll the daemon via IPC until all services are healthy or the timeout expires.
/// Used by `a3s up --detach --wait`.
async fn wait_for_healthy(sock: &std::path::Path, timeout_secs: u64) -> Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

    // Wait for socket to appear (daemon may still be starting)
    loop {
        if sock.exists() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            return Err(DevError::Config(
                "timeout: daemon did not start in time".into(),
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    loop {
        if std::time::Instant::now() >= deadline {
            return Err(DevError::Config(
                "timeout: services did not become healthy in time".into(),
            ));
        }
        if let Ok(IpcResponse::Status { rows }) = ipc_send(IpcRequest::Status, sock).await {
            if rows.iter().any(|r| r.state == "failed") {
                return Err(DevError::Config(
                    "one or more services failed to start".into(),
                ));
            }
            let all_settled = rows
                .iter()
                .all(|r| matches!(r.state.as_str(), "running" | "stopped" | "failed"));
            if all_settled {
                return Ok(());
            }
        }
        // socket not ready yet or non-Status response — retry
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

async fn ipc_send(req: IpcRequest, sock: &std::path::Path) -> Result<IpcResponse> {
    let stream = UnixStream::connect(sock)
        .await
        .map_err(|_| DevError::Config("no running a3s daemon — run `a3s up` first".into()))?;

    let (reader, mut writer) = tokio::io::split(stream);
    let line = serde_json::to_string(&req)
        .map_err(|e| DevError::Config(format!("IPC serialize error: {e}")))?;
    writer.write_all(format!("{line}\n").as_bytes()).await?;

    let mut lines = BufReader::new(reader).lines();
    let resp_line = lines
        .next_line()
        .await?
        .ok_or_else(|| DevError::Config("daemon closed connection".into()))?;

    serde_json::from_str(&resp_line).map_err(|e| DevError::Config(format!("bad IPC response: {e}")))
}

async fn stream_logs(
    service: Option<String>,
    follow: bool,
    grep: Option<String>,
    last: usize,
    sock: &std::path::Path,
) -> Result<()> {
    // First replay history
    {
        let stream = UnixStream::connect(sock)
            .await
            .map_err(|_| DevError::Config("no running a3s daemon — run `a3s up` first".into()))?;
        let (reader, mut writer) = tokio::io::split(stream);
        let req = IpcRequest::History {
            service: service.clone(),
            lines: last,
        };
        writer
            .write_all(
                format!(
                    "{}\n",
                    serde_json::to_string(&req)
                        .map_err(|e| DevError::Config(format!("IPC serialize error: {e}")))?
                )
                .as_bytes(),
            )
            .await?;
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Ok(IpcResponse::LogLine {
                service: svc,
                line: text,
            }) = serde_json::from_str::<IpcResponse>(&line)
            {
                if grep
                    .as_deref()
                    .map_or(true, |g| text.to_lowercase().contains(&g.to_lowercase()))
                {
                    println!("{} {}", format!("[{svc}]").dimmed(), text);
                }
            }
        }
    }

    if !follow {
        return Ok(());
    }

    // Then stream live
    let stream = UnixStream::connect(sock)
        .await
        .map_err(|_| DevError::Config("no running a3s daemon — run `a3s up` first".into()))?;
    let (reader, mut writer) = tokio::io::split(stream);
    let req = IpcRequest::Logs {
        service,
        follow: true,
    };
    writer
        .write_all(
            format!(
                "{}\n",
                serde_json::to_string(&req)
                    .map_err(|e| DevError::Config(format!("IPC serialize error: {e}")))?
            )
            .as_bytes(),
        )
        .await?;

    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if let Ok(IpcResponse::LogLine {
            service,
            line: text,
        }) = serde_json::from_str::<IpcResponse>(&line)
        {
            if grep
                .as_deref()
                .map_or(true, |g| text.to_lowercase().contains(&g.to_lowercase()))
            {
                println!("{} {}", format!("[{service}]").cyan(), text);
            }
        }
    }

    Ok(())
}

/// Check whether a TCP port is available to bind on localhost.
fn port_available(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// Query CPU% and RSS memory (in bytes) for a process by PID using `ps`.
/// Returns `None` if the process is not found or `ps` is unavailable.
fn query_process_stats(pid: u32) -> Option<(f32, u64)> {
    let output = std::process::Command::new("ps")
        .args(["-o", "%cpu=,rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().find(|l| !l.trim().is_empty())?;
    let mut parts = line.split_whitespace();
    let cpu: f32 = parts.next()?.parse().ok()?;
    let rss_kb: u64 = parts.next()?.parse().ok()?;
    Some((cpu, rss_kb * 1024))
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 * 1024 {
        format!("{} KB", bytes / 1024)
    } else {
        format!("{:.1} MB", bytes as f64 / 1024.0 / 1024.0)
    }
}

/// Detect the dominant project type by inspecting `dir` for well-known files.
fn detect_project_type_in(dir: &std::path::Path) -> &'static str {
    if dir.join("package.json").exists() {
        "node"
    } else if dir.join("Cargo.toml").exists() {
        "rust"
    } else if dir.join("go.mod").exists() {
        "go"
    } else if dir.join("pyproject.toml").exists() || dir.join("requirements.txt").exists() {
        "python"
    } else {
        "generic"
    }
}

fn detect_project_type() -> &'static str {
    let cwd = std::env::current_dir().unwrap_or_default();
    detect_project_type_in(&cwd)
}

fn init_template(project_type: &str) -> &'static str {
    match project_type {
        "node" => INIT_TEMPLATE_NODE,
        "rust" => INIT_TEMPLATE_RUST,
        "go" => INIT_TEMPLATE_GO,
        "python" => INIT_TEMPLATE_PYTHON,
        _ => INIT_TEMPLATE,
    }
}

const INIT_TEMPLATE: &str = include_str!("A3sfile.hcl");

const INIT_TEMPLATE_NODE: &str = r#"# A3sfile.hcl — generated by `a3s init` (Node.js project)
# Run `a3s up` to start all services.

dev {
  proxy_port = 7080
}

service "web" {
  cmd       = "npm run dev"
  port      = 3000
  subdomain = "web"

  watch {
    paths  = ["src", "public"]
    ignore = ["node_modules", ".next", "dist", "build"]
  }

  health {
    type     = "http"
    path     = "/"
    interval = "2s"
    timeout  = "2s"
    retries  = 10
  }
}
"#;

const INIT_TEMPLATE_RUST: &str = r#"# A3sfile.hcl — generated by `a3s init` (Rust project)
# Run `a3s up` to start all services.

dev {
  proxy_port = 7080
}

service "api" {
  cmd       = "cargo run"
  port      = 3000
  subdomain = "api"

  watch {
    paths  = ["src"]
    ignore = ["target"]
  }

  health {
    type     = "http"
    path     = "/health"
    interval = "2s"
    timeout  = "1s"
    retries  = 10
  }
}
"#;

const INIT_TEMPLATE_GO: &str = r#"# A3sfile.hcl — generated by `a3s init` (Go project)
# Run `a3s up` to start all services.

dev {
  proxy_port = 7080
}

service "api" {
  cmd       = "go run ."
  port      = 3000
  subdomain = "api"

  watch {
    paths  = ["."]
    ignore = ["vendor"]
  }

  health {
    type     = "http"
    path     = "/health"
    interval = "2s"
    timeout  = "1s"
    retries  = 10
  }
}
"#;

const INIT_TEMPLATE_PYTHON: &str = r#"# A3sfile.hcl — generated by `a3s init` (Python project)
# Run `a3s up` to start all services.

dev {
  proxy_port = 7080
}

service "api" {
  cmd       = "uvicorn main:app --reload --port ${PORT}"
  port      = 8000
  subdomain = "api"

  watch {
    paths  = ["."]
    ignore = ["__pycache__", ".venv", "*.pyc"]
  }

  health {
    type     = "http"
    path     = "/health"
    interval = "2s"
    timeout  = "1s"
    retries  = 10
  }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_port_available_free_port() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        assert!(port_available(port));
    }

    #[test]
    fn test_port_unavailable_when_bound() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(!port_available(port));
        drop(listener);
    }

    #[test]
    fn test_format_bytes_kb() {
        assert_eq!(format_bytes(512 * 1024), "512 KB");
    }

    #[test]
    fn test_format_bytes_mb() {
        assert_eq!(format_bytes(10 * 1024 * 1024), "10.0 MB");
    }

    #[test]
    fn test_detect_project_type_generic_in_tempdir() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(detect_project_type_in(dir.path()), "generic");
    }

    #[test]
    fn test_detect_project_type_node() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        assert_eq!(detect_project_type_in(dir.path()), "node");
    }

    #[test]
    fn test_detect_project_type_rust() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        assert_eq!(detect_project_type_in(dir.path()), "rust");
    }

    #[test]
    fn test_detect_project_type_go() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), "module example.com/app").unwrap();
        assert_eq!(detect_project_type_in(dir.path()), "go");
    }

    #[test]
    fn test_detect_project_type_python() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("requirements.txt"), "fastapi\n").unwrap();
        assert_eq!(detect_project_type_in(dir.path()), "python");
    }

    #[test]
    fn test_init_template_returns_correct_template() {
        assert!(init_template("node").contains("npm run dev"));
        assert!(init_template("rust").contains("cargo run"));
        assert!(init_template("go").contains("go run"));
        assert!(init_template("python").contains("uvicorn"));
        assert!(init_template("generic").contains("a3s init"));
    }
}
