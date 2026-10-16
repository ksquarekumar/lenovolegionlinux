//! `legionctl-app` (DESIGN-09): the rail of the spaces, the navigator of the space, and tabs: views (the shipped
//! seeds and the views that agents make) and threads, each tab with its chat bar. Every value comes from a view's read
//! operation; every button runs an operation with its own approval: a read at once, a change with its plan first.

pub mod activity;
mod forms;
mod graph;
pub mod greeter;
pub mod shell;
pub mod theme;
pub mod thread;
mod widgets;
mod window;
pub use window::AppAssets;

use std::sync::Arc;
use std::time::Duration;

use gpui_kit::{AppContext, AsyncApp, Context, FocusHandle, Task, WeakEntity};
use legionctl_api::client::Client;
use legionctl_api::{Class, EventRow, EventsIn, OPS, ViewButton, op};
use serde_json::Value;

pub use shell::{Space, Tab};

/// How often the screen checks the operator and the live views.
const REFRESH: Duration = Duration::from_secs(2);
/// How often the screen takes the events that the stream gave.
const EVENTS: Duration = Duration::from_millis(100);
/// The view of the first start.
pub const FIRST: &str = "machine-now";

/// A button's change waiting for the user: its operation's class, and the plan of its dry run when it has one.
#[derive(Debug, Clone)]
pub struct Pending {
    pub button: ViewButton,
    pub class: Class,
    /// The operation has a dry run: Apply runs it with `dry_run` false.
    pub dry_run: bool,
    /// The dry run's text; none when the operation has no dry run (an inline confirm of the operation and input).
    pub plan: Option<String>,
}

/// The form of a button's operation (DESIGN-09 G3): its fields from the operation's input schema, the texts, the
/// field that takes the keys, and why the last Continue did not go on.
#[derive(Debug, Clone)]
pub struct Form {
    pub button: ViewButton,
    pub fields: Vec<legionctl_api::form::Field>,
    pub values: std::collections::BTreeMap<String, String>,
    pub focused: Option<usize>,
    /// The optional fields show.
    pub all: bool,
    pub error: Option<String>,
}

/// What the app knows besides the screen.
#[derive(Debug, Default)]
pub struct State {
    pub form: Option<Form>,
    pub pending: Option<Pending>,
    /// The last result or error.
    pub message: Option<String>,
    /// The operator does not answer: the app shows how to start it.
    pub offline: Option<String>,
    pub busy: bool,
}

pub struct LegionApp {
    client: Arc<Client>,
    runtime: tokio::runtime::Handle,
    pub state: State,
    /// The screen: spaces, navigator, tabs, chat bars, threads and views.
    pub screen: thread::Screen,
    chat_focus: FocusHandle,
    form_focus: FocusHandle,
    /// The tab body's scroll: a form or a plan shows at its top.
    body_scroll: gpui_kit::ScrollHandle,
    _refresh: Task<()>,
    _events: Task<()>,
    _follower: Follower,
}

/// The task that follows the event stream; it stops with the app.
struct Follower(tokio::task::JoinHandle<()>);

impl Drop for Follower {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl LegionApp {
    /// The app on `client`; `open` is a tab to show at the start (the command line's), besides the tabs that the
    /// layout facts keep; `repo` is the folder whose repository layer of views and config applies.
    pub fn new(
        client: Client,
        runtime: tokio::runtime::Handle,
        open: Option<Tab>,
        repo: std::path::PathBuf,
        cx: &mut Context<Self>,
    ) -> LegionApp {
        let refresh = cx.spawn(
            async move |this: WeakEntity<LegionApp>, cx: &mut AsyncApp| {
                loop {
                    if this.update(cx, LegionApp::refresh).is_err() {
                        return;
                    }
                    cx.background_executor().timer(REFRESH).await;
                }
            },
        );
        let client = Arc::new(client);
        let (events, follower) = LegionApp::follow(client.clone(), &runtime, cx);
        let mut app = LegionApp {
            client,
            runtime,
            state: State::default(),
            screen: thread::Screen::new(repo),
            chat_focus: cx.focus_handle(),
            form_focus: cx.focus_handle(),
            body_scroll: gpui_kit::ScrollHandle::new(),
            _refresh: refresh,
            _events: events,
            _follower: follower,
        };
        app.read_layout(open, cx);
        app.read_thread(cx);
        app.read_views(cx);
        app
    }

    /// The dossier of the chat bar's runs.
    #[must_use]
    pub fn with_dossier(mut self, dossier: Option<String>) -> Self {
        self.screen.dossier = dossier;
        self
    }

