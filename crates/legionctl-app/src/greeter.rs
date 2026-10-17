//! `legionctl-greeter` (todo-83, note 21): the login screen in place of GDM. greetd starts it in `cage` as its user
//! `greeter`; it logs a user in through greetd's IPC (PAM behind greetd asks for the password or the fingerprint) and
//! starts the chosen session. Before login it changes the Windows VM, the Windows disk and the GPUs of the user typed
//! in, through the daemon (a polkit rule of the package lets the user `greeter` do these actions only).

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, bail};
use gpui_kit::assets::IconName;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Disableable, Selectable, Sizable, Size, StyledExt, Theme, ThemeMode};
use gpui_kit::{
    AppContext, AsyncWindowContext, Context, Div, Entity, FontWeight, InteractiveElement,
    IntoElement, KeyDownEvent, ParentElement, Render, SharedString, StatefulInteractiveElement,
    Styled, Subscription, Task, WeakEntity, Window, div, prelude::FluentBuilder as _, px,
};

use crate::theme::{self, R_LG, S2, S3, S4, S5, T_MD, T_SM, T_XL, Tokens};
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
            Response::Error {
                error_type,
                description,
            } => {
                self.cancel()?;
                // PAM's own words for a wrong password ("Authentication failure") do not tell the user what to do
                Auth::Refused(if error_type == "auth_error" {
                    "Wrong password. Try again.".into()
                } else {
                    description
                })
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

/// The people's accounts in `passwd` (uid 1000 to 65533, as the daemon's `person`, with a login shell), in file order.
///
/// # Errors
/// `passwd` cannot be read.
pub fn people(passwd: &Path) -> Result<Vec<String>> {
    Ok(std::fs::read_to_string(passwd)
        .with_context(|| format!("reading {}", passwd.display()))?
        .lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split(':').collect();
            let uid: u32 = f.get(2)?.parse().ok()?;
            let shell = f.get(6)?;
            ((1000..65534).contains(&uid)
                && !shell.ends_with("nologin")
                && !shell.ends_with("false"))
            .then(|| f[0].to_string())
        })
        .collect())
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

    /// The user who logged in last (the last line).
    ///
    /// # Errors
    /// The file is there but cannot be read.
    pub fn last_user(&self) -> Result<Option<String>> {
        Ok(self.read()?.pop().map(|(u, _)| u))
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

gpui_kit::assets::icon_assets!(
    Extra,
    [
        Power,
        Moon,
        RotateCcw,
        Monitor,
        MonitorCog,
        SlidersHorizontal,
        FingerprintPattern
    ]
);

/// The kit's icons and the greeter's extra icons.
pub struct GreeterAssets;

impl gpui_kit::AssetSource for GreeterAssets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<std::borrow::Cow<'static, [u8]>>> {
        match Extra.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> gpui_kit::Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(Extra.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

/// The height of the field's icon buttons: the height of a large input.
const FIELD: Size = Size::Size(px(44.));

/// What the screen asks for now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    /// Choose a person, or type a user name.
    Choose,
    /// PAM asks for `text` (the password when `secret`).
    Prompt { text: String, secret: bool },
    /// PAM says `text` and waits (the fingerprint reader).
    Waiting(String),
    /// greetd starts the session when the greeter ends: no login form from here on.
    Started,
}

/// What a greetd call of the screen ended with.
enum After {
    Auth(Auth),
    Cancelled,
    Started,
}

/// logind's power actions; the screen asks once more before it calls one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Power {
    Suspend,
    Restart,
    Off,
}

impl Power {
    fn method(self) -> &'static str {
        match self {
            Power::Suspend => "Suspend",
            Power::Restart => "Reboot",
            Power::Off => "PowerOff",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Power::Suspend => "Suspend",
            Power::Restart => "Restart",
            Power::Off => "Power off",
        }
    }
}

/// One sentence for a failed bus call: the service's own message, not zbus's wrapping.
fn sentence(e: &anyhow::Error) -> String {
    match e.downcast_ref::<zbus::Error>() {
        Some(zbus::Error::MethodError(_, Some(text), _)) => text.clone(),
        _ => format!("{e:#}"),
    }
}

