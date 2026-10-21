//! `legionctl-operator`: the user's legionctl engine. It serves the operation registry of `legionctl-api` on
//! `$XDG_RUNTIME_DIR/legionctl/api.sock` (socket-activated) and, in a graphical session, runs the session module
//! (Fn+R, OSD).

mod accounts;
mod acp;
mod calls;
mod code;
mod desktop;
mod display;
mod engine;
mod facts;
mod faults;
mod gateway;
mod kernels;
mod policy;
mod recorder;
mod resources;
mod rlcd;
mod rules;
mod scope;
mod semantic;
mod server;
mod session;
mod settings;
mod system1;
mod thread;
mod translate;
mod trust;
mod views;
mod virt;
mod wincare;

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

/// The model gateway's listeners, when `gateway.port` is set (`None`: off).
async fn gateway_bind() -> Result<Option<gateway::Bound>> {
    let port = legionctl_core::config::resolve(&legionctl_core::config::Dirs::from_env(), None)?
        .config
        .gateway
        .port;
    if port == 0 {
        return Ok(None);
    }
    Ok(Some(gateway::bind(port).await?))
}

fn main() -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let ended = runtime.block_on(run());
    // a stop waits for no blocking work (a job, an ACP run, a wake): a restart resumes what it cut off (todo-125)
    runtime.shutdown_background();
    ended
}

/// The fault of log retention that did not run.
const RETENTION: &str = "old log rows are not removed (events.keep_days)";

/// The fault of a job that a restart cut off and that does not go on (its row stays `running`).
pub(crate) const JOBS: &str = "jobs that a restart cut off are not resumed";

/// The operator until SIGTERM, or until a module fails.
async fn run() -> Result<()> {
    // while it runs: no process of another build steps the schema of a database it reads (todo-122)
    let _holder = legionctl_core::db::hold(&legionctl_core::db::default_path())?;
    let listener = server::activated_listener()?;
    trust::init(tokio::runtime::Handle::current());
    let operator = Arc::new(engine::Operator {
        runtime: tokio::runtime::Handle::current(),
        ring: std::sync::Mutex::default(),
        overlay: std::sync::atomic::AtomicBool::default(),
    });
    // retention (`events.keep_days`): the log and gateway usage lose their old rows at each start, before the first
    // call reads them
    let pruned = tokio::task::spawn_blocking(|| {
        let days = legionctl_core::config::resolve(&legionctl_core::config::Dirs::from_env(), None)
            .map(|r| r.config.events.keep_days);
        match days.and_then(|days| legionctl_core::db::Db::open_default()?.prune(days)) {
            Ok(rows) => {
                faults::works("retention");
                if rows > 0 {
                    eprintln!("operator: {rows} old log rows removed (events.keep_days)");
                }
            }
            Err(e) => faults::failed("retention", RETENTION, &e),
        }
        // the dossier registry reads the folders (a folder renamed by hand keeps its rows); it waits for the dossier
        // migration
        if !legionctl_core::migrate::pending() {
            let settled =
                legionctl_core::config::resolve(&legionctl_core::config::Dirs::from_env(), None)
                    .and_then(|r| {
                        let store = legionctl_core::dossier::Dossiers::open(&r.config.dossier)?;
                        for finding in store.sync_registry()? {
                            eprintln!("operator: {finding}");
                        }
                        for kept in legionctl_core::settings::upgrade(&r, &store)? {
                            eprintln!("operator: {kept}");
                        }
                        Ok(())
                    });
            match settled {
                Ok(()) => faults::works("registry"),
                Err(e) => faults::failed("registry", "the dossier registry is not settled", &e),
            }
        }
    })
    .await;
    if let Err(e) = pruned {
        faults::failed("start", "the start's upkeep stopped", &e.into());
    }
    // the long operations a restart cut off go on in the background
    match operator.resume_jobs("running") {
        Ok(()) => faults::works("jobs"),
        Err(e) => faults::failed("jobs", JOBS, &e),
    }
    // after the cut-off wakes kept their ACP sessions: each member with a wake due wakes
    if !legionctl_core::migrate::pending() {
        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || thread::init(runtime));
    }
    // bound before the first call is served: a session started by a call finds the gateway's socket
    let bound = gateway_bind().await;
    let modules = vec![
        module("the machine event recorder", recorder::run),
        module("the session module", |ready| {
            session::run(ready, operator.clone())
        }),
        module("the model gateway", |ready| async move {
            match bound {
                Ok(Some(bound)) => gateway::serve(bound, ready).await,
                Ok(None) => Ok(()),
                Err(e) => Err(e),
            }
        }),
        module("the session policy", |ready| {
            policy::run(ready, operator.clone())
        }),
    ];
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    // a stop waits for no open request or event stream: their clients see the connection end (todo-125)
    tokio::select! {
        served = axum::serve(listener, server::router(operator)) => Ok(served?),
        failed = supervise(modules) => failed,
        _ = term.recv() => Ok(()),
    }
}
