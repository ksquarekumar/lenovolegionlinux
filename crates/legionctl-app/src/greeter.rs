//! `legionctl-greeter` (todo-83, note 21): the login screen in place of GDM. greetd starts it in `cage` as its user
//! `greeter`; it logs a user in through greetd's IPC (PAM behind greetd asks for the password or the fingerprint) and
//! starts the chosen session. Before login it changes the Windows VM, the Windows disk and the GPUs of the user typed
//! in, through the daemon (a polkit rule of the package lets the user `greeter` do these actions only).

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, bail};
use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Disableable, Selectable, StyledExt};
use gpui_kit::{
    AppContext, AsyncWindowContext, Context, Div, Entity, IntoElement, ParentElement, Render,
    Styled, Subscription, Task, WeakEntity, Window, div, px,
};
use serde::{Deserialize, Serialize};

/// A request to greetd (greetd-ipc(7)).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    CreateSession { username: String },
    PostAuthMessageResponse { response: Option<String> },
    StartSession { cmd: Vec<String>, env: Vec<String> },
    CancelSession,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMessageType {
    Visible,
    Secret,
    Info,
    Error,
}

/// greetd's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Success,
    Error {
        error_type: String,
        description: String,
    },
    AuthMessage {
        auth_message_type: AuthMessageType,
        auth_message: String,
    },
}

/// Where a login is after greetd's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Auth {
    /// PAM asks: show `text`, send the answer (hidden when `secret`).
    Prompt { text: String, secret: bool },
    /// PAM says something and waits (the fingerprint reader): show `text`; `respond(None)` continues.
    Info(String),
    /// The user is authenticated: start the session.
    Done,
    /// PAM refused (a wrong password); the session is cancelled, so a new login starts again.
    Refused(String),
}

/// A connection to greetd: one JSON message per frame, its length first (u32, native byte order).
pub struct Greetd(UnixStream);

impl Greetd {
    /// # Errors
    /// greetd's socket does not answer.
    pub fn connect(path: &Path) -> Result<Greetd> {
        Ok(Greetd(UnixStream::connect(path).with_context(|| {
            format!("connecting to greetd at {}", path.display())
        })?))
    }

    fn call(&mut self, request: &Request) -> Result<Response> {
        let body = serde_json::to_vec(request)?;
        let len = u32::try_from(body.len())?;
        self.0.write_all(&len.to_ne_bytes())?;
        self.0.write_all(&body)?;
        let mut len = [0u8; 4];
        self.0
            .read_exact(&mut len)
            .context("greetd closed the connection")?;
        let mut body = vec![0u8; usize::try_from(u32::from_ne_bytes(len))?];
        self.0.read_exact(&mut body)?;
        Ok(serde_json::from_slice(&body)?)
    }

    fn auth(&mut self, response: Response) -> Result<Auth> {
        Ok(match response {
            Response::Success => Auth::Done,
            Response::AuthMessage {
                auth_message_type,
                auth_message,
            } => match auth_message_type {
                AuthMessageType::Visible => Auth::Prompt {
                    text: auth_message,
                    secret: false,
                },
                AuthMessageType::Secret => Auth::Prompt {
                    text: auth_message,
                    secret: true,
                },
                AuthMessageType::Info | AuthMessageType::Error => Auth::Info(auth_message),
            },
            // greetd keeps a failed session until it is cancelled
            Response::Error { description, .. } => {
                self.cancel()?;
                Auth::Refused(description)
            }
        })
    }

    /// Start a login of `user`.
    ///
    /// # Errors
    /// greetd does not answer.
    pub fn create_session(&mut self, user: &str) -> Result<Auth> {
        let r = self.call(&Request::CreateSession {
            username: user.into(),
        })?;
        self.auth(r)
    }

    /// Answer PAM's message (`None` after an info message).
    ///
    /// # Errors
    /// greetd does not answer.
    pub fn respond(&mut self, answer: Option<String>) -> Result<Auth> {
        let r = self.call(&Request::PostAuthMessageResponse { response: answer })?;
        self.auth(r)
    }