    /// Follow the operator's event stream from now on the app's tokio runtime; the events go to
    /// [`LegionApp::events`]. A broken stream opens again after its last event (C6).
    fn follow(
        client: Arc<Client>,
        runtime: &tokio::runtime::Handle,
        cx: &mut Context<Self>,
    ) -> (Task<()>, Follower) {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<EventRow>();
        let follower = runtime.spawn(async move {
            let mut offset = "now".to_string();
            loop {
                if let Ok(events) = client.follow(&offset).await {
                    let _ = events
                        .each(|e| {
                            if let Some(id) = e["id"].as_i64() {
                                offset = id.to_string();
                            }
                            if let Ok(row) = serde_json::from_value::<EventRow>(e.clone()) {
                                let _ = tx.send(row);
                            }
                        })
                        .await;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });
        // shortcut: the screen takes the events each 100 ms, because GPUI's test scheduler refuses a wake from a
        // tokio thread; wake it from the stream if GPUI gets a thread-safe test waker
        let task = cx.spawn(
            async move |this: WeakEntity<LegionApp>, cx: &mut AsyncApp| {
                loop {
                    cx.background_executor().timer(EVENTS).await;
                    let mut rows = Vec::new();
                    while let Ok(row) = rx.try_recv() {
                        rows.push(row);
                    }
                    if this
                        .update(cx, |app, cx| {
                            if !rows.is_empty() {
                                app.events(rows, cx);
                            }
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            },
        );
        (task, Follower(follower))
    }

    /// Whether the operator answers (one event read), the live view of the tab, and the Activity's slow parts.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let task = self.operator(cx, async move {
            client
                .call::<op::EventsOp>(&EventsIn {
                    cursor: None,
                    limit: 1,
                })
                .await
        });
        cx.spawn(
            async move |this: WeakEntity<LegionApp>, cx: &mut AsyncApp| {
                let result = task.await;
                let _ = this.update(cx, |app, cx| {
                    match result {
                        Ok(_) => {
                            app.state.offline = None;
                            app.screen.updated = Some(std::time::Instant::now());
                        }
                        Err(e) => app.state.offline = Some(format!("{e:#}")),
                    }
                    cx.notify();
                });
            },
        )
        .detach();
        // a live view reads its data again; its spec stays (a refresh never regenerates a view)
        let key = self.tab_key();
        let due = self
            .screen
            .view()
            .and_then(|v| v.every)
            .is_some_and(|every| {
                self.screen
                    .rendered_at
                    .get(&key)
                    .is_none_or(|at| at.elapsed() >= Duration::from_secs(u64::from(every)))
            });
        if due {
            self.render_view(cx);
        }
        if self.screen.ticks.is_multiple_of(15) {
            self.load_space(cx);
        }
        self.screen.ticks += 1;
    }

    /// The data of the tab that shows: its view (once; then its refresh), or its thread.
    pub(crate) fn load_tab(&mut self, cx: &mut Context<Self>) {
        match self.tab().cloned() {
            Some(Tab::View { .. }) if !self.screen.view_data.contains_key(&self.tab_key()) => {
                self.render_view(cx);
            }
            Some(Tab::Thread { .. }) => self.read_thread(cx),
            _ => {}
        }
    }

    /// The key of the tab that shows.
    pub(crate) fn tab_key(&self) -> String {
        self.tab().map(Tab::key).unwrap_or_default()
    }

    /// Run an operator call. It blocks a background thread of GPUI's executor on the app's tokio runtime, so that
    /// only GPUI's own threads wake the view's tasks.
    fn operator<T: Send + 'static>(
        &self,
        cx: &Context<Self>,
        call: impl Future<Output = anyhow::Result<T>> + Send + 'static,
    ) -> Task<anyhow::Result<T>> {
        let runtime = self.runtime.clone();
        cx.background_spawn(async move { runtime.block_on(call) })
    }

    /// Await an operator call on the view: `ok` with the answer, or the error as the message.
    fn finish<T: Send + 'static>(
        &mut self,
        cx: &mut Context<Self>,
        task: Task<anyhow::Result<T>>,
        ok: impl FnOnce(&mut LegionApp, T) + 'static,
    ) {
        self.finish_with(cx, task, |app, value, _| ok(app, value));
    }

    /// [`LegionApp::finish`] where `ok` needs the view's context.
    fn finish_with<T: Send + 'static>(
        &mut self,
        cx: &mut Context<Self>,
        task: Task<anyhow::Result<T>>,
        ok: impl FnOnce(&mut LegionApp, T, &mut Context<Self>) + 'static,
    ) {
        self.state.busy = true;
        cx.spawn(
            async move |this: WeakEntity<LegionApp>, cx: &mut AsyncApp| {
                let result = task.await;
                let _ = this.update(cx, |app, cx| {
                    app.state.busy = false;
                    match result {
                        Ok(value) => ok(app, value, cx),
                        Err(e) => app.state.message = Some(format!("{e:#}")),
                    }
                    cx.notify();
                });
            },
        )
        .detach();
    }

