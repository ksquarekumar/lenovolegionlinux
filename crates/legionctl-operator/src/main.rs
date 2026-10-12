//! `legionctl-operator`: the user's legionctl engine. It serves the operation registry of `legionctl-api` on
//! `$XDG_RUNTIME_DIR/legionctl/api.sock` (socket-activated) and, in a graphical session, runs the session module
//! (Fn+R, OSD).

mod display;
mod engine;
mod server;
mod session;

use std::sync::Arc;

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    let listener = server::activated_listener()?;
    let operator = Arc::new(engine::Operator {
        runtime: tokio::runtime::Handle::current(),
    });
    tokio::spawn(async {
        if let Err(e) = session::run().await {
            eprintln!("operator: session module stopped: {e:#}");
        }
    });
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    axum::serve(listener, server::router(operator))
        .with_graceful_shutdown(async move {
            term.recv().await;
        })
        .await?;
    Ok(())
}