    /// Start the session of the authenticated user; greetd starts it when the greeter ends.
    ///
    /// # Errors
    /// greetd refuses it.
    pub fn start(&mut self, session: &Session) -> Result<()> {
        match self.call(&Request::StartSession {
            cmd: session.exec.clone(),
            env: session.env(),
        })? {
            Response::Success => Ok(()),
            other => bail!("greetd did not start the session: {other:?}"),
        }
    }

    /// Cancel the login that is in progress.
    ///
    /// # Errors
    /// greetd does not answer, or refuses.
    pub fn cancel(&mut self) -> Result<()> {
        match self.call(&Request::CancelSession)? {
            Response::Success => Ok(()),
            other => bail!("greetd did not cancel the login: {other:?}"),
        }
    }
}

/// Where the greeter keeps the last desktop of each user (`greeter install` makes the folder for the user
/// `greeter`).
pub const REMEMBERED: &str = "/var/lib/legionctl-greeter/last-sessions";

/// The folders of the session files: Wayland sessions, which the greeter starts, and X11 sessions, which it shows as
/// not supported.
pub const WAYLAND_SESSIONS: &str = "/usr/share/wayland-sessions";
pub const X11_SESSIONS: &str = "/usr/share/xsessions";

/// A session from `/usr/share/wayland-sessions` (or an X11 one from `/usr/share/xsessions`, with why it does not
/// start).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub name: String,
    /// The file's name without `.desktop` (`gnome`).
    pub id: String,
    pub exec: Vec<String>,
    /// `DesktopNames`, for `XDG_CURRENT_DESKTOP`.
    pub desktops: String,
    /// Why the greeter cannot start it (an X11 session); `None` for a Wayland session.
    pub unsupported: Option<String>,
}

impl Session {
    fn env(&self) -> Vec<String> {
        vec![
            "XDG_SESSION_TYPE=wayland".into(),
            format!("XDG_SESSION_DESKTOP={}", self.id),
            format!("XDG_CURRENT_DESKTOP={}", self.desktops),
        ]
    }
}

/// The Wayland sessions in `wayland`, GNOME first, then the X11 sessions in `x11`, each with why it does not start:
/// the greeter runs in cage, a Wayland compositor, and starts no X server (todo-150).
#[must_use]
pub fn sessions(wayland: &Path, x11: &Path) -> Vec<Session> {
    let mut found = read_sessions(wayland, None);
    found.extend(read_sessions(
        x11,
        Some(
            "an X11 session: this greeter starts Wayland sessions only; log in to it through GDM \
             (legionctl greeter remove)",
        ),
    ));
    found.sort_by_key(|s| (s.unsupported.is_some(), s.id != "gnome", s.name.clone()));
    found
}

fn read_sessions(dir: &Path, unsupported: Option<&str>) -> Vec<Session> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let id = entry
                .file_name()
                .to_string_lossy()
                .strip_suffix(".desktop")?
                .to_string();
            let text = std::fs::read_to_string(entry.path()).ok()?;
            let key = |k: &str| {
                text.lines()
                    .find_map(|l| l.strip_prefix(k)?.strip_prefix('='))
                    .map(str::trim)
            };
            if key("Hidden") == Some("true") || key("NoDisplay") == Some("true") {
                return None;
            }
            let exec: Vec<String> = key("Exec")?
                .split_whitespace()
                .filter(|w| !w.starts_with('%'))
                .map(String::from)
                .collect();
            let name = key("Name").unwrap_or(&id).to_string();
            Some(Session {
                desktops: key("DesktopNames")
                    .map_or(name.clone(), |d| d.trim_end_matches(';').replace(';', ":")),
                name,
                id,
                exec,
                unsupported: unsupported.map(String::from),
            })
        })
        .collect()
}

/// The last desktop of each user, one `<user>\t<session id>` line each, in a file of the greeter's own.
#[derive(Debug, Clone)]
pub struct Remembered(pub PathBuf);

