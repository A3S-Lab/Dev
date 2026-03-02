use colored::Colorize;

use crate::error::{DevError, Result};

/// Install and start k3s (lightweight Kubernetes).
/// Requires root/sudo on Linux. On macOS, uses a Lima VM via `limactl`.
pub async fn start() -> Result<()> {
    #[cfg(target_os = "macos")]
    return start_macos().await;

    #[cfg(target_os = "linux")]
    return start_linux().await;

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    Err(DevError::Config(
        "kube is only supported on macOS and Linux".into(),
    ))
}

/// Stop and clean up k3s.
pub async fn stop() -> Result<()> {
    #[cfg(target_os = "macos")]
    return stop_macos().await;

    #[cfg(target_os = "linux")]
    return stop_linux().await;

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    Err(DevError::Config(
        "kube is only supported on macOS and Linux".into(),
    ))
}

/// Show k3s status.
pub async fn status() -> Result<()> {
    #[cfg(target_os = "macos")]
    return status_macos().await;

    #[cfg(target_os = "linux")]
    return status_linux().await;

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        println!(
            "  {} kube status is only supported on macOS and Linux",
            "·".dimmed()
        );
        Ok(())
    }
}

// ── macOS: Lima VM ────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
async fn start_macos() -> Result<()> {
    if !cmd_exists("limactl").await {
        println!(
            "  {} limactl not found — installing via Homebrew...",
            "→".cyan()
        );
        run("brew", &["install", "lima"]).await?;
    }

    let list = tokio::process::Command::new("limactl")
        .args(["list", "--format", "{{.Name}}"])
        .output()
        .await
        .map_err(DevError::Io)?;
    let existing = String::from_utf8_lossy(&list.stdout);

    if existing.lines().any(|l| l.trim() == "k3s") {
        println!(
            "  {} k3s VM already exists — starting in background...",
            "→".cyan()
        );
    } else {
        println!("  {} creating k3s Lima VM in background...", "→".cyan());
    }

    // Spawn limactl detached so the command returns immediately.
    let args: &[&str] = if existing.lines().any(|l| l.trim() == "k3s") {
        &["start", "k3s"]
    } else {
        &["start", "--name=k3s", "template://k3s"]
    };
    tokio::process::Command::new("limactl")
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| DevError::Config(format!("failed to spawn limactl: {e}")))?;

    println!(
        "  {} k3s starting in background. Run {} to check status.",
        "✓".green(),
        "a3s kube status".cyan()
    );
    Ok(())
}

#[cfg(target_os = "macos")]
async fn stop_macos() -> Result<()> {
    println!("  {} stopping k3s Lima VM...", "→".cyan());
    run("limactl", &["stop", "k3s"]).await?;
    println!("  {} k3s stopped.", "✓".green());
    Ok(())
}

#[cfg(target_os = "macos")]
async fn status_macos() -> Result<()> {
    if !cmd_exists("limactl").await {
        println!("  {} k3s  {}", "·".dimmed(), "not installed".dimmed());
        return Ok(());
    }

    let out = tokio::process::Command::new("limactl")
        .args(["list", "--format", "{{.Name}} {{.Status}}"])
        .output()
        .await
        .map_err(DevError::Io)?;

    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout.lines().find(|l| l.starts_with("k3s "));

    match line {
        Some(l) if l.contains("Running") => {
            println!("  {} k3s  {}", "●".green(), "running".green())
        }
        Some(l) if l.contains("Stopped") => {
            println!("  {} k3s  {}", "○".yellow(), "stopped".yellow())
        }
        Some(_) => println!("  {} k3s  {}", "○".dimmed(), "exists".dimmed()),
        None => println!("  {} k3s  {}", "·".dimmed(), "not installed".dimmed()),
    }
    Ok(())
}

