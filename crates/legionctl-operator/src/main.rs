//! `legionctl-operator`: the user's legionctl engine. It serves the operation registry of `legionctl-api` on
//! `$XDG_RUNTIME_DIR/legionctl/api.sock` (socket-activated) and, in a graphical session, runs the session module
//! (Fn+R, OSD).

mod acp;
mod approval;
mod display;
mod engine;
mod gateway;
mod recorder;
mod semantic;
mod server;
mod session;
mod system1;
mod translate;
mod virt;

use std::sync::Arc;

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    let listener = server::activated_listener()?;
    let operator = Arc::new(engine::Operator {
        runtime: tokio::runtime::Handle::current(),
    });
    tokio::spawn(async {
        match recorder::run().await {
            Ok(()) => {
                eprintln!("operator: machine event recorder ended (the daemon's signals stopped)");
            }
            Err(e) => eprintln!("operator: machine event recorder stopped: {e:#}"),
        }
    });
    tokio::spawn(async {
        if let Err(e) = session::run().await {
            eprintln!("operator: session module stopped: {e:#}");
        }
    });
    tokio::spawn(async {
        let port = legionctl_core::config::resolve(&legionctl_core::config::Dirs::from_env(), None)
            .map(|r| r.config.gateway.port);
        match port {
            Ok(0) => {}
            Ok(port) => {
                if let Err(e) = gateway::serve(port).await {
                    eprintln!("operator: model gateway stopped: {e:#}");
                }
            }
            Err(e) => eprintln!("operator: model gateway not started: {e:#}"),
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
