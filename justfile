default:
    @just --list

# ── UI ────────────────────────────────────────────────────────

# Install UI dependencies
ui-install:
    cd src/ui && npm install

# Build the React UI (outputs src/ui/dist/index.html, required before cargo build)
build-ui:
    cd src/ui && npm run build

# Start the React dev server (proxies /api to a running `a3s up` on :10350)
dev-ui:
    cd src/ui && npm run dev

# ── Rust ─────────────────────────────────────────────────────

run:
    cargo run -- up

build:
    cargo build --release

test:
    cargo test --lib

check:
    cargo check
    cargo clippy -- -D warnings

fmt:
    cargo fmt