impl Remembered {
    fn read(&self) -> Result<Vec<(String, String)>> {
        let text = match std::fs::read_to_string(&self.0) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", self.0.display())),
        };
        Ok(text
            .lines()
            .filter_map(|l| l.split_once('\t'))
            .map(|(u, s)| (u.to_string(), s.to_string()))
            .collect())
    }

    /// The session id that `user` started last.
    ///
    /// # Errors
    /// The file is there but cannot be read.
    pub fn session(&self, user: &str) -> Result<Option<String>> {
        Ok(self
            .read()?
            .into_iter()
            .find(|(u, _)| u == user)
            .map(|(_, s)| s))
    }

    /// `user` started `session`.
    ///
    /// # Errors
    /// The file cannot be read or written.
    pub fn set(&self, user: &str, session: &str) -> Result<()> {
        let mut all = self.read()?;
        all.retain(|(u, _)| u != user);
        all.push((user.to_string(), session.to_string()));
        let text: String = all
            .iter()
            .flat_map(|(u, s)| [u.as_str(), "\t", s.as_str(), "\n"])
            .collect();
        let tmp = self.0.with_extension("new");
        std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.0).with_context(|| format!("writing {}", self.0.display()))?;
        Ok(())
    }
}

/// What the screen asks for now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    User,
    Prompt { text: String, secret: bool },
    Waiting(String),
    Started,
}

/// What a greetd call of the screen ended with.
enum After {
    Auth(Auth),
    Cancelled,
    Started,
}

pub struct Greeter {
    greetd: Arc<Mutex<Greetd>>,
    runtime: tokio::runtime::Handle,
    pub sessions: Vec<Session>,
    pub session: usize,
    /// The user chose a session on this screen: the remembered one does not replace it.
    chosen: bool,
    remembered: Remembered,
    user: Entity<InputState>,
    answer: Entity<InputState>,
    pub stage: Stage,
    /// The last result or error, in one line.
    pub message: Option<String>,
    pub busy: bool,
    _subscriptions: Vec<Subscription>,
}

