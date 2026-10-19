//! `legionctl-app` (DESIGN-09): the rail of the spaces, the navigator of the space, and tabs: views (the shipped
//! seeds and the views that agents make) and threads, each tab with its chat bar. Every value comes from a view's read
//! operation; every button runs an operation with its own approval: a read at once, a change with its plan first.

pub mod activity;
mod feedback;
mod forms;
mod graph;
pub mod greeter;
pub mod help;
mod history;
mod input;
pub mod shell;
pub mod theme;
pub mod thread;
mod widgets;
mod window;
pub use window::AppAssets;

use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::input::{InputState, TextareaState};
use gpui_kit::{
    AppContext, AsyncApp, Context, Entity, FocusHandle, Subscription, Task, WeakEntity, Window,
};
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

/// The app's key context: its keys go before the kit's root keys.
const KEYS: &str = "LegionApp";

/// The app's key bindings; call it once after `gpui_kit::init`. Every text field (the kit's input) pastes with
/// Ctrl+V, Ctrl+Shift+V and Shift+Insert, and copies with Ctrl+C and Ctrl+Insert, as the other apps of the desktop do.
pub fn init(cx: &mut gpui_kit::App) {
    use gpui_kit::component::input::{Copy, Enter, Paste};
    cx.bind_keys([
        gpui_kit::KeyBinding::new(
            "ctrl-j",
            Enter {
                secondary: false,
                shift: true,
            },
            Some(input::CHAT),
        ),
        gpui_kit::KeyBinding::new("ctrl-shift-v", Paste, Some(input::CONTEXT)),
        gpui_kit::KeyBinding::new("shift-insert", Paste, Some(input::CONTEXT)),
        gpui_kit::KeyBinding::new("ctrl-insert", Copy, Some(input::CONTEXT)),
        // the right-click menu names the newest key of an action: Ctrl+C and Ctrl+V, the keys people know
        gpui_kit::KeyBinding::new("ctrl-c", Copy, Some(input::CONTEXT)),
        gpui_kit::KeyBinding::new("ctrl-v", Paste, Some(input::CONTEXT)),
    ]);
}

/// A button's change waiting for the user: its operation's class, and the plan of its dry run when it has one.
#[derive(Debug, Clone)]
pub struct Pending {
    pub button: ViewButton,
    pub class: Class,
    /// The operation has a dry run: Apply runs it with `dry_run` false.
    pub dry_run: bool,
    /// The dry run's text; none when the operation has no dry run (an inline confirm of the operation and input).
    pub plan: Option<String>,
    /// The operation's name and raw input show (the Details disclosure).
    pub details: bool,
}

/// The form of a button's operation (DESIGN-09 G3): its fields from the operation's input schema, the texts, the
/// text field of each field that takes text, and why the last Continue did not go on.
#[derive(Debug, Clone)]
pub struct Form {
    pub button: ViewButton,
    pub then: Then,
    pub fields: Vec<legionctl_api::form::Field>,
    pub values: std::collections::BTreeMap<String, String>,
    /// The text fields, by field name; the render makes them (they need the window).
    pub inputs: std::collections::BTreeMap<String, Entity<InputState>>,
    /// The dropdowns of the choices with more words than buttons, by field name, and what they tell the form.
    pub selects: std::collections::BTreeMap<
        String,
        (Entity<forms::Dropdown>, std::rc::Rc<gpui_kit::Subscription>),
    >,
    /// The optional fields show.
    pub all: bool,
    pub error: Option<String>,
}

impl Form {
    #[must_use]
    pub fn new(
        button: ViewButton,
        then: Then,
        fields: Vec<legionctl_api::form::Field>,
        values: std::collections::BTreeMap<String, String>,
        all: bool,
    ) -> Form {
        Form {
            button,
            then,
            fields,
            values,
            inputs: std::collections::BTreeMap::new(),
            selects: std::collections::BTreeMap::new(),
            all,
            error: None,
        }
    }
}

/// What a form's Continue does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Then {
    /// The operation runs, with its class's approval.
    Run,
    /// The chat's environment step: the folder becomes the app's project; a new chat works in it from the base ref,
    /// an existing chat (its `thread` in the form's input) moves to it.
    ChatPlace,
    /// The folder becomes the app's project, and the view tab reads its code.
    TabProject,
}

/// What the app knows besides the screen.
#[derive(Debug, Default)]
pub struct State {
    pub form: Option<Form>,
    pub pending: Option<Pending>,
    /// The last result or error.
    pub message: Option<String>,
    /// The operator is off: the app shows why and Turn on.
    pub offline: Option<String>,
    /// Why the last Turn on failed.
    pub turn_on: Option<String>,
    pub busy: bool,
}