#[cfg(target_os = "macos")]
async fn merge_kubeconfig_macos() -> Result<()> {
    let home = dirs_next::home_dir()
        .ok_or_else(|| DevError::Config("cannot determine home directory".into()))?;
    let kube_dir = home.join(".kube");
    tokio::fs::create_dir_all(&kube_dir)
        .await
        .map_err(DevError::Io)?;

    let kubeconfig_path = kube_dir.join("config");
    let kubeconfig_str = kubeconfig_path
        .to_str()
        .ok_or_else(|| DevError::Config("kubeconfig path contains non-UTF8 characters".into()))?;

    let output = tokio::process::Command::new("limactl")
        .args(["shell", "k3s", "sudo", "cat", "/etc/rancher/k3s/k3s.yaml"])
        .output()
        .await
        .map_err(DevError::Io)?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(DevError::Config(format!(
            "failed to read k3s kubeconfig: {err}"
        )));
    }

    let raw = String::from_utf8_lossy(&output.stdout);
    // Lima forwards port 6443 to 127.0.0.1 on the host — use localhost directly.
    let patched = raw.replace("default", "k3s");
    tokio::fs::write(&kubeconfig_path, patched.as_bytes())
        .await
        .map_err(DevError::Io)?;

    println!(
        "  {} kubeconfig written to {}",
        "✓".green(),
        kubeconfig_str.dimmed()
    );
    Ok(())
}

// ── Linux: native k3s ────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
async fn start_linux() -> Result<()> {
    if !cmd_exists("k3s").await {
        println!("  {} k3s not found — installing...", "→".cyan());
        install_k3s_linux().await?;
    }

    let status = tokio::process::Command::new("systemctl")
        .args(["is-active", "--quiet", "k3s"])
        .status()
        .await
        .map_err(DevError::Io)?;

    if status.success() {
        println!("  {} k3s is already running.", "✓".green());
        return Ok(());
    }

    println!("  {} starting k3s service...", "→".cyan());
    run("sudo", &["systemctl", "start", "k3s"]).await?;
    run("sudo", &["systemctl", "enable", "k3s"]).await?;

    let home = dirs_next::home_dir()
        .ok_or_else(|| DevError::Config("cannot determine home directory".into()))?;
    let kube_dir = home.join(".kube");
    tokio::fs::create_dir_all(&kube_dir)
        .await
        .map_err(DevError::Io)?;
    run(
        "sudo",
        &[
            "cp",
            "/etc/rancher/k3s/k3s.yaml",
            kube_dir.join("config").to_str().unwrap_or("/tmp/k3s.yaml"),
        ],
    )
    .await?;
    run(
        "sudo",
        &[
            "chown",
            &format!(
                "{}:{}",
                std::env::var("USER").unwrap_or_default(),
                std::env::var("USER").unwrap_or_default()
            ),
            kube_dir.join("config").to_str().unwrap_or("/tmp/k3s.yaml"),
        ],
    )
    .await?;

    println!(
        "  {} k3s is running. Use {} to interact with the cluster.",
        "✓".green(),
        "kubectl".cyan()
    );
    Ok(())
}