impl Greeter {
    pub fn new(
        greetd: Greetd,
        sessions: Vec<Session>,
        remembered: Remembered,
        runtime: tokio::runtime::Handle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Greeter {
        let user = cx.new(|cx| InputState::new(window, cx).placeholder("User name"));
        let answer = cx.new(|cx| InputState::new(window, cx).masked(true));
        let subscriptions = vec![
            cx.subscribe_in(&user, window, |this: &mut Greeter, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.login(window, cx);
                }
            }),
            cx.subscribe_in(
                &answer,
                window,
                |this: &mut Greeter, _, event, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.answer(window, cx);
                    }
                },
            ),
        ];
        user.update(cx, |input, cx| input.focus(window, cx));
        Greeter {
            greetd: Arc::new(Mutex::new(greetd)),
            runtime,
            sessions,
            session: 0,
            chosen: false,
            remembered,
            user,
            answer,
            stage: Stage::User,
            message: None,
            busy: false,
            _subscriptions: subscriptions,
        }
    }

    /// Run `call` on greetd on a background thread, then go on with its answer.
    fn greetd(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        call: impl FnOnce(&mut Greetd) -> Result<After> + Send + 'static,
    ) {
        let greetd = self.greetd.clone();
        let task: Task<Result<After>> = cx.background_spawn(async move {
            let mut greetd = greetd
                .lock()
                .map_err(|_| anyhow::anyhow!("greetd connection poisoned"))?;
            call(&mut greetd)
        });
        self.busy = true;
        cx.spawn_in(
            window,
            async move |this: WeakEntity<Greeter>, cx: &mut AsyncWindowContext| {
                let result = task.await;
                let _ = this.update_in(cx, |greeter, window, cx| {
                    greeter.busy = false;
                    match result {
                        Ok(After::Auth(auth)) => greeter.next(auth, window, cx),
                        Ok(After::Cancelled) => greeter.stage = Stage::User,
                        // greetd starts the session when the greeter ends
                        Ok(After::Started) => cx.quit(),
                        Err(e) => {
                            greeter.message = Some(format!("{e:#}"));
                            greeter.stage = Stage::User;
                        }
                    }
                    cx.notify();
                });
            },
        )
        .detach();
    }

    fn next(&mut self, auth: Auth, window: &mut Window, cx: &mut Context<Self>) {
        match auth {
            Auth::Prompt { text, secret } => {
                self.answer.update(cx, |input, cx| {
                    input.set_masked(secret, window, cx);
                    input.set_value("", window, cx);
                    input.focus(window, cx);
                });
                self.stage = Stage::Prompt { text, secret };
            }
            Auth::Info(text) => {
                self.stage = Stage::Waiting(text);
                self.greetd(window, cx, |g| g.respond(None).map(After::Auth));
            }
            Auth::Done => {
                let Some(session) = self
                    .sessions
                    .get(self.session)
                    .filter(|s| s.unsupported.is_none())
                    .cloned()
                else {
                    self.message = Some(format!("no session in {WAYLAND_SESSIONS}"));
                    return;
                };
                let (user, remembered) = (self.user_name(cx), self.remembered.clone());
                self.greetd(window, cx, move |g| {
                    g.start(&session)?;
                    // the session starts either way; greetd's journal keeps the line
                    if let Err(e) = remembered.set(&user, &session.id) {
                        eprintln!(
                            "legionctl-greeter: the last desktop of {user} is not kept: {e:#}"
                        );
                    }
                    Ok(After::Started)
                });
                self.stage = Stage::Started;
            }
            Auth::Refused(why) => {
                self.message = Some(why);
                // a new login of the same user: PAM asks again
                self.login(window, cx);
            }
        }
    }

    fn user_name(&self, cx: &Context<Self>) -> String {
        self.user.read(cx).value().trim().to_string()
    }

    pub fn login(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let user = self.user_name(cx);
        if user.is_empty() {
            self.message = Some("Type the user name".into());
            cx.notify();
            return;
        }
        if !self.chosen {
            match self.remembered.session(&user) {
                Ok(last) => {
                    if let Some(i) = self
                        .sessions
                        .iter()
                        .position(|s| Some(&s.id) == last.as_ref() && s.unsupported.is_none())
                    {
                        self.session = i;
                    }
                }
                Err(e) => self.message = Some(format!("the last desktop of {user}: {e:#}")),
            }
        }
        self.greetd(window, cx, move |g| {
            g.create_session(&user).map(After::Auth)
        });
    }

    fn answer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let answer = self.answer.read(cx).value().to_string();
        self.answer
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.greetd(window, cx, move |g| {
            g.respond(Some(answer)).map(After::Auth)
        });
    }

    pub fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.greetd(window, cx, |g| g.cancel().map(|()| After::Cancelled));
        self.user.update(cx, |input, cx| input.focus(window, cx));
    }

    /// A machine change through the daemon (`method` with `body`); polkit allows it for the user `greeter`.
    fn daemon<B>(&mut self, method: &'static str, body: B, cx: &mut Context<Self>)
    where
        B: Serialize + zbus::zvariant::DynamicType + Send + Sync + 'static,
    {
        let runtime = self.runtime.clone();
        let task = cx.background_spawn(async move {
            runtime.block_on(async move {
                let conn = zbus::Connection::system().await?;
                let proxy = zbus::Proxy::new(
                    &conn,
                    "org.legionctl.System1",
                    "/org/legionctl/System1",
                    "org.legionctl.System1",
                )
                .await?;
                let lines: Vec<String> = proxy.call(method, &body).await?;
                anyhow::Ok(lines)
            })
        });
        self.busy = true;
        cx.spawn(async move |this: WeakEntity<Greeter>, cx| {
            let result = task.await;
            let _ = this.update(cx, |greeter, cx| {
                greeter.busy = false;
                greeter.message = Some(match result {
                    Ok(lines) if lines.is_empty() => "nothing to change".into(),
                    Ok(lines) => lines.join("; "),
                    Err(e) => format!("{e:#}"),
                });
                cx.notify();
            });
        })
        .detach();
    }

    /// A choice for the user typed in (`vm-start`, `disk-detach`, `gpu-none`, ...).
    fn act(&mut self, action: &'static str, cx: &mut Context<Self>) {
        let user = self.user_name(cx);
        if user.is_empty() {
            self.message = Some("Type the user name first".into());
            cx.notify();
            return;
        }
        self.daemon("GreeterAct", (user, action.to_string()), cx);
    }

    fn row(
        &self,
        label: &str,
        buttons: &[(&'static str, &'static str)],
        cx: &mut Context<Self>,
    ) -> Div {
        div()
            .h_flex()
            .gap_2()
            .child(div().w(px(130.)).text_sm().child(label.to_string()))
            .children(buttons.iter().map(|&(action, text)| {
                Button::new(action)
                    .label(text)
                    .disabled(self.busy)
                    .on_click(cx.listener(move |g, _, _, cx| g.act(action, cx)))
            }))
    }

    fn machine(&self, cx: &mut Context<Self>) -> Div {
        div()
            .v_flex()
            .gap_2()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .child(div().font_semibold().child("Before login"))
            .child(self.row(
                "Windows VM",
                &[("vm-start", "Start"), ("vm-stop", "Stop")],
                cx,
            ))
            .child(self.row(
                "Windows disk",
                &[
                    ("disk-attach", "Attach to Linux"),
                    ("disk-detach", "Detach"),
                ],
                cx,
            ))
            .child(self.row(
                "NVIDIA GPU",
                &[("gpu-linux", "Linux"), ("gpu-none", "Free for the VM")],
                cx,
            ))
            .child(
                div()
                    .h_flex()
                    .gap_2()
                    .child(div().w(px(130.)).text_sm().child("Desktop GPU"))
                    .children(
                        [
                            ("desktop-intel", "Intel", "intel"),
                            ("desktop-nvidia", "NVIDIA", "nvidia"),
                            ("desktop-auto", "Auto", "auto"),
                        ]
                        .map(|(id, text, choice)| {
                            Button::new(id)
                                .label(text)
                                .disabled(self.busy)
                                .on_click(cx.listener(move |g, _, _, cx| {
                                    g.daemon("SetPrimaryGpu", (choice.to_string(), false), cx);
                                }))
                        }),
                    ),
            )
    }
}

