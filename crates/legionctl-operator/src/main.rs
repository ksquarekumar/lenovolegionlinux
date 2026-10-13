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

/// Set by a module once it runs; until then a stop means it cannot run here (no bus, a port in use).
#[derive(Clone, Default)]
pub struct Ready(Arc<std::sync::atomic::AtomicBool>);

impl Ready {
    pub fn set(&self) {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    fn is_set(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// The operator's resident modules. One that cannot run here is logged and left out; one that panics or stops after
/// it ran ends the process with an error, and systemd starts it again (the socket keeps clients waiting).
async fn supervise(
    modules: Vec<(&'static str, Ready, tokio::task::JoinHandle<Result<()>>)>,
) -> Result<()> {
    use futures_util::StreamExt;
    // each module's end comes with its own name and ready flag
    let mut ends: futures_util::stream::FuturesUnordered<_> = modules
        .into_iter()
        .map(|(name, ready, handle)| async move { (name, ready, handle.await) })
        .collect();
    while let Some((name, ready, ended)) = ends.next().await {
        match ended {
            Err(e) if e.is_panic() => anyhow::bail!("{name} panicked: {e}"),
            Err(e) => anyhow::bail!("{name} was cancelled: {e}"),
            Ok(result) if ready.is_set() => anyhow::bail!(
                "{name} stopped while it ran: {}",
                result
                    .err()
                    .map_or_else(|| "it ended".into(), |e| format!("{e:#}"))
            ),
            Ok(Ok(())) => eprintln!("operator: {name} is off"),
            Ok(Err(e)) => eprintln!("operator: {name} cannot run here: {e:#}"),
        }
    }
    std::future::pending().await
}

/// Start a resident module with its ready flag.
fn module<F>(
    name: &'static str,
    run: impl FnOnce(Ready) -> F,
) -> (&'static str, Ready, tokio::task::JoinHandle<Result<()>>)
where
    F: Future<Output = Result<()>> + Send + 'static,
{
    let ready = Ready::default();
    (name, ready.clone(), tokio::spawn(run(ready)))
}

/// The model gateway, when `gateway.port` is set.
async fn gateway(ready: Ready) -> Result<()> {
    let port = legionctl_core::config::resolve(&legionctl_core::config::Dirs::from_env(), None)?
        .config
        .gateway
        .port;
    if port == 0 {
        return Ok(());
    }
    gateway::serve(port, ready).await
}

#[tokio::main]
async fn main() -> Result<()> {
    let listener = server::activated_listener()?;
    let operator = Arc::new(engine::Operator {
        runtime: tokio::runtime::Handle::current(),
    });
    // retention (`events.keep_days`): the log and gateway usage lose their old rows at each start, before the first
    // call reads them
    let pruned = tokio::task::spawn_blocking(|| {
        let days = legionctl_core::config::resolve(&legionctl_core::config::Dirs::from_env(), None)
            .map(|r| r.config.events.keep_days);
        match days.and_then(|days| legionctl_core::db::Db::open_default()?.prune(days)) {
            Ok(0) => {}
            Ok(rows) => eprintln!("operator: {rows} old log rows removed (events.keep_days)"),
            Err(e) => eprintln!("operator: old log rows not removed: {e:#}"),
        }
        // account logins left in profiles (a new login, a session that ended) go into the keyring
        let settled =
            legionctl_core::config::resolve(&legionctl_core::config::Dirs::from_env(), None)
                .and_then(|r| {
                    legionctl_core::dossier::Dossiers::open(&r.config.dossier)?.settle_logins()
                });
        if let Err(e) = settled {
            eprintln!("operator: account logins not moved to the keyring: {e:#}");
        }
    })
    .await;
    if let Err(e) = pruned {
        eprintln!("operator: old log rows not removed: {e}");
    }
    let modules = vec![
        module("the machine event recorder", recorder::run),
        module("the session module", session::run),
        module("the model gateway", gateway),
    ];
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let api = axum::serve(listener, server::router(operator)).with_graceful_shutdown(async move {
        term.recv().await;
    });
    tokio::select! {
        served = api => Ok(served?),
        failed = supervise(modules) => failed,
    }
}