    /// Open a view as a tab.
    pub fn open_view(&mut self, name: &str, cx: &mut Context<Self>) {
        self.open_tab(
            Tab::View {
                name: name.to_string(),
                params: Vec::new(),
            },
            cx,
        );
    }

    /// A view's button: a read runs at once; a change with a dry run shows its plan first; a change without one
    /// asks inline with its operation and input. Apply runs it; for the machine, polkit asks.
    pub fn act(&mut self, button: ViewButton, cx: &mut Context<Self>) {
        let Some(info) = OPS.iter().find(|o| o.name == button.op && !o.internal()) else {
            self.state.message = Some(format!(
                "the button {} names no operation {}",
                button.label, button.op
            ));
            cx.notify();
            return;
        };
        // the form or the plan shows at the top of the tab
        self.body_scroll
            .set_offset(gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(0.)));
        if legionctl_api::form::needed(info, &button.input) {
            self.state.pending = None;
            self.state.form = Some(Form {
                fields: legionctl_api::form::fields(info),
                values: legionctl_api::form::texts(&button.input),
                button,
                focused: None,
                all: false,
                error: None,
            });
            cx.notify();
            return;
        }
        self.confirm(button, cx);
    }

    /// The form's Continue: its input, then the approval of the operation's class.
    pub fn submit_form(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.state.form.as_mut() else {
            return;
        };
        let Some(info) = OPS.iter().find(|o| o.name == form.button.op) else {
            return;
        };
        match legionctl_api::form::input(info, &form.values, &self.screen.repo) {
            Ok(input) => {
                let mut button = form.button.clone();
                button.input = input;
                self.state.form = None;
                self.confirm(button, cx);
            }
            Err(e) => form.error = Some(format!("{e:#}")),
        }
        cx.notify();
    }

    /// `button` with its whole input, in its class's approval.
    fn confirm(&mut self, button: ViewButton, cx: &mut Context<Self>) {
        let Some(info) = OPS.iter().find(|o| o.name == button.op) else {
            return;
        };
        let dry_run = (info.input)()["properties"].get("dry_run").is_some();
        let class = info.class;
        if class == Class::Read {
            self.state.pending = None;
            self.run(button, false, cx);
            return;
        }
        if !dry_run {
            self.state.pending = Some(Pending {
                button,
                class,
                dry_run,
                plan: None,
            });
            cx.notify();
            return;
        }
        let client = self.client.clone();
        let mut input = button.input.clone();
        input["dry_run"] = Value::Bool(true);
        let name = info.name;
        let task = self.operator(cx, async move {
            let value = client.call_value(name, &input).await?;
            legionctl_api::render(name, value).map(|(text, _)| text)
        });
        self.finish(cx, task, move |app, plan| {
            app.state.pending = Some(Pending {
                button,
                class,
                dry_run,
                plan: Some(plan),
            });
        });
    }

    /// Run `button`'s operation; its text is the message, and the tab's view reads again.
    fn run(&mut self, button: ViewButton, dry_run: bool, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let mut input = button.input;
        if dry_run {
            input["dry_run"] = Value::Bool(false);
        }
        let op = button.op;
        let task = self.operator(cx, async move {
            let value = client.call_value(&op, &input).await?;
            legionctl_api::render(&op, value).map(|(text, _)| text)
        });
        self.finish(cx, task, |app, text: String| {
            app.state.message = Some(if text.trim().is_empty() {
                "done".into()
            } else {
                text
            });
        });
        let key = self.tab_key();
        self.screen.view_data.remove(&key);
        self.load_tab(cx);
    }

    /// Apply the pending action.
    pub fn apply(&mut self, cx: &mut Context<Self>) {
        if let Some(pending) = self.state.pending.take() {
            self.run(pending.button, pending.dry_run, cx);
        }
    }

    pub fn discard(&mut self, cx: &mut Context<Self>) {
        self.state.pending = None;
        cx.notify();
    }

    /// Show another version of the view `name` (undo, redo, reset, pin); the tab reads it again.
    pub fn version(&mut self, name: &str, to: &str, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let input = legionctl_api::ViewVersionIn {
            name: name.to_string(),
            to: to.to_string(),
            repo: legionctl_api::ClientPath(self.screen.repo.clone()),
        };
        let task = self.operator(
            cx,
            async move { client.call::<op::ViewsVersion>(&input).await },
        );
        self.finish_with(cx, task, |app, lines, cx| {
            app.state.message = Some(lines.lines.join("\n"));
            let key = app.tab_key();
            app.screen.view_data.remove(&key);
            app.load_tab(cx);
        });
    }
}
