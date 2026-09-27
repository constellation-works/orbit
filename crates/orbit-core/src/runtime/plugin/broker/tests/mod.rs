mod peer;
mod protocol;
mod sandbox;
mod server;
mod socket;

/// Keep broker socket fixtures inside the Unix socket path limit on macOS.
fn short_tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("obk")
        .tempdir_in("/tmp")
        .expect("short broker test root")
}
