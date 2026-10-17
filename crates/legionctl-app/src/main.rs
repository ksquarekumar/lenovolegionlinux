use gpui_kit::component::{Theme, TitleBar};
use gpui_kit::{AppContext, Bounds, WindowBounds, WindowOptions, px, size};
use legionctl_api::client::Client;
use legionctl_app::{AppAssets, LegionApp, Tab, theme};

/// The window's decoration around its content (the kit's client-side shadow, 20 px on each side), so that the content
/// gets the size asked for (U3).
const DECORATION: f32 = 40.;

fn main() -> anyhow::Result<()> {
    let usage = "usage: legionctl-app [<view> [name=value...]]";
    let mut open = None;
    for word in std::env::args().skip(1) {
        match word.as_str() {
            w if w.starts_with('-') => anyhow::bail!(usage),
            // a view, and its parameters after it: `code-graph key=rust:.:add`
            w if w.contains('=') => match open.as_mut() {
                Some(Tab::View { params, .. }) => params.push(w.to_string()),
                _ => anyhow::bail!(usage),
            },
            w => {
                open = Some(Tab::View {
                    name: w.to_string(),
                    params: Vec::new(),
                });
            }
        }
    }
    desktop_display()?;
    let repo = std::env::current_dir()?;
    let client = Client::new("app")?;
    // operator calls run here; the GPUI executor awaits them
    let runtime = tokio::runtime::Runtime::new()?;
    let handle = runtime.handle().clone();
    gpui_kit::application()
        .with_assets(AppAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            legionctl_app::init(cx);
            // 70% of the monitor, at least the smallest content size at which every tab reads well
            // Wayland names no primary display: the first one
            let monitor = cx
                .primary_display()
                .or_else(|| cx.displays().into_iter().next())
                .map_or(size(px(1600.), px(1000.)), |d| d.bounds().size);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(
                        (monitor.width * 0.7).max(px(960. + DECORATION)),
                        (monitor.height * 0.7).max(px(600. + DECORATION)),
                    ),
                    cx,
                ))),
                window_min_size: Some(size(px(960. + DECORATION), px(600. + DECORATION))),
                app_id: Some("legionctl".into()),
                ..TitleBar::window_options()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                Theme::sync_system_appearance(Some(window), cx);
                theme::adopt(cx);
                // the desktop portal reports the dark/light choice after the window opens, and on each change
                window
                    .observe_window_appearance(|window, cx| {
                        Theme::sync_system_appearance(Some(window), cx);
                        theme::adopt(cx);
                    })
                    .detach();
                cx.new(|cx| LegionApp::new(client, handle, open, repo, window, cx))
            })
            .expect("opening the window");
        });
    drop(runtime);
    Ok(())
}

/// The Wayland desktop of this login. A shell started in an earlier desktop session (for example before a logout)
/// keeps that session's `WAYLAND_DISPLAY`; its socket is gone, so the one live desktop socket is used instead.
fn desktop_display() -> anyhow::Result<()> {
    let live = |path: &std::path::Path| std::os::unix::net::UnixStream::connect(path).is_ok();
    let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from) else {
        return Ok(());
    };
    let named = std::env::var_os("WAYLAND_DISPLAY").map(|d| runtime.join(d));
    if named.as_deref().is_some_and(live) {
        return Ok(());
    }
    let sockets: Vec<std::path::PathBuf> = std::fs::read_dir(&runtime)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_none()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("wayland-"))
                && live(p)
        })
        .collect();
    match sockets.as_slice() {
        [one] => {
            // SAFETY: no other thread runs yet
            unsafe { std::env::set_var("WAYLAND_DISPLAY", one.file_name().unwrap_or_default()) };
            Ok(())
        }
        [] => anyhow::bail!(
            "legionctl needs a desktop session. Log in to the desktop, then start legionctl from its menu."
        ),
        _ => anyhow::bail!(
            "More than one desktop session runs. Start legionctl from the menu of the desktop you use."
        ),
    }
}
