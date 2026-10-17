//! The greeter (todo-83) against a stand-in greetd on a unix socket that speaks greetd's framing (a u32 length in
//! native byte order, then JSON) and keeps greetd's session rules: one session at a time (a failed one must be
//! cancelled first), a start only after success. Behind it a small PAM: the password `right`, and in fingerprint mode
//! an info message first (the reader), then the password after the reader gives up.

use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AppContext, Entity, SharedString, TestAppContext, VisualTestContext, WindowHandle,
    WindowOptions,
};
use legionctl_app::greeter::{
    Auth, Greetd, Greeter, Power, Remembered, Session, Setup, Stage, sessions,
};
use serde_json::{Value, json};

#[path = "common/process.rs"]
mod process;
#[path = "common/scratch.rs"]
mod scratch;

use process::Kill;
use scratch::Scratch;

type Requests = Arc<Mutex<Vec<Value>>>;

/// greetd's session state in the stand-in.
#[derive(PartialEq)]
enum Pam {
    None,
    Reader,
    Password,
    Failed,
    Authenticated,
}

/// A stand-in greetd at `greetd.sock` in a new folder; returns the folder and every request it got, in order. With
/// `hold`, its answer to `start_session` waits for a message on it (the moment between the start and the greeter's
/// end).
fn greetd(tag: &str, fingerprint: bool, hold: Option<Receiver<()>>) -> (Scratch, Requests) {
    let dir = Scratch::new(
        std::env::temp_dir().join(format!("legionctl-greeter-{tag}-{}", std::process::id())),
    );
    let listener = UnixListener::bind(dir.join("greetd.sock")).unwrap();
    let requests = Requests::default();
    let seen = requests.clone();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut pam = Pam::None;
        loop {
            let mut len = [0u8; 4];
            if stream.read_exact(&mut len).is_err() {
                return;
            }
            // a wrong length (another byte order) closes the connection, as a broken frame does in greetd
            let len = u32::from_ne_bytes(len) as usize;
            if len > 1 << 20 {
                return;
            }
            let mut body = vec![0u8; len];
            stream.read_exact(&mut body).unwrap();
            let request: Value = serde_json::from_slice(&body).unwrap();
            seen.lock().unwrap().push(request.clone());
            let secret = json!({"type": "auth_message", "auth_message_type": "secret", "auth_message": "Password: "});
            let error = |kind: &str, why: &str| json!({"type": "error", "error_type": kind, "description": why});
            let reply = match (request["type"].as_str().unwrap(), &pam) {
                ("create_session", Pam::None) if fingerprint => {
                    pam = Pam::Reader;
                    json!({"type": "auth_message", "auth_message_type": "info", "auth_message": "Place your finger on the fingerprint reader"})
                }
                ("create_session", Pam::None) => {
                    pam = Pam::Password;
                    secret
                }
                ("create_session", _) => error("error", "a session is already being configured"),
                ("post_auth_message_response", Pam::Reader) if request["response"].is_null() => {
                    pam = Pam::Password;
                    secret
                }
                ("post_auth_message_response", Pam::Password) if request["response"] == "right" => {
                    pam = Pam::Authenticated;
                    json!({"type": "success"})
                }
                ("post_auth_message_response", Pam::Password) => {
                    pam = Pam::Failed;
                    error("auth_error", "Authentication failure")
                }
                ("start_session", Pam::Authenticated) => {
                    if let Some(hold) = &hold {
                        hold.recv().unwrap();
                    }
                    json!({"type": "success"})
                }
                ("cancel_session", _) => {
                    pam = Pam::None;
                    json!({"type": "success"})
                }
                _ => error("error", "out of order"),
            };
            let body = serde_json::to_vec(&reply).unwrap();
            stream
                .write_all(&u32::try_from(body.len()).unwrap().to_ne_bytes())
                .unwrap();
            stream.write_all(&body).unwrap();
        }
    });
    (dir, requests)
}

fn gnome() -> Session {
    Session {
        name: "GNOME".into(),
        id: "gnome".into(),
        exec: vec!["/usr/bin/gnome-session".into()],
        desktops: "GNOME".into(),
        unsupported: None,
    }
}

fn types(requests: &Requests) -> Vec<String> {
    requests
        .lock()
        .unwrap()
        .iter()
        .map(|r| r["type"].as_str().unwrap().to_string())
        .collect()
}

fn password() -> Auth {
    Auth::Prompt {
        text: "Password: ".into(),
        secret: true,
    }
}

