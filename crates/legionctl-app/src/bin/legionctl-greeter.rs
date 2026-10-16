//! The greeter binary: greetd runs `cage -s -- legionctl-greeter` as its user `greeter` (note 21).

use gpui_kit::{AppContext, WindowOptions};
use legionctl_app::greeter::{
    Greetd, Greeter, REMEMBERED, Remembered, WAYLAND_SESSIONS, X11_SESSIONS, sessions,
};

fn main() -> anyhow::Result<()> {
    let socket = std::env::var_os("GREETD_SOCK")
        .ok_or_else(|| anyhow::anyhow!("GREETD_SOCK is not set: greetd starts the greeter"))?;
    let greetd = Greetd::connect(socket.as_ref())?;
    let sessions = sessions(WAYLAND_SESSIONS.as_ref(), X11_SESSIONS.as_ref());
    let remembered = Remembered(REMEMBERED.into());
    // daemon calls run here; the GPUI executor awaits them
    let runtime = tokio::runtime::Runtime::new()?;
    let handle = runtime.handle().clone();
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            let options = WindowOptions {
                app_id: Some("legionctl-greeter".into()),
                ..WindowOptions::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| Greeter::new(greetd, sessions, remembered, handle, window, cx))
            })
            .expect("opening the window");
        });
    drop(runtime);
    Ok(())
}