#[cfg(target_os = "linux")]
async fn install_k3s_linux() -> Result<()> {
    let script = tokio::process::Command::new("sh")
        .args(["-c", "curl -sfL https://get.k3s.io | sh -"])
        .status()
        .await
        .map_err(DevError::Io)?;

    if !script.success() {
        return Err(DevError::Config("k3s installation failed".into()));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
async fn stop_linux() -> Result<()> {
    println!("  {} stopping k3s service...", "→".cyan());
    run("sudo", &["systemctl", "stop", "k3s"]).await?;

    if tokio::fs::metadata("/usr/local/bin/k3s-killall.sh")
        .await
        .is_ok()
    {
        println!("  {} running k3s-killall.sh...", "→".cyan());
        run("sudo", &["/usr/local/bin/k3s-killall.sh"]).await?;
    }

    println!("  {} k3s stopped.", "✓".green());
    Ok(())
}

#[cfg(target_os = "linux")]
async fn status_linux() -> Result<()> {
    if !cmd_exists("k3s").await {
        println!("  {} k3s  {}", "·".dimmed(), "not installed".dimmed());
        return Ok(());
    }

    let active = tokio::process::Command::new("systemctl")
        .args(["is-active", "--quiet", "k3s"])
        .status()
        .await
        .map_err(DevError::Io)?;

    if active.success() {
        println!("  {} k3s  {}", "●".green(), "running".green());
    } else {
        println!("  {} k3s  {}", "○".yellow(), "stopped".yellow());
    }
    Ok(())
}

// ── Structured status (for API) ───────────────────────────────────────────────

#[derive(serde::Serialize)]
pub struct KubeStatus {
    pub installed: bool,
    pub running: bool,
    /// "running" | "stopped" | "not_installed"
    pub state: String,
}

/// Return structured kube status for the web UI.
pub async fn query_status() -> KubeStatus {
    #[cfg(target_os = "macos")]
    return query_status_macos().await;

    #[cfg(target_os = "linux")]
    return query_status_linux().await;

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    KubeStatus {
        installed: false,
        running: false,
        state: "not_installed".into(),
    }
}

#[cfg(target_os = "macos")]
async fn query_status_macos() -> KubeStatus {
    if !cmd_exists("limactl").await {
        return KubeStatus { installed: false, running: false, state: "not_installed".into() };
    }
    let out = tokio::process::Command::new("limactl")
        .args(["list", "--format", "{{.Name}} {{.Status}}"])
        .output()
        .await;
    let Ok(out) = out else {
        return KubeStatus { installed: false, running: false, state: "not_installed".into() };
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout.lines().find(|l| l.starts_with("k3s "));
    match line {
        Some(l) if l.contains("Running") => KubeStatus { installed: true, running: true, state: "running".into() },
        Some(_) => KubeStatus { installed: true, running: false, state: "stopped".into() },
        None => KubeStatus { installed: false, running: false, state: "not_installed".into() },
    }
}

#[cfg(target_os = "linux")]
async fn query_status_linux() -> KubeStatus {
    if !cmd_exists("k3s").await {
        return KubeStatus { installed: false, running: false, state: "not_installed".into() };
    }
    let active = tokio::process::Command::new("systemctl")
        .args(["is-active", "--quiet", "k3s"])
        .status()
        .await;
    match active {
        Ok(s) if s.success() => KubeStatus { installed: true, running: true, state: "running".into() },
        _ => KubeStatus { installed: true, running: false, state: "stopped".into() },
    }
}

// ── Resource queries (for web UI) ─────────────────────────────────────────────

#[derive(serde::Serialize)]
pub struct KubeResources {
    pub namespaces: Vec<String>,
    pub nodes: Vec<KubeNode>,
    pub pods: Vec<KubePod>,
}

#[derive(serde::Serialize)]
pub struct KubeNode {
    pub name: String,
    pub status: String,
    pub roles: String,
    pub version: String,
}

#[derive(serde::Serialize)]
pub struct KubePod {
    pub name: String,
    pub namespace: String,
    pub status: String,
    pub ready: String,
    pub restarts: u32,
    pub age: String,
    pub node: String,
}

/// Fetch cluster resources via kubectl. `namespace` = None means all namespaces.
pub async fn query_resources(namespace: Option<&str>) -> Result<KubeResources> {
    let kubectl = kubectl_cmd().await;
    let namespaces = get_namespaces(&kubectl).await?;
    let nodes = get_nodes(&kubectl).await?;
    let pods = get_pods(&kubectl, namespace).await?;
    Ok(KubeResources { namespaces, nodes, pods })
}

/// Fetch recent logs for a pod (tail N lines).
pub async fn pod_logs(namespace: &str, name: &str, tail: usize) -> Result<String> {
    let out = tokio::process::Command::new("kubectl")
        .args([
            "logs", name,
            "-n", namespace,
            &format!("--tail={tail}"),
            "--timestamps=true",
        ])
        .output()
        .await
        .map_err(DevError::Io)?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub async fn delete_pod(namespace: &str, name: &str) -> Result<()> {
    let kubectl = kubectl_cmd().await;
    let status = tokio::process::Command::new(&kubectl)
        .args(["delete", "pod", name, "-n", namespace, "--grace-period=0"])
        .status()
        .await
        .map_err(DevError::Io)?;
    if !status.success() {
        return Err(DevError::Config(format!("kubectl delete pod {name} failed")));
    }
    Ok(())
}

async fn kubectl_cmd() -> String {
    // On macOS with Lima, kubectl talks to the Lima-forwarded API server.
    // The kubeconfig is written to ~/.kube/config by start_macos / start_linux.
    "kubectl".into()
}

async fn get_namespaces(kubectl: &str) -> Result<Vec<String>> {
    let out = tokio::process::Command::new(kubectl)
        .args(["get", "namespaces", "-o", "jsonpath={.items[*].metadata.name}"])
        .output()
        .await
        .map_err(DevError::Io)?;
    if !out.status.success() {
        return Ok(vec![]);
    }
    let s = String::from_utf8_lossy(&out.stdout);
    Ok(s.split_whitespace().map(|s| s.to_string()).collect())
}

async fn get_nodes(kubectl: &str) -> Result<Vec<KubeNode>> {
    // Use JSON output for reliable parsing
    let out = tokio::process::Command::new(kubectl)
        .args(["get", "nodes", "-o", "json"])
        .output()
        .await
        .map_err(DevError::Io)?;
    if !out.status.success() {
        return Ok(vec![]);
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    let items = v["items"].as_array().map(|a| a.as_slice()).unwrap_or(&[]);
    Ok(items.iter().map(|item| {
        let name = item["metadata"]["name"].as_str().unwrap_or("").to_string();
        let version = item["status"]["nodeInfo"]["kubeletVersion"].as_str().unwrap_or("").to_string();
        let roles = item["metadata"]["labels"]
            .as_object()
            .map(|labels| {
                let mut r: Vec<&str> = labels.keys()
                    .filter_map(|k| k.strip_prefix("node-role.kubernetes.io/"))
                    .collect();
                r.sort();
                if r.is_empty() { "<none>".to_string() } else { r.join(",") }
            })
            .unwrap_or_else(|| "<none>".to_string());
        let status = item["status"]["conditions"].as_array()
            .and_then(|conds| conds.iter().find(|c| c["type"] == "Ready"))
            .and_then(|c| c["status"].as_str())
            .map(|s| if s == "True" { "Ready" } else { "NotReady" })
            .unwrap_or("Unknown")
            .to_string();
        KubeNode { name, status, roles, version }
    }).collect())
}

async fn get_pods(kubectl: &str, namespace: Option<&str>) -> Result<Vec<KubePod>> {
    let ns_args: Vec<&str> = match namespace {
        Some(ns) => vec!["-n", ns],
        None => vec!["--all-namespaces"],
    };
    let mut args = vec!["get", "pods"];
    args.extend_from_slice(&ns_args);
    args.extend_from_slice(&[
        "-o", "custom-columns=NAME:.metadata.name,NAMESPACE:.metadata.namespace,STATUS:.status.phase,READY:.status.containerStatuses[0].ready,RESTARTS:.status.containerStatuses[0].restartCount,NODE:.spec.nodeName",
        "--no-headers",
    ]);
    let out = tokio::process::Command::new(kubectl)
        .args(&args)
        .output()
        .await
        .map_err(DevError::Io)?;
    if !out.status.success() {
        return Ok(vec![]);
    }
    let s = String::from_utf8_lossy(&out.stdout);
    Ok(s.lines().filter(|l| !l.trim().is_empty()).map(|line| {
        let cols: Vec<&str> = line.split_whitespace().collect();
        KubePod {
            name: cols.first().unwrap_or(&"").to_string(),
            namespace: cols.get(1).unwrap_or(&"").to_string(),
            status: cols.get(2).unwrap_or(&"").to_string(),
            ready: cols.get(3).unwrap_or(&"false").to_string(),
            restarts: cols.get(4).and_then(|s| s.parse().ok()).unwrap_or(0),
            age: String::new(),
            node: cols.get(5).unwrap_or(&"").to_string(),
        }
    }).collect())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Run a command, streaming output to stdout, returning error on non-zero exit.
async fn run(program: &str, args: &[&str]) -> Result<()> {
    let status = tokio::process::Command::new(program)
        .args(args)
        .status()
        .await
        .map_err(|e| DevError::Config(format!("failed to run `{program}`: {e}")))?;

    if !status.success() {
        return Err(DevError::Config(format!(
            "`{program} {}` exited with {}",
            args.join(" "),
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "?".into())
        )));
    }
    Ok(())
}

/// Check if a command exists in PATH.
async fn cmd_exists(name: &str) -> bool {
    tokio::process::Command::new("which")
        .arg(name)
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_cmd_exists_true() {
        assert!(cmd_exists("sh").await);
    }

    #[tokio::test]
    async fn test_cmd_exists_false() {
        assert!(!cmd_exists("__nonexistent_binary_xyz__").await);
    }
}
