//! The greeter (todo-83) against a stand-in greetd on a unix socket that speaks greetd's framing (a u32 length in
//! native byte order, then JSON) and keeps greetd's session rules: one session at a time (a failed one must be
//! cancelled first), a start only after success. Behind it a small PAM: the password `right`, and in fingerprint mode
//! an info message first (the reader), then the password after the reader gives up.

use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AppContext, Entity, TestAppContext, VisualTestContext, WindowHandle, WindowOptions,
};
use legionctl_app::greeter::{Auth, Greetd, Greeter, Session, Stage, sessions};
use serde_json::{Value, json};

#[path = "common/scratch.rs"]
mod scratch;

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

/// A stand-in greetd at `greetd.sock` in a new folder; returns the folder and every request it got, in order.
fn greetd(tag: &str, fingerprint: bool) -> (Scratch, Requests) {
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
                ("start_session", Pam::Authenticated) => json!({"type": "success"}),
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
    let (stand_in, requests) = greetd("password", false);
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
    let (stand_in, requests) = greetd("retry", false);
    let socket = stand_in.join("greetd.sock");
    let mut g = Greetd::connect(&socket).unwrap();
    g.create_session("alice").unwrap();
    assert_eq!(
        g.respond(Some("wrong".into())).unwrap(),
        Auth::Refused("Authentication failure".into())
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
    let (stand_in, requests) = greetd("fingerprint", true);
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
    let (stand_in, requests) = greetd("cancel", false);
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

/// The screen with the keyboard and the mouse: GNOME is the first session and chosen; a click chooses COSMIC; the
/// user name, Enter, the password, Enter: greetd starts COSMIC's command.
#[gpui_kit::test]
fn the_screen_logs_in_with_the_keyboard_and_starts_the_chosen_session(cx: &mut TestAppContext) {
    let (stand_in, requests) = greetd("screen", false);
    let socket = stand_in.join("greetd.sock");
    let dir = socket.with_file_name("wayland-sessions");
    std::fs::create_dir_all(&dir).unwrap();
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
    let found = sessions(&dir);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let handle = runtime.handle().clone();
    let greetd = Greetd::connect(&socket).unwrap();
    cx.update(gpui_kit::init);
    let window: WindowHandle<gpui_kit::base::Root> = cx.update(|cx| {
        let (window, _) = gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| Greeter::new(greetd, found, handle, window, cx))
        })
        .unwrap();
        window.downcast().unwrap()
    });
    let screen: Entity<Greeter> = window
        .update(cx, |root, _, _| root.view().clone().downcast().unwrap())
        .unwrap();
    let mut vcx = VisualTestContext::from_window(window.into(), cx);
    let cx = &mut vcx;
    let wait = |cx: &mut VisualTestContext, what: &str, ready: &dyn Fn(&Greeter) -> bool| {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            if screen.read_with(cx, |g, _| ready(g)) {
                return;
            }
            let state = screen.read_with(cx, |g, _| format!("{:?} {:?}", g.stage, g.message));
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {state}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    };

    screen.read_with(cx, |g, _| {
        let names: Vec<&str> = g.sessions.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            ["GNOME", "COSMIC"],
            "GNOME first, hidden ones left out"
        );
        assert_eq!(g.session, 0);
    });
    cx.update(|window, cx| window.click("session-cosmic", cx));
    cx.update(|window, cx| {
        window.click("user", cx);
        window.input("alice", cx);
        window.press("enter", cx);
    });
    wait(cx, "the password prompt", &|g| {
        matches!(g.stage, Stage::Prompt { secret: true, .. })
    });
    cx.update(|window, cx| {
        window.input("right", cx);
        window.press("enter", cx);
    });
    wait(cx, "the session start", &|g| {
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
    drop(runtime);
}