#[test]
fn a_password_login_starts_the_session_with_its_command_and_environment() {
    let (stand_in, requests) = greetd("password", false, None);
    let socket = stand_in.join("greetd.sock");
    let mut g = Greetd::connect(&socket).unwrap();
    assert_eq!(g.create_session("alice").unwrap(), password());
    assert_eq!(g.respond(Some("right".into())).unwrap(), Auth::Done);
    g.start(&gnome()).unwrap();
    assert_eq!(
        *requests.lock().unwrap(),
        vec![
            json!({"type": "create_session", "username": "alice"}),
            json!({"type": "post_auth_message_response", "response": "right"}),
            json!({"type": "start_session", "cmd": ["/usr/bin/gnome-session"], "env": [
                "XDG_SESSION_TYPE=wayland", "XDG_SESSION_DESKTOP=gnome", "XDG_CURRENT_DESKTOP=GNOME"]}),
        ]
    );
}

/// After a wrong password greetd keeps the failed session: the client cancels it, so the next try is a new session.
#[test]
fn a_wrong_password_is_refused_and_the_next_try_logs_in() {
    let (stand_in, requests) = greetd("retry", false, None);
    let socket = stand_in.join("greetd.sock");
    let mut g = Greetd::connect(&socket).unwrap();
    g.create_session("alice").unwrap();
    assert_eq!(
        g.respond(Some("wrong".into())).unwrap(),
        Auth::Refused("Wrong password. Try again.".into())
    );
    assert_eq!(g.create_session("alice").unwrap(), password());
    assert_eq!(g.respond(Some("right".into())).unwrap(), Auth::Done);
    assert_eq!(
        types(&requests),
        [
            "create_session",
            "post_auth_message_response",
            "cancel_session",
            "create_session",
            "post_auth_message_response"
        ]
    );
}

/// `pam_fprintd` first says where to put the finger and waits; the client shows it and answers with nothing, then
/// PAM asks for the password (the reader gave up).
#[test]
fn a_fingerprint_message_comes_first_then_the_password() {
    let (stand_in, requests) = greetd("fingerprint", true, None);
    let socket = stand_in.join("greetd.sock");
    let mut g = Greetd::connect(&socket).unwrap();
    assert_eq!(
        g.create_session("alice").unwrap(),
        Auth::Info("Place your finger on the fingerprint reader".into())
    );
    assert_eq!(g.respond(None).unwrap(), password());
    assert_eq!(g.respond(Some("right".into())).unwrap(), Auth::Done);
    assert_eq!(
        requests.lock().unwrap()[1],
        json!({"type": "post_auth_message_response", "response": null})
    );
}

#[test]
fn a_cancelled_login_lets_another_user_start_one() {
    let (stand_in, requests) = greetd("cancel", false, None);
    let socket = stand_in.join("greetd.sock");
    let mut g = Greetd::connect(&socket).unwrap();
    g.create_session("alice").unwrap();
    g.cancel().unwrap();
    assert_eq!(g.create_session("bob").unwrap(), password());
    assert_eq!(
        types(&requests),
        ["create_session", "cancel_session", "create_session"]
    );
}

fn desktop(dir: &Path, id: &str, body: &str) {
    std::fs::write(
        dir.join(format!("{id}.desktop")),
        format!("[Desktop Entry]\n{body}\n"),
    )
    .unwrap();
}

/// GNOME, COSMIC, a hidden session and an X11 session in folders next to `socket`.
fn found(socket: &Path) -> Vec<Session> {
    let dir = socket.with_file_name("wayland-sessions");
    let x11 = socket.with_file_name("xsessions");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&x11).unwrap();
    desktop(
        &dir,
        "cosmic",
        "Name=COSMIC\nExec=start-cosmic\nDesktopNames=COSMIC;",
    );
    desktop(
        &dir,
        "gnome",
        "Name=GNOME\nExec=/usr/bin/gnome-session\nDesktopNames=GNOME",
    );
    desktop(
        &dir,
        "gnome-wayland",
        "Name=GNOME on Wayland\nExec=/usr/bin/gnome-session\nNoDisplay=true",
    );
    desktop(
        &x11,
        "gnome-flashback-metacity",
        "Name=GNOME Flashback (Metacity)\nExec=gnome-session --session=gnome-flashback-metacity",
    );
    sessions(&dir, &x11)
}

