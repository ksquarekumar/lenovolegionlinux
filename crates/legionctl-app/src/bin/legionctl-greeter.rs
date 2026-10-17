//! The greeter binary: greetd runs `cage -s -m last -- env -u DISPLAY FONTCONFIG_FILE=<its fonts.conf>
//! legionctl-greeter` as its user `greeter` (note 21).

use gpui_kit::{AppContext, WindowOptions};
use legionctl_app::greeter::{
    Greetd, Greeter, GreeterAssets, REMEMBERED, Remembered, Setup, WAYLAND_SESSIONS, X11_SESSIONS,
    people, sessions,
};

fn main() -> anyhow::Result<()> {
    let socket = std::env::var_os("GREETD_SOCK")
        .ok_or_else(|| anyhow::anyhow!("GREETD_SOCK is not set: greetd starts the greeter"))?;
    let greetd = Greetd::connect(socket.as_ref())?;
    let setup = Setup {
        sessions: sessions(WAYLAND_SESSIONS.as_ref(), X11_SESSIONS.as_ref()),
        people: people("/etc/passwd".as_ref()),
        remembered: Remembered(REMEMBERED.into()),
        // the D-Bus specification's address of the system bus when the variable is not set
        bus: std::env::var("DBUS_SYSTEM_BUS_ADDRESS")
            .unwrap_or_else(|_| "unix:path=/run/dbus/system_bus_socket".into()),
    };
    // bus calls run here; the GPUI executor awaits them
    let runtime = tokio::runtime::Runtime::new()?;
    let handle = runtime.handle().clone();
    gpui_kit::application()
        .with_assets(GreeterAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            let options = WindowOptions {
                app_id: Some("legionctl-greeter".into()),
                ..WindowOptions::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| Greeter::new(greetd, setup, handle, window, cx))
            })
            .expect("opening the window");
        });
    drop(runtime);
    Ok(())
}
