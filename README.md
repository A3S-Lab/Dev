# a3s

Local development orchestration tool for the A3S monorepo — and a unified CLI for the entire A3S ecosystem.

## What it does

`a3s` is a single binary that replaces the need to juggle multiple terminals, process managers, and tool installers when working on A3S projects. It:

- Starts and supervises multiple services defined in `A3sfile.hcl`
- Manages Homebrew dependencies declaratively
- Proxies to A3S ecosystem tools (`a3s box`, `a3s gateway`, etc.), auto-installing them if missing
- Deploys a local k3s Kubernetes cluster (Lima VM on macOS, systemd on Linux)
- Scaffolds new `a3s-code` agent projects
- Provides a web UI for real-time service monitoring

## Install

```bash
brew install a3s-lab/tap/a3s
```

Or build from source:

```bash
cargo build --release -p a3s
```

## Quick start

```bash
# Create an A3sfile.hcl in the current directory
a3s init

# Edit A3sfile.hcl, then start all services
a3s up

# Start in background
a3s up --detach

# Check status
a3s status

# Tail logs
a3s logs
a3s logs --service api

# Stop everything
a3s down
```

## A3sfile.hcl

```hcl
dev {
  proxy_port = 7080
  log_level  = "info"
}

brew {
  packages = ["redis", "postgresql@16"]
}

service "api" {
  cmd        = "cargo run -p my-api"
  dir        = "./services/api"
  port       = 3000
  subdomain  = "api"
  depends_on = ["db"]

  env = {
    DATABASE_URL = "postgres://localhost:5432/dev"
  }

  watch {
    paths   = ["./services/api/src"]
    ignore  = ["target"]
    restart = true
  }

  health {
    type     = "http"
    path     = "/health"
    interval = "2s"
    timeout  = "1s"
    retries  = 5
  }
}
```

## Commands

### Service orchestration

| Command | Description |
|---------|-------------|
| `a3s up [services]` | Start all (or named) services in dependency order |
| `a3s up --detach` | Start as background daemon |
| `a3s down [services]` | Stop all (or named) services |
| `a3s restart <service>` | Restart a service |
| `a3s status` | Show service status table |
| `a3s logs [--service name]` | Tail logs (all or one service) |
| `a3s validate` | Validate A3sfile.hcl without starting anything |
| `a3s init` | Generate a new A3sfile.hcl |

### Package management

| Command | Description |
|---------|-------------|
| `a3s add <pkg>` | Add a Homebrew package to A3sfile.hcl and install it |
| `a3s remove <pkg>` | Remove a Homebrew package from A3sfile.hcl and uninstall it |
| `a3s search <query>` | Search Homebrew packages |
| `a3s install` | Install all brew packages declared in A3sfile.hcl |
| `a3s list` | List all installed A3S tools and brew packages |

### Kubernetes

| Command | Description |
|---------|-------------|
| `a3s kube start` | Install and start a local k3s cluster |
| `a3s kube stop` | Stop the local k3s cluster |
| `a3s kube status` | Show k3s cluster status |

On macOS, uses [Lima](https://lima-vm.io/) with the `template://k3s` template. On Linux, uses the official k3s install script + systemd. Kubeconfig is written to `~/.kube/config` automatically.

### A3S ecosystem tools

`a3s` acts as a unified entry point for all A3S tools. If a tool is not installed, it is downloaded automatically from GitHub Releases.

```bash
a3s box run ubuntu:24.04 -- bash
a3s box ps
a3s gateway --help
```

### Agent scaffolding

```bash
# Scaffold a new a3s-code agent project
a3s code init ./my-agent
a3s code init --dir ./my-agent  # Python or TypeScript
```

### Self-update

```bash
a3s upgrade
```

## Web UI

When running `a3s up`, a web UI is available at `http://localhost:10350` by default. It shows real-time service status, logs, and health. Disable with `--no-ui` or change the port with `--ui-port`.

## Proxy routing

Services with a `subdomain` field are accessible at `http://<subdomain>.localhost:<proxy_port>`. The proxy runs on port 7080 by default.

## Configuration reference

```hcl
dev {
  proxy_port = 7080      # Local reverse proxy port
  log_level  = "info"    # Logging level: trace, debug, info, warn, error
}

brew {
  packages = ["redis"]   # Homebrew packages to install before `a3s up`
}

service "<name>" {
  cmd        = "..."     # Shell command to run
  dir        = "."       # Working directory (default: current)
  port       = 3000      # Port the service listens on (0 = auto-detect)
  subdomain  = "api"     # Proxy subdomain (optional)
  depends_on = ["db"]    # Services to start first (optional)

  env = {                # Environment variables (optional)
    KEY = "value"
  }

  watch {                # File watcher — restart on change (optional)
    paths   = ["./src"]
    ignore  = ["target", "node_modules"]
    restart = true
  }

  health {               # Health check (optional)
    type     = "http"    # http or tcp
    path     = "/health" # HTTP path (http only)
    interval = "2s"
    timeout  = "1s"
    retries  = 5
  }
}
```

## License

MIT — see [LICENSE](LICENSE).