/// What the stand-in logind and daemon on the private bus were asked, in order.
type Calls = Arc<Mutex<Vec<String>>>;

struct Logind(Calls);

#[zbus::interface(name = "org.freedesktop.login1.Manager")]
impl Logind {
    fn power_off(&self, interactive: bool) {
        self.0
            .lock()
            .unwrap()
            .push(format!("PowerOff {interactive}"));
    }
    fn reboot(&self, interactive: bool) {
        self.0.lock().unwrap().push(format!("Reboot {interactive}"));
    }
    fn suspend(&self, interactive: bool) {
        self.0
            .lock()
            .unwrap()
            .push(format!("Suspend {interactive}"));
    }
}

struct Daemon(Calls);

struct Systemd(Calls);

#[zbus::interface(name = "org.freedesktop.systemd1.Manager")]
impl Systemd {
    fn start_unit(&self, name: &str, mode: &str) -> zbus::zvariant::OwnedObjectPath {
        self.0
            .lock()
            .unwrap()
            .push(format!("StartUnit {name} {mode}"));
        zbus::zvariant::OwnedObjectPath::try_from("/org/freedesktop/systemd1/job/1").unwrap()
    }
}

#[zbus::interface(name = "org.legionctl.System1")]
impl Daemon {
    fn greeter_act(&self, user: &str, action: &str) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .push(format!("GreeterAct {user} {action}"));
        vec![format!("{action} for {user}")]
    }
}

/// A private bus in `dir` with the stand-in logind, daemon and systemd; returns their calls, the bus's address and what keeps
/// them running.
fn bus(dir: &Path, runtime: &tokio::runtime::Runtime) -> (Calls, String, Kill, zbus::Connection) {
    let path = dir.join("bus");
    let daemon = Kill::spawn(
        std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--nopidfile"])
            .arg(format!("--address=unix:path={}", path.display())),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(Instant::now() < deadline, "the private bus did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
    let address = format!("unix:path={}", path.display());
    let calls = Calls::default();
    let conn = runtime.block_on(async {
        zbus::connection::Builder::address(address.as_str())
            .unwrap()
            .name("org.freedesktop.login1")
            .unwrap()
            .name("org.legionctl.System1")
            .unwrap()
            .name("org.freedesktop.systemd1")
            .unwrap()
            .serve_at("/org/freedesktop/systemd1", Systemd(calls.clone()))
            .unwrap()
            .serve_at("/org/freedesktop/login1", Logind(calls.clone()))
            .unwrap()
            .serve_at("/org/legionctl/System1", Daemon(calls.clone()))
            .unwrap()
            .build()
            .await
            .unwrap()
    });
    (calls, address, daemon, conn)
}

/// The greeter's window on `greetd` with `setup`, and its screen.
fn open_screen(
    cx: &mut TestAppContext,
    greetd: Greetd,
    setup: Setup,
    handle: tokio::runtime::Handle,
) -> (Entity<Greeter>, VisualTestContext) {
    let window: WindowHandle<gpui_kit::base::Root> = cx.update(|cx| {
        let (window, _) = gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| Greeter::new(greetd, setup, handle, window, cx))
        })
        .unwrap();
        window.downcast().unwrap()
    });
    let screen: Entity<Greeter> = window
        .update(cx, |root, _, _| root.view().clone().downcast().unwrap())
        .unwrap();
    (screen, VisualTestContext::from_window(window.into(), cx))
}

fn setup(found: &[Session], people: &[&str], remembered: &Remembered, bus: &str) -> Setup {
    Setup {
        sessions: found.to_vec(),
        people: Ok(people.iter().map(|p| (*p).to_string()).collect()),
        remembered: remembered.clone(),
        bus: bus.into(),
    }
}

