#[cfg(feature = "native")]
fn main() {
    // Declaring the app's commands puts them under the ACL, so only windows a
    // capability names can call them. The dashboard page is a remote origin
    // and no capability grants it any command.
    let manifest = tauri_build::AppManifest::new().commands(&["status", "retry"]);
    if let Err(error) =
        tauri_build::try_build(tauri_build::Attributes::new().app_manifest(manifest))
    {
        panic!("tauri build: {error:#}");
    }
}

#[cfg(not(feature = "native"))]
fn main() {}