impl Render for Greeter {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut login = div()
            .v_flex()
            .gap_3()
            .w(px(420.))
            .child(div().text_xl().font_semibold().child("legionctl"))
            .child(Input::new(&self.user).id("user"));
        match &self.stage {
            Stage::Prompt { text, .. } => {
                login = login
                    .child(div().text_sm().child(text.clone()))
                    .child(Input::new(&self.answer).id("answer"))
                    .child(
                        Button::new("cancel")
                            .label("Cancel")
                            .on_click(cx.listener(|g, _, window, cx| g.cancel(window, cx))),
                    );
            }
            Stage::Waiting(text) => login = login.child(div().text_sm().child(text.clone())),
            Stage::User | Stage::Started => {}
        }
        login = login.child(div().h_flex().flex_wrap().gap_2().children(
            self.sessions.iter().enumerate().map(|(i, s)| {
                let label = match &s.unsupported {
                    Some(_) => format!("{} (X11, not supported)", s.name),
                    None => s.name.clone(),
                };
                Button::new(gpui_kit::SharedString::from(format!("session-{}", s.id)))
                    .label(label)
                    .selected(i == self.session)
                    .disabled(s.unsupported.is_some())
                    .on_click(cx.listener(move |g, _, _, cx| {
                        g.session = i;
                        g.chosen = true;
                        cx.notify();
                    }))
            }),
        ));
        if let Some(why) = self.sessions.iter().find_map(|s| s.unsupported.as_ref()) {
            login = login.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("X11 sessions: {why}")),
            );
        }
        if let Some(message) = &self.message {
            login = login.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(message.clone()),
            );
        }
        div()
            .size_full()
            .v_flex()
            .items_center()
            .justify_center()
            .gap_4()
            .bg(cx.theme().background)
            .child(login)
            .child(self.machine(cx))
    }
}