pub struct LegionApp {
    client: Arc<Client>,
    runtime: tokio::runtime::Handle,
    pub state: State,
    /// The screen: spaces, navigator, tabs, chat bars, threads and views.
    pub screen: thread::Screen,
    /// The chat bar's text field; each tab keeps its own text in `screen`.
    chat: Entity<TextareaState>,
    /// The chat bar's text that the field and the tab last agreed on, and the hint that the field shows.
    chat_synced: (String, String),
    chat_focus: FocusHandle,
    /// The pending change's card: its first button (Discard, the safe one) takes the keys.
    pending_focus: FocusHandle,
    /// Where the keys go back to when a form or a pending change closes.
    back: Option<FocusHandle>,
    /// What took the keys in the last frame: a form, a pending change, or neither.
    modal: input::Modal,
    /// The tab body: Up and Down there scroll it.
    body_focus: FocusHandle,
    /// The tab body's scroll: a form or a plan shows at its top.
    body_scroll: gpui_kit::ScrollHandle,
    /// The chat bar's list of commands: the picked one scrolls into view.
    list_scroll: gpui_kit::ScrollHandle,
    /// The field above a view that the person types a parameter in (a search): the tab and the parameter it sets.
    pub(crate) find: Option<input::Find>,
    _refresh: Task<()>,
    _events: Task<()>,
    _follower: Follower,
    _chat_events: Subscription,
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
    /// layout facts keep; `project` is the folder of its calls until the user chooses a project.
    pub fn new(
        client: Client,
        runtime: tokio::runtime::Handle,
        open: Option<Tab>,
        project: std::path::PathBuf,
        window: &mut Window,
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
        let chat = input::chat_field(window, cx);
        let chat_events = cx.subscribe_in(&chat, window, LegionApp::chat_event);
        // every call works in the app's folder (its project once the user chooses one)
        client.set_folder(&project);
        let mut app = LegionApp {
            client,
            runtime,
            state: State::default(),
            screen: thread::Screen::new(project),
            chat_focus: gpui_kit::Focusable::focus_handle(chat.read(cx), cx),
            chat,
            chat_synced: (String::new(), String::new()),
            pending_focus: cx.focus_handle().tab_index(0).tab_stop(false),
            back: None,
            modal: input::Modal::None,
            body_focus: cx.focus_handle().tab_index(0).tab_stop(true),
            body_scroll: gpui_kit::ScrollHandle::new(),
            list_scroll: gpui_kit::ScrollHandle::new(),
            find: None,
            _refresh: refresh,
            _events: events,
            _follower: follower,
            _chat_events: chat_events,
        };
        app.read_layout(open, cx);
        app.read_thread(cx);
        app.read_views(cx);
        app.read_history(cx);
        app
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
                            // the operator was off at the start: the layout read failed then
                            if app.state.offline.take().is_some() && !app.screen.restored {
                                app.read_layout(None, cx);
                            }
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
            self.read_accounts(cx);
        }
        self.screen.ticks += 1;
    }

    /// The offline card's Turn on: the user's service manager enables and starts the operator's socket, as
    /// `systemctl --user enable --now legionctl-operator.socket` does (no root); then the app asks the operator again.
    pub(crate) fn turn_on(&mut self, cx: &mut Context<Self>) {
        let private = self.client.runtime().join("systemd/private");
        self.state.turn_on = None;
        let task = self.operator(cx, async move {
            let conn = zbus::connection::Builder::address(
                format!("unix:path={}", private.display()).as_str(),
            )?
            .p2p()
            .build()
            .await?;
            manager(
                &conn,
                "EnableUnitFiles",
                &(&["legionctl-operator.socket"][..], false, false),
            )
            .await?;
            manager(&conn, "Reload", &()).await?;
            manager(
                &conn,
                "StartUnit",
                &("legionctl-operator.socket", "replace"),
            )
            .await?;
            Ok(())
        });
        cx.spawn(
            async move |this: WeakEntity<LegionApp>, cx: &mut AsyncApp| {
                let result = task.await;
                let _ = this.update(cx, |app, cx| match result {
                    Ok(()) => app.refresh(cx),
                    Err(e) => {
                        app.state.turn_on = Some(format!("Turn on failed: {e:#}"));
                        cx.notify();
                    }
                });
            },
        )
        .detach();
    }

    /// The data of the tab that shows: its view (once; then its refresh), or its thread.
    pub(crate) fn load_tab(&mut self, cx: &mut Context<Self>) {
        match self.tab().cloned() {
            Some(Tab::View { .. }) if !self.screen.view_data.contains_key(&self.tab_key()) => {
                self.render_view(cx);
            }
            Some(Tab::Thread { .. }) => self.read_thread(cx),
            Some(Tab::Chat) if !self.screen.view_data.contains_key(thread::PICKER) => {
                self.load_picker(cx);
            }
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

    /// Run a long operator call (an agent's run) on the app's tokio runtime, and take its end on a timer of the screen,
    /// as the event stream's rows: the screen goes on (and shows the run's output events) while the call runs.
    fn operator_long<T: Send + 'static>(
        &self,
        cx: &Context<Self>,
        call: impl Future<Output = anyhow::Result<T>> + Send + 'static,
    ) -> Task<anyhow::Result<T>> {
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        self.runtime.spawn(async move {
            let _ = tx.send(call.await);
        });
        cx.spawn(async move |_, cx: &mut AsyncApp| {
            loop {
                cx.background_executor().timer(EVENTS).await;
                match rx.try_recv() {
                    Ok(result) => return result,
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
                    Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                        return Err(anyhow::anyhow!("no answer from legionctl; try again"));
                    }
                }
            }
        })
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

    /// A view's button: a read and a change of the user's own session run at once (but for a button of a
    /// repository's or a model's view, which confirms every change); a change with a dry run shows its plan first; a
    /// change without one asks inline with its operation and input. Apply runs it; for the machine, polkit asks.
    pub fn act(&mut self, button: ViewButton, cx: &mut Context<Self>) {
        // a button that renders a view opens it in a tab (a map setting's entries)
        if button.op == "views.render"
            && let Some(name) = button.input["name"].as_str()
        {
            let params = button.input["params"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| p.as_str().map(String::from))
                .collect();
            self.open_tab(
                Tab::View {
                    name: name.to_string(),
                    params,
                },
                cx,
            );
            return;
        }
        let Some(info) = OPS.iter().find(|o| o.name == button.op && !o.internal()) else {
            self.state.message = Some(format!("{}: unknown operation {}", button.label, button.op));
            cx.notify();
            return;
        };
        // the form or the plan shows at the top of the tab
        self.body_scroll
            .set_offset(gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(0.)));
        if legionctl_api::form::needed(info, &button.input) {
            self.state.pending = None;
            let texts = legionctl_api::form::texts(&button.input);
            self.state.form = Some(Form::new(
                button.clone(),
                Then::Run,
                legionctl_api::form::fields_for(info, &texts),
                texts,
                false,
            ));
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
        // the text fields' texts
        for (name, input) in &form.inputs {
            form.values
                .insert(name.clone(), input.read(cx).value().to_string());
        }
        if form.then != Then::Run {
            self.submit_place(cx);
            return;
        }
        let Some(info) = OPS.iter().find(|o| o.name == form.button.op) else {
            return;
        };
        match legionctl_api::form::input(info, &form.values) {
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

    /// `button` with its whole input, in its class's approval. Its paths are the user's, against the app's folder,
    /// as on the chat bar (the operator takes only absolute paths).
    fn confirm(&mut self, mut button: ViewButton, cx: &mut Context<Self>) {
        let Some(info) = OPS.iter().find(|o| o.name == button.op) else {
            return;
        };
        legionctl_api::cli::fill_user_fields(info, &mut button.input, &self.screen.project);
        let route = legionctl_api::form::route(info);
        let dry_run = route == legionctl_api::form::Route::Plan;
        let class = info.class;
        if route == legionctl_api::form::Route::Run && !(button.confirm && class != Class::Read) {
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
                details: false,
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
                details: false,
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
        let then = (op.clone(), input.clone());
        let task = self.operator(cx, async move {
            let value = client.call_value(&op, &input).await?;
            legionctl_api::render(&op, value).map(|(text, _)| text)
        });
        self.finish_with(cx, task, move |app, text: String, cx| {
            app.state.message = Some(if text.trim().is_empty() {
                "done".into()
            } else {
                text
            });
            app.after(&then.0, &then.1, cx);
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

    /// Close the form: nothing runs.
    pub fn cancel_form(&mut self, cx: &mut Context<Self>) {
        self.state.form = None;
        cx.notify();
    }

    /// Show another version of the view `name` (undo, redo, reset, pin); the tab reads it again.
    pub fn version(&mut self, name: &str, to: &str, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let input = legionctl_api::ViewVersionIn {
            name: name.to_string(),
            to: to.to_string(),
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

/// A call to the systemd manager on `conn`.
async fn manager<B>(conn: &zbus::Connection, method: &str, body: &B) -> zbus::Result<zbus::Message>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    conn.call_method(
        Some("org.freedesktop.systemd1"),
        "/org/freedesktop/systemd1",
        Some("org.freedesktop.systemd1.Manager"),
        method,
        body,
    )
    .await
}
