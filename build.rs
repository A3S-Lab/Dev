use std::process::Command;

fn main() {
    // Re-run only when UI sources actually change.
    println!("cargo:rerun-if-changed=src/ui/src");
    println!("cargo:rerun-if-changed=src/ui/index.html");
    println!("cargo:rerun-if-changed=src/ui/rsbuild.config.ts");
    println!("cargo:rerun-if-changed=src/ui/package.json");

    // Install node_modules on first build or after package.json changes.
    if !std::path::Path::new("src/ui/node_modules").exists() {
        let ok = Command::new("npm")
            .args(["install"])
            .current_dir("src/ui")
            .status()
            .expect("npm not found — install Node.js to build the UI")
            .success();
        assert!(ok, "npm install failed");
    }

    // Build the React UI → src/ui/dist/index.html
    let ok = Command::new("npm")
        .args(["run", "build"])
        .current_dir("src/ui")
        .status()
        .expect("npm not found")
        .success();
    assert!(ok, "npm run build failed");
}