/// Run the screen until `ready`, at most 10 s.
fn wait(
    cx: &mut VisualTestContext,
    screen: &Entity<Greeter>,
    what: &str,
    ready: &dyn Fn(&Greeter) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        cx.run_until_parked();
        if screen.read_with(cx, |g, _| ready(g)) {
            return;
        }
        let state = screen.read_with(cx, |g, _| {
            format!("{:?} {:?} {:?}", g.stage, g.message, g.note)
        });
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {state}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn prompt(g: &Greeter) -> bool {
    matches!(g.stage, Stage::Prompt { secret: true, .. }) && !g.busy
}

/// The password `answer`, Enter, in the password field that has the focus.
fn type_password(cx: &mut VisualTestContext, answer: &str) {
    cx.update(|window, cx| {
        window.input(answer, cx);
        window.press("enter", cx);
    });
}

/// Whether the element `id` is on the screen now.
fn shown(cx: &mut VisualTestContext, id: &str) -> bool {
    cx.update(|window, cx| {
        window.render_frame(cx);
        window
            .try_find(SharedString::from(id.to_string()))
            .is_some()
    })
}

/// The login of a person chosen with the mouse: GNOME first and chosen, the hidden session left out, the X11 session
/// shown and not chosen; a click on COSMIC, a click on alice, her password, Enter: greetd starts COSMIC. After the
/// logout the next screen asks alice's password at once (she logged in last) with COSMIC chosen; Esc goes back to the
/// people, bob gets GNOME (todo-150).
#[gpui_kit::test]
fn a_chosen_person_logs_in_to_the_chosen_session_and_the_next_screen_remembers_both(
    cx: &mut TestAppContext,
) {
    let (stand_in, requests) = greetd("screen", false, None);
    let socket = stand_in.join("greetd.sock");
    let found = found(&socket);
    let remembered = Remembered(socket.with_file_name("last-sessions"));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    cx.update(gpui_kit::init);
    let (screen, mut vcx) = open_screen(
        cx,
        Greetd::connect(&socket).unwrap(),
        setup(
            &found,
            &["alice", "bob"],
            &remembered,
            "unix:path=/nonexistent",
        ),
        runtime.handle().clone(),
    );
    let vcx = &mut vcx;
    screen.read_with(vcx, |g, _| {
        let names: Vec<(&str, bool)> = g
            .sessions
            .iter()
            .map(|s| (s.name.as_str(), s.unsupported.is_some()))
            .collect();
        assert_eq!(
            names,
            [
                ("GNOME", false),
                ("COSMIC", false),
                ("GNOME Flashback (Metacity)", true)
            ]
        );
        assert_eq!((g.session, &g.stage, &g.user), (0, &Stage::Choose, &None));
    });
    vcx.update(|window, cx| window.click("session-gnome-flashback-metacity", cx));
    assert_eq!(
        screen.read_with(vcx, |g, _| g.session),
        0,
        "an X11 session cannot be chosen"
    );
    vcx.update(|window, cx| {
        window.click("session-cosmic", cx);
        window.click("person-alice", cx);
    });
    wait(vcx, &screen, "alice's password prompt", &prompt);
    type_password(vcx, "right");
    wait(vcx, &screen, "the session start", &|g| {
        g.stage == Stage::Started && !g.busy
    });
    assert_eq!(
        requests.lock().unwrap().clone(),
        vec![
            json!({"type": "create_session", "username": "alice"}),
            json!({"type": "post_auth_message_response", "response": "right"}),
            json!({"type": "start_session", "cmd": ["start-cosmic"], "env": [
                "XDG_SESSION_TYPE=wayland", "XDG_SESSION_DESKTOP=cosmic", "XDG_CURRENT_DESKTOP=COSMIC"]}),
        ]
    );

    after_the_logout(cx, &found, &remembered, &runtime);
    drop(runtime);
}

/// After the logout greetd starts a new greeter: alice is asked at once, with COSMIC; Esc, bob, GNOME.
fn after_the_logout(
    cx: &mut TestAppContext,
    found: &[Session],
    remembered: &Remembered,
    runtime: &tokio::runtime::Runtime,
) {
    let (stand_in, requests) = greetd("again", false, None);
    let (screen, mut vcx) = open_screen(
        cx,
        Greetd::connect(&stand_in.join("greetd.sock")).unwrap(),
        setup(
            found,
            &["alice", "bob"],
            remembered,
            "unix:path=/nonexistent",
        ),
        runtime.handle().clone(),
    );
    let vcx = &mut vcx;
    wait(vcx, &screen, "alice's prompt without a click", &prompt);
    screen.read_with(vcx, |g, _| {
        assert_eq!((g.user.as_deref(), g.session), (Some("alice"), 1));
    });
    vcx.update(|window, cx| window.press("escape", cx));
    wait(vcx, &screen, "the people again", &|g| {
        g.stage == Stage::Choose && !g.busy
    });
    vcx.update(|window, cx| window.click("person-bob", cx));
    wait(vcx, &screen, "bob's prompt", &prompt);
    screen.read_with(vcx, |g, _| {
        assert_eq!(g.session, 0, "bob's own last desktop: none");
    });
    type_password(vcx, "right");
    wait(vcx, &screen, "bob's session start", &|g| {
        g.stage == Stage::Started && !g.busy
    });
    assert_eq!(
        types(&requests),
        [
            "create_session",
            "cancel_session",
            "create_session",
            "post_auth_message_response",
            "start_session"
        ]
    );
    assert_eq!(
        requests.lock().unwrap()[4]["cmd"],
        json!(["/usr/bin/gnome-session"])
    );
}

/// The boot of 2026-10-17: after a good password, while greetd starts the desktop and the greeter has not ended, the
/// screen showed its login form again ("logged in, then back to the greeter"). Now it shows the start, and no field,
/// session or power control.
#[gpui_kit::test]
fn after_a_good_password_the_screen_shows_the_start_and_no_login_form(cx: &mut TestAppContext) {
    let (release, hold) = channel();
    let (stand_in, _) = greetd("handover", false, Some(hold));
    let socket = stand_in.join("greetd.sock");
    let found = found(&socket);
    let remembered = Remembered(socket.with_file_name("last-sessions"));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    cx.update(gpui_kit::init);
    let (screen, mut vcx) = open_screen(
        cx,
        Greetd::connect(&socket).unwrap(),
        setup(&found, &["alice"], &remembered, "unix:path=/nonexistent"),
        runtime.handle().clone(),
    );
    let vcx = &mut vcx;
    wait(vcx, &screen, "the only person's prompt at once", &prompt);
    assert!(shown(vcx, "answer") && shown(vcx, "power-off"));
    type_password(vcx, "right");
    // one task at a time: greetd's answer to the start waits for `release`, and the test's executor runs the call on
    // this thread
    while screen.read_with(vcx, |g, _| g.stage != Stage::Started) {
        assert!(vcx.executor().tick(), "the screen stopped before the start");
    }
    for control in [
        "answer",
        "user",
        "go",
        "back",
        "session-gnome",
        "power-off",
        "machine",
    ] {
        assert!(
            !shown(vcx, control),
            "{control} is shown after the password"
        );
    }
    release.send(()).unwrap();
    wait(vcx, &screen, "greetd's answer", &|g| !g.busy);
    drop(runtime);
}

/// A wrong password: one short line under the field, the field again for the next try, which logs in.
#[gpui_kit::test]
fn a_wrong_password_says_so_by_the_field_and_the_next_try_logs_in(cx: &mut TestAppContext) {
    let (stand_in, requests) = greetd("wrong", false, None);
    let socket = stand_in.join("greetd.sock");
    let found = found(&socket);
    let remembered = Remembered(socket.with_file_name("last-sessions"));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    cx.update(gpui_kit::init);
    let (screen, mut vcx) = open_screen(
        cx,
        Greetd::connect(&socket).unwrap(),
        setup(&found, &["alice"], &remembered, "unix:path=/nonexistent"),
        runtime.handle().clone(),
    );
    let vcx = &mut vcx;
    wait(vcx, &screen, "the prompt", &prompt);
    type_password(vcx, "wrong");
    wait(vcx, &screen, "the prompt again", &|g| {
        prompt(g) && g.message.is_some()
    });
    assert_eq!(
        screen.read_with(vcx, |g, _| g.message.clone()).as_deref(),
        Some("Wrong password. Try again.")
    );
    assert!(shown(vcx, "answer"));
    type_password(vcx, "right");
    wait(vcx, &screen, "the start", &|g| {
        g.stage == Stage::Started && !g.busy
    });
    assert_eq!(screen.read_with(vcx, |g, _| g.message.clone()), None);
    assert_eq!(
        types(&requests),
        [
            "create_session",
            "post_auth_message_response",
            "cancel_session",
            "create_session",
            "post_auth_message_response",
            "start_session"
        ]
    );
    drop(runtime);
}

/// Power off and restart ask once more; Esc takes the question back; the confirmation calls logind with no prompt
/// (`interactive` false: before login no agent can ask).
#[gpui_kit::test]
fn a_power_action_asks_once_more_then_calls_logind(cx: &mut TestAppContext) {
    let (stand_in, _) = greetd("power", false, None);
    let socket = stand_in.join("greetd.sock");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (calls, address, _bus, _conn) = bus(&stand_in, &runtime);
    let found = found(&socket);
    let remembered = Remembered(socket.with_file_name("last-sessions"));
    cx.update(gpui_kit::init);
    let (screen, mut vcx) = open_screen(
        cx,
        Greetd::connect(&socket).unwrap(),
        setup(&found, &["alice"], &remembered, &address),
        runtime.handle().clone(),
    );
    let vcx = &mut vcx;
    wait(vcx, &screen, "the prompt", &prompt);
    vcx.update(|window, cx| window.click("power-off", cx));
    assert_eq!(screen.read_with(vcx, |g, _| g.power), Some(Power::Off));
    vcx.update(|window, cx| window.press("escape", cx));
    assert_eq!(screen.read_with(vcx, |g, _| g.power), None);
    vcx.update(|window, cx| {
        window.render_frame(cx);
        window.click("restart", cx);
    });
    vcx.update(|window, cx| {
        window.render_frame(cx);
        window.click("power-confirm", cx);
    });
    wait(vcx, &screen, "logind's answer", &|g| {
        !g.busy && g.power.is_none()
    });
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(*calls.lock().unwrap(), ["Reboot false"]);
    assert!(
        screen.read_with(vcx, |g, _| matches!(g.stage, Stage::Prompt { .. })),
        "Esc on the question kept the login"
    );
    drop(runtime);
}

/// A machine choice for a user (G-25): without a user its buttons do nothing and show no error after the click; with a
/// typed name the daemon gets the choice for that name and the screen shows its answer.
#[gpui_kit::test]
fn the_machine_choices_for_a_user_wait_for_a_user(cx: &mut TestAppContext) {
    let (stand_in, _) = greetd("machine", false, None);
    let socket = stand_in.join("greetd.sock");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (calls, address, _bus, _conn) = bus(&stand_in, &runtime);
    let found = found(&socket);
    let remembered = Remembered(socket.with_file_name("last-sessions"));
    cx.update(gpui_kit::init);
    let (screen, mut vcx) = open_screen(
        cx,
        Greetd::connect(&socket).unwrap(),
        setup(&found, &[], &remembered, &address),
        runtime.handle().clone(),
    );
    let vcx = &mut vcx;
    vcx.update(|window, cx| window.click("machine", cx));
    vcx.update(|window, cx| {
        window.render_frame(cx);
        window.click("vm-start", cx);
    });
    vcx.run_until_parked();
    assert_eq!(
        (
            calls.lock().unwrap().len(),
            screen.read_with(vcx, |g, _| g.note.clone())
        ),
        (0, None)
    );
    vcx.update(|window, cx| {
        window.click("user", cx);
        window.input("alice", cx);
    });
    vcx.update(|window, cx| {
        window.render_frame(cx);
        window.click("vm-start", cx);
    });
    wait(vcx, &screen, "the daemon's answer", &|g| g.note.is_some());
    assert_eq!(*calls.lock().unwrap(), ["GreeterAct alice vm-start"]);
    assert_eq!(
        screen.read_with(vcx, |g, _| g.note.clone()).as_deref(),
        Some("vm-start for alice")
    );
    drop(runtime);
}

/// The boot splash stays until the greeter shows (todo-200): legionctl's greetd unit only deactivates plymouth, and the
/// greeter asks systemd to start legionctl-splash-quit.service once its first frame is drawn, not before.
#[gpui_kit::test]
fn the_first_frame_ends_the_boot_splash(cx: &mut TestAppContext) {
    let (stand_in, _) = greetd("splash", false, None);
    let socket = stand_in.join("greetd.sock");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (calls, address, _bus, _conn) = bus(&stand_in, &runtime);
    let found = found(&socket);
    let remembered = Remembered(socket.with_file_name("last-sessions"));
    cx.update(gpui_kit::init);
    let (_screen, mut vcx) = open_screen(
        cx,
        Greetd::connect(&socket).unwrap(),
        setup(&found, &["alice"], &remembered, &address),
        runtime.handle().clone(),
    );
    let start = "StartUnit legionctl-splash-quit.service replace";
    let asked = |calls: &Calls| calls.lock().unwrap().iter().any(|c| c == start);
    // the first frame's callback runs before its draw: no call yet
    vcx.update(gpui_kit::Window::simulate_next_frame);
    std::thread::sleep(Duration::from_millis(300));
    assert!(!asked(&calls), "{:?}", calls.lock().unwrap());
    vcx.update(gpui_kit::Window::simulate_next_frame);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !asked(&calls) {
        assert!(Instant::now() < deadline, "{:?}", calls.lock().unwrap());
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(*calls.lock().unwrap(), [start], "one start, nothing else");
    drop(runtime);
}
