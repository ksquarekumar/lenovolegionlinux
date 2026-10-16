use gpui_kit::component::{Theme, TitleBar};
use gpui_kit::{AppContext, Bounds, WindowBounds, WindowOptions, px, size};
use legionctl_api::client::Client;
use legionctl_app::{AppAssets, LegionApp, Tab, theme};

/// The window's decoration around its content (the kit's client-side shadow, 20 px on each side), so that the content
/// gets the size asked for (U3).
const DECORATION: f32 = 40.;

fn main() -> anyhow::Result<()> {
    let usage = "usage: legionctl-app [<view>] [--dossier <name>]";
    let mut open = None;
    let mut dossier = None;
    let mut args = std::env::args().skip(1);
    while let Some(word) = args.next() {
        match word.as_str() {
            "--dossier" => dossier = Some(args.next().ok_or_else(|| anyhow::anyhow!(usage))?),
            w if w.starts_with('-') => anyhow::bail!(usage),
            w => {
                open = Some(Tab::View {
                    name: w.to_string(),
                    params: Vec::new(),
                });
            }
        }
    }
    let repo = std::env::current_dir()?;
    let client = Client::new("app")?;
    // operator calls run here; the GPUI executor awaits them
    let runtime = tokio::runtime::Runtime::new()?;
    let handle = runtime.handle().clone();
    gpui_kit::application()
        .with_assets(AppAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
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
                cx.new(|cx| LegionApp::new(client, handle, open, repo, cx).with_dossier(dossier))
            })
            .expect("opening the window");
        });
    drop(runtime);
    Ok(())
}