/// The screen's machine choices before login: (row, icon, buttons as (id, label)).
type MachineRow = (
    &'static str,
    IconName,
    &'static [(&'static str, &'static str)],
);
const MACHINE: [MachineRow; 4] = [
    (
        "Windows VM",
        IconName::Monitor,
        &[("vm-start", "Start"), ("vm-stop", "Stop")],
    ),
    (
        "Windows disk",
        IconName::HardDrive,
        &[("disk-attach", "Attach"), ("disk-detach", "Detach")],
    ),
    (
        "NVIDIA GPU",
        IconName::Cpu,
        &[("gpu-linux", "Linux"), ("gpu-none", "VM")],
    ),
    (
        "Desktop GPU",
        IconName::MonitorCog,
        &[
            ("desktop-intel", "Intel"),
            ("desktop-nvidia", "NVIDIA"),
            ("desktop-auto", "Auto"),
        ],
    ),
];

/// What the screen shows and where it calls.
pub struct Setup {
    pub sessions: Vec<Session>,
    /// The accounts to choose from, or why they cannot be read.
    pub people: Result<Vec<String>>,
    pub remembered: Remembered,
    /// The system bus's address (logind and the daemon).
    pub bus: String,
}

pub struct Greeter {
    greetd: Arc<Mutex<Greetd>>,
    runtime: tokio::runtime::Handle,
    /// The system bus (logind and the daemon).
    bus: String,
    pub sessions: Vec<Session>,
    pub session: usize,
    /// The user chose a session on this screen: the remembered one does not replace it.
    chosen: bool,
    remembered: Remembered,
    pub people: Vec<String>,
    /// The user of the login in progress.
    pub user: Option<String>,
    name: Entity<InputState>,
    answer: Entity<InputState>,
    pub stage: Stage,
    /// The last error of the login, in one line under the field.
    pub message: Option<String>,
    /// The result of the last machine or power action, in one line.
    pub note: Option<String>,
    pub busy: bool,
    pub machine: bool,
    /// A power action that waits for its confirmation.
    pub power: Option<Power>,
    _subscriptions: Vec<Subscription>,
}

/// The boot splash stays until this screen shows: legionctl's greetd unit only deactivates plymouth. After the first
/// frame is on the screen, systemd starts `legionctl-splash-quit.service` (`plymouth quit --retain-splash`; polkit
/// allows only that start to the user `greeter`). A failure goes to the journal: the screen works without it.
fn end_splash(window: &Window, runtime: tokio::runtime::Handle, bus: String) {
    window.on_next_frame(move |window, _| {
        // the first frame's callback runs before its draw, the second's after it
        window.on_next_frame(move |_, _| {
            runtime.spawn(async move {
                let start = async {
                    let conn = zbus::connection::Builder::address(bus.as_str())?
                        .build()
                        .await?;
                    conn.call_method(
                        Some("org.freedesktop.systemd1"),
                        "/org/freedesktop/systemd1",
                        Some("org.freedesktop.systemd1.Manager"),
                        "StartUnit",
                        &(SPLASH_QUIT, "replace"),
                    )
                    .await?;
                    anyhow::Ok(())
                };
                if let Err(e) = start.await {
                    eprintln!("the boot splash stays: starting {SPLASH_QUIT} failed: {e:#}");
                }
            });
        });
    });
}

/// The unit that ends the boot splash once the greeter shows (the package's).
pub const SPLASH_QUIT: &str = "legionctl-splash-quit.service";

