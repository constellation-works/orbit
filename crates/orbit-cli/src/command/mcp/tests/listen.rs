//! Argument surface for `orbit mcp listen`.

use std::path::PathBuf;

use clap::Parser;

use super::*;

/// Minimal parser so the subcommand's own arguments can be exercised without
/// the whole CLI tree.
#[derive(Parser)]
struct ListenCli {
    #[command(flatten)]
    args: ListenArgs,
}

fn parse(argv: &[&str]) -> ListenArgs {
    ListenCli::parse_from(argv).args
}

#[test]
fn listen_defaults_to_a_loopback_bind() {
    let args = parse(&["listen"]);
    assert_eq!(args.addr, DEFAULT_LISTEN_ADDR);
    assert!(args.addr.ip().is_loopback());
    assert_eq!(args.addr.port(), DEFAULT_MCP_LISTEN_PORT);
    assert!(!args.allow_non_loopback);
    assert_eq!(args.exposure(), ListenerExposure::LoopbackOnly);
}

#[test]
fn an_explicit_address_is_taken_positionally() {
    let args = parse(&["listen", "127.0.0.1:9123"]);
    assert_eq!(args.addr, "127.0.0.1:9123".parse::<SocketAddr>().unwrap());
    assert_eq!(args.exposure(), ListenerExposure::LoopbackOnly);
}

#[test]
fn a_wider_bind_requires_the_explicit_flag() {
    let args = parse(&["listen", "0.0.0.0:9123", "--allow-non-loopback"]);
    assert!(!args.addr.ip().is_loopback());
    assert_eq!(args.exposure(), ListenerExposure::AnyInterface);
}

#[test]
fn a_workspace_root_override_is_refused_before_any_socket_opens() {
    let error = parse(&["listen"])
        .execute_without_runtime(Some(&PathBuf::from("/tmp/does-not-matter")), None)
        .expect_err("root override must be refused");
    assert!(
        matches!(&error, OrbitError::InvalidInput(message) if message.contains("orbit mcp listen")),
        "{error:?}"
    );
}
