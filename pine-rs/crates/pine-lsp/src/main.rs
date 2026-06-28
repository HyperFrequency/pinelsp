//! `pine-lsp` — Pine Script v6 language server over stdio (tower-lsp).

mod backend;
mod features;

use backend::Backend;
use tower_lsp::{LspService, Server};
use tracing_subscriber::EnvFilter;

/// Build the tracing subscriber.
///
/// CRITICAL: logs go to **stderr**, never stdout — stdout is the LSP JSON-RPC
/// wire and any stray byte there corrupts the protocol for every client.
///
/// Filtering precedence: `PINE_LOG` (server-specific) takes priority, then the
/// standard `RUST_LOG`, then a quiet default of `warn` so the common
/// no-env-vars case stays near-zero cost (debug/trace events are not even
/// formatted).
fn init_logging() {
    let env_filter = EnvFilter::try_from_env("PINE_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("warn"));

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        // No ANSI escapes so piped/captured logs stay clean.
        .with_ansi(false)
        .with_env_filter(env_filter)
        .init();
}

/// Route panics (including those caught by `catch_unwind` in the backend, which
/// re-run the hook) through `tracing::error!` to stderr, so a panic anywhere is
/// observable in the same log stream as everything else. Kept trivial and
/// stderr-only: a panic hook that itself panics — or writes to stdout — could
/// corrupt the wire or deadlock.
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_string());
        tracing::error!(location = %location, message = %message, "panic");
    }));
}

#[tokio::main]
async fn main() {
    init_logging();
    install_panic_hook();

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