impl Greeter {
    /// The screen on `greetd` with `setup`: the user who logged in last (else the only person) is chosen, and the
    /// login starts at once.
    pub fn new(
        greetd: Greetd,
        setup: Setup,
        runtime: tokio::runtime::Handle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Greeter {
        let Setup {
            sessions,
            people,
            remembered,
            bus,
        } = setup;
        // a login screen is dark (no desktop setting before login), its text one step larger than the app's
        Theme::change(ThemeMode::Dark, Some(window), cx);
        theme::adopt(cx);
        Theme::global_mut(cx).font_size = px(16.);
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("User name"));
        let answer = cx.new(|cx| InputState::new(window, cx).masked(true));
        let subscriptions = vec![
            cx.subscribe_in(
                &name,
                window,
                |this: &mut Greeter, input, event, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        let user = input.read(cx).value().trim().to_string();
                        this.choose(user, window, cx);
                    }
                },
            ),
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
        let (people, mut message) = match people {
            Ok(people) => (people, None),
            Err(e) => (Vec::new(), Some(format!("{e:#}"))),
        };
        let last = match remembered.last_user() {
            Ok(last) => last.filter(|u| people.contains(u)),
            Err(e) => {
                message = Some(format!("{e:#}"));
                None
            }
        };
        let first = last.or_else(|| (people.len() == 1).then(|| people[0].clone()));
        end_splash(window, runtime.clone(), bus.clone());
        let mut greeter = Greeter {
            greetd: Arc::new(Mutex::new(greetd)),
            runtime,
            bus,
            sessions,
            session: 0,
            chosen: false,
            remembered,
            people,
            user: None,
            name,
            answer,
            stage: Stage::Choose,
            message,
            note: None,
            busy: false,
            machine: false,
            power: None,
            _subscriptions: subscriptions,
        };
        match first {
            Some(user) => greeter.choose(user, window, cx),
            None => greeter.name.update(cx, |input, cx| input.focus(window, cx)),
        }
        greeter
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
                        Ok(After::Cancelled) => greeter.choose_again(window, cx),
                        // greetd starts the session when the greeter ends
                        Ok(After::Started) => cx.quit(),
                        Err(e) => {
                            greeter.message = Some(format!("{e:#}"));
                            greeter.choose_again(window, cx);
                        }
                    }
                    cx.notify();
                });
            },
        )
        .detach();
    }

    fn choose_again(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stage = Stage::Choose;
        self.user = None;
        self.name.update(cx, |input, cx| input.focus(window, cx));
    }

    fn next(&mut self, auth: Auth, window: &mut Window, cx: &mut Context<Self>) {
        match auth {
            Auth::Prompt { text, secret } => {
                // PAM's prompt ("Password: ") is the field's placeholder
                let hint = text.trim().trim_end_matches(':').to_string();
                self.answer.update(cx, |input, cx| {
                    input.set_masked(secret, window, cx);
                    input.set_placeholder(hint, window, cx);
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
                let (Some(session), Some(user)) = (
                    self.sessions
                        .get(self.session)
                        .filter(|s| s.unsupported.is_none())
                        .cloned(),
                    self.user.clone(),
                ) else {
                    self.message = Some("No Wayland desktop is installed.".into());
                    return;
                };
                let remembered = self.remembered.clone();
                self.stage = Stage::Started;
                self.greetd(window, cx, move |g| {
                    g.start(&session)?;
                    // the session starts either way; the greeter's journal keeps the line
                    if let Err(e) = remembered.set(&user, &session.id) {
                        eprintln!(
                            "legionctl-greeter: the last desktop of {user} is not kept: {e:#}"
                        );
                    }
                    Ok(After::Started)
                });
            }
            Auth::Refused(why) => {
                self.message = Some(why);
                // a new login of the same user: PAM asks again
                self.login(window, cx);
            }
        }
    }

    /// Log `user` in: their last desktop is chosen (unless one was chosen here), then greetd asks PAM.
    pub fn choose(&mut self, user: String, window: &mut Window, cx: &mut Context<Self>) {
        if user.is_empty() {
            self.message = Some("Type a user name.".into());
            cx.notify();
            return;
        }
        if !self.chosen {
            match self.remembered.session(&user) {
                // the user's last desktop, else the first (not the one of the user before on this screen)
                Ok(last) => {
                    self.session = self
                        .sessions
                        .iter()
                        .position(|s| Some(&s.id) == last.as_ref() && s.unsupported.is_none())
                        .unwrap_or(0);
                }
                Err(e) => self.message = Some(format!("{e:#}")),
            }
        }
        self.user = Some(user);
        self.login(window, cx);
    }

    fn login(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(user) = self.user.clone() else {
            return;
        };
        self.greetd(window, cx, move |g| {
            g.create_session(&user).map(After::Auth)
        });
    }

    fn answer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let answer = self.answer.read(cx).value().to_string();
        self.answer
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.message = None;
        self.greetd(window, cx, move |g| {
            g.respond(Some(answer)).map(After::Auth)
        });
    }

    /// Back to the choice of user (Esc): greetd cancels the login in progress.
    pub fn back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.stage, Stage::Prompt { .. } | Stage::Waiting(_)) && !self.busy {
            self.message = None;
            self.greetd(window, cx, |g| g.cancel().map(|()| After::Cancelled));
        }
    }

    /// Choose session `i` (an X11 session cannot be chosen); the focus goes back to the field.
    fn pick(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.sessions[i].unsupported.is_none() {
            self.session = i;
            self.chosen = true;
        }
        let field = match self.stage {
            Stage::Prompt { .. } => &self.answer,
            _ => &self.name,
        };
        field.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    /// `method` of `interface` at `destination` on the system bus with `body`, on the runtime; `done` gets the
    /// answer or the error.
    fn call<B, R>(
        &mut self,
        (destination, path, interface, method): (
            &'static str,
            &'static str,
            &'static str,
            &'static str,
        ),
        body: B,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Greeter, Result<R>) + 'static,
    ) where
        B: Serialize + zbus::zvariant::DynamicType + Send + Sync + 'static,
        R: serde::de::DeserializeOwned + zbus::zvariant::Type + Send + 'static,
    {
        let (runtime, bus) = (self.runtime.clone(), self.bus.clone());
        let task = cx.background_spawn(async move {
            runtime.block_on(async move {
                let conn = zbus::connection::Builder::address(bus.as_str())?
                    .build()
                    .await?;
                let proxy = zbus::Proxy::new(&conn, destination, path, interface).await?;
                let answer: R = proxy.call(method, &body).await?;
                anyhow::Ok(answer)
            })
        });
        self.busy = true;
        cx.spawn(async move |this: WeakEntity<Greeter>, cx| {
            let result = task.await;
            let _ = this.update(cx, |greeter, cx| {
                greeter.busy = false;
                done(greeter, result);
                cx.notify();
            });
        })
        .detach();
    }

    /// A machine choice through the daemon; polkit allows it for the user `greeter`.
    fn act(&mut self, action: &'static str, cx: &mut Context<Self>) {
        const DAEMON: (&str, &str, &str) = (
            "org.legionctl.System1",
            "/org/legionctl/System1",
            "org.legionctl.System1",
        );
        let (d, p, i) = DAEMON;
        let done = |g: &mut Greeter, r: Result<Vec<String>>| {
            g.note = Some(match r {
                Ok(lines) if lines.is_empty() => "No change.".into(),
                Ok(lines) => lines.join("; "),
                Err(e) => sentence(&e),
            });
        };
        if let Some(choice) = action.strip_prefix("desktop-") {
            self.call(
                (d, p, i, "SetPrimaryGpu"),
                (choice.to_string(), false),
                cx,
                done,
            );
        } else if let Some(user) = self.target(cx) {
            self.call(
                (d, p, i, "GreeterAct"),
                (user, action.to_string()),
                cx,
                done,
            );
        }
    }

    /// The user whom a machine choice is for: the chosen one, else the name typed; the user's buttons are disabled
    /// without one.
    fn target(&self, cx: &Context<Self>) -> Option<String> {
        self.user.clone().or_else(|| {
            Some(self.name.read(cx).value().trim().to_string()).filter(|u| !u.is_empty())
        })
    }

    /// The power button: the first push asks, the confirmation calls logind (polkit decides, with no prompt).
    pub fn power(&mut self, power: Power, cx: &mut Context<Self>) {
        self.power = None;
        self.call(
            (
                "org.freedesktop.login1",
                "/org/freedesktop/login1",
                "org.freedesktop.login1.Manager",
                power.method(),
            ),
            (false,),
            cx,
            move |g, r: Result<()>| {
                if let Err(e) = r {
                    g.note = Some(format!("{}: {}", power.label(), sentence(&e)));
                }
            },
        );
    }

    /// A person's avatar: the first letter of `user` in a circle.
    fn avatar(user: &str, size: f32, t: &Tokens) -> Div {
        div()
            .size(px(size))
            .flex_none()
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(t.accent.opacity(0.22))
            .border_1()
            .border_color(t.accent.opacity(0.5))
            .text_color(t.text)
            .text_size(px(size * 0.42))
            .font_weight(FontWeight::MEDIUM)
            .child(
                user.chars()
                    .next()
                    .map(|c| c.to_uppercase().to_string())
                    .unwrap_or_default(),
            )
    }

    fn error_line(&self, t: &Tokens) -> Div {
        // the line keeps its height, so the screen does not move when an error comes or goes
        div()
            .h(px(22.))
            .h_flex()
            .gap(S2)
            .text_size(T_SM)
            .text_color(t.critical)
            .children(self.message.as_ref().map(|m| {
                div()
                    .h_flex()
                    .gap(S2)
                    .child(theme::icon(IconName::TriangleAlert, px(16.), t.critical))
                    .child(m.clone())
            }))
    }

    fn person(user: &str, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let name = user.to_string();
        div()
            .id(SharedString::from(format!("person-{user}")))
            .v_flex()
            .items_center()
            .gap(S3)
            .p(S4)
            .w(px(152.))
            .rounded(R_LG)
            .cursor_pointer()
            .hover(|s| s.bg(t.card))
            .child(Self::avatar(user, 88., t))
            .child(
                div()
                    .text_size(T_MD)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.text)
                    .child(user.to_string()),
            )
            .on_click(cx.listener(move |g, _, window, cx| g.choose(name.clone(), window, cx)))
            .test_support()
    }

    /// The people and the user name field.
    fn choose_card(&self, col: Div, t: &Tokens, cx: &mut Context<Self>) -> Div {
        col.child(
            div().h_flex().gap(S3).justify_center().children(
                self.people
                    .iter()
                    .map(|p| Self::person(p, t, cx).into_any_element()),
            ),
        )
        .child(
            div()
                .w_full()
                .h_flex()
                .gap(S2)
                .child(
                    Input::new(&self.name)
                        .id("user")
                        .large()
                        .prefix(theme::icon(IconName::User, px(18.), t.muted)),
                )
                .child(
                    Button::new("go")
                        .primary()
                        .with_size(FIELD)
                        .icon(IconName::ArrowRight)
                        .loading(self.busy)
                        .tooltip("Log in (Enter)")
                        .accessibility_label("Log in")
                        .on_click(cx.listener(|g, _, window, cx| {
                            let user = g.name.read(cx).value().trim().to_string();
                            g.choose(user, window, cx);
                        })),
                ),
        )
        .child(self.error_line(t))
    }

    fn login_card(&self, t: &Tokens, cx: &mut Context<Self>) -> Div {
        let col = div().v_flex().items_center().gap(S4).w(px(440.));
        let Some(user) = self.user.clone() else {
            return self.choose_card(col, t, cx);
        };
        let head = div()
            .v_flex()
            .items_center()
            .gap(S3)
            .child(Self::avatar(&user, 128., t))
            .child(
                div()
                    .text_size(T_XL)
                    .font_semibold()
                    .text_color(t.text)
                    .child(user),
            );
        let body = match &self.stage {
            Stage::Prompt { .. } => div()
                .w_full()
                .h_flex()
                .gap(S2)
                .child(
                    Button::new("back")
                        .ghost()
                        .with_size(FIELD)
                        .icon(IconName::ArrowLeft)
                        .tooltip("Other user (Esc)")
                        .accessibility_label("Other user")
                        .disabled(self.busy)
                        .on_click(cx.listener(|g, _, window, cx| g.back(window, cx))),
                )
                .child(
                    Input::new(&self.answer)
                        .id("answer")
                        .large()
                        .disabled(self.busy),
                )
                .child(
                    Button::new("go")
                        .primary()
                        .with_size(FIELD)
                        .icon(IconName::ArrowRight)
                        .loading(self.busy)
                        .tooltip("Log in (Enter)")
                        .accessibility_label("Log in")
                        .on_click(cx.listener(|g, _, window, cx| g.answer(window, cx))),
                ),
            Stage::Waiting(text) => div()
                .h(px(40.))
                .h_flex()
                .gap(S2)
                .text_size(T_MD)
                .text_color(t.text2)
                .child(theme::icon(IconName::FingerprintPattern, px(20.), t.accent))
                .child(text.clone()),
            Stage::Started => div()
                .h(px(40.))
                .h_flex()
                .gap(S2)
                .text_size(T_MD)
                .text_color(t.text2)
                .child(Spinner::new().color(t.accent))
                .child(format!(
                    "Starting {}",
                    self.sessions
                        .get(self.session)
                        .map_or("", |s| s.name.as_str())
                )),
            // the login of this user is not asked yet (greetd is busy)
            Stage::Choose => div()
                .h(px(40.))
                .h_flex()
                .child(Spinner::new().color(t.accent)),
        };
        col.child(head).child(body).child(self.error_line(t))
    }

    fn sessions_row(&self, cx: &mut Context<Self>) -> Div {
        div()
            .h_flex()
            .gap(S2)
            .justify_center()
            .flex_wrap()
            .children(self.sessions.iter().enumerate().map(|(i, s)| {
                let chip = Button::new(SharedString::from(format!("session-{}", s.id)))
                    .label(s.name.clone())
                    .icon(IconName::Monitor)
                    .selected(i == self.session)
                    .on_click(cx.listener(move |g, _, window, cx| g.pick(i, window, cx)));
                match &s.unsupported {
                    // shown, with why, and not chosen: the user sees that the desktop is there
                    Some(why) => chip.ghost().disabled(true).tooltip(why.clone()),
                    None if i == self.session => chip.primary(),
                    None => chip.ghost(),
                }
            }))
    }

    fn machine_panel(&self, t: &Tokens, cx: &mut Context<Self>) -> Div {
        let no_user = self.target(cx).is_none();
        theme::card(t)
            .w(px(420.))
            .children(MACHINE.iter().map(|&(row, icon, buttons)| {
                div()
                    .h_flex()
                    .gap(S3)
                    .child(theme::icon(icon, px(18.), t.muted))
                    .child(
                        div()
                            .w(px(120.))
                            .text_size(T_SM)
                            .text_color(t.text2)
                            .child(row),
                    )
                    .children(buttons.iter().map(|&(action, label)| {
                        Button::new(action)
                            .label(label)
                            .small()
                            .disabled(self.busy || (no_user && !action.starts_with("desktop-")))
                            .on_click(cx.listener(move |g, _, _, cx| g.act(action, cx)))
                    }))
            }))
    }

    fn bar(&self, t: &Tokens, cx: &mut Context<Self>) -> Div {
        let right = match self.power {
            Some(power) => div()
                .h_flex()
                .gap(S2)
                .child(
                    div()
                        .text_size(T_MD)
                        .text_color(t.text)
                        .child(format!("{}?", power.label())),
                )
                .child(
                    Button::new("power-confirm")
                        .danger()
                        .label(power.label())
                        .on_click(cx.listener(move |g, _, _, cx| g.power(power, cx))),
                )
                .child(
                    Button::new("power-cancel")
                        .ghost()
                        .label("Cancel")
                        .on_click(cx.listener(|g, _, _, cx| {
                            g.power = None;
                            cx.notify();
                        })),
                ),
            None => div().h_flex().gap(S2).children(
                [
                    ("suspend", IconName::Moon, Power::Suspend),
                    ("restart", IconName::RotateCcw, Power::Restart),
                    ("power-off", IconName::Power, Power::Off),
                ]
                .map(|(id, icon, power)| {
                    Button::new(id)
                        .ghost()
                        .large()
                        .icon(icon)
                        .label(power.label())
                        .disabled(self.busy)
                        .on_click(cx.listener(move |g, _, _, cx| {
                            g.power = Some(power);
                            cx.notify();
                        }))
                }),
            ),
        };
        div()
            .w_full()
            .h_flex()
            .justify_between()
            .p(S5)
            .child(
                Button::new("machine")
                    .ghost()
                    .large()
                    .icon(IconName::SlidersHorizontal)
                    .label("Before login")
                    .selected(self.machine)
                    .on_click(cx.listener(|g, _, _, cx| {
                        g.machine = !g.machine;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .text_size(T_SM)
                    .text_color(t.text2)
                    .children(self.note.clone()),
            )
            .child(right)
    }
}

impl Render for Greeter {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Tokens::of(cx);
        let started = self.stage == Stage::Started;
        div()
            .id("greeter")
            .size_full()
            .v_flex()
            .bg(t.window)
            .text_color(t.text)
            .capture_key_down(cx.listener(|g, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    if g.power.take().is_none() {
                        g.back(window, cx);
                    }
                    cx.notify();
                }
            }))
            .child(
                div()
                    .flex_1()
                    .v_flex()
                    .items_center()
                    .justify_center()
                    .gap(S5)
                    .child(self.login_card(&t, cx))
                    .when(!started, |d| d.child(self.sessions_row(cx))),
            )
            .when(self.machine && !started, |d| {
                d.child(div().px(S4).child(self.machine_panel(&t, cx)))
            })
            .when(!started, |d| d.child(self.bar(&t, cx)))
    }
}
