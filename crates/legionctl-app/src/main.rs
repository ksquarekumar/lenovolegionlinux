use gpui_kit::component::{Theme, TitleBar};
use gpui_kit::{AppContext, Bounds, WindowBounds, WindowOptions, px, size};
use legionctl_api::client::Client;
use legionctl_app::{LegionApp, Page};

fn main() -> anyhow::Result<()> {
    let usage =
        "usage: legionctl-app [now|gpu|health|bench|resources] [--agent <name> [--dossier <name>]]";
    let mut page = Page::Now;
    let (mut agent, mut dossier) = (None, None);
    let mut args = std::env::args().skip(1);
    while let Some(word) = args.next() {
        match word.as_str() {
            "--agent" => agent = Some(args.next().ok_or_else(|| anyhow::anyhow!(usage))?),
            "--dossier" => dossier = Some(args.next().ok_or_else(|| anyhow::anyhow!(usage))?),
            _ => page = Page::named(&word).ok_or_else(|| anyhow::anyhow!(usage))?,
        }
    }
    let repo = std::env::current_dir()?;
    let client = Client::new("app")?;
    // operator calls run here; the GPUI executor awaits them
    let runtime = tokio::runtime::Runtime::new()?;
    let handle = runtime.handle().clone();
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(1280.), px(800.)),
                    cx,
                ))),
                app_id: Some("legionctl".into()),
                ..TitleBar::window_options()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                Theme::sync_system_appearance(Some(window), cx);
                // the desktop portal reports the dark/light choice after the window opens, and on each change
                window
                    .observe_window_appearance(|window, cx| {
                        Theme::sync_system_appearance(Some(window), cx);
                    })
                    .detach();
                cx.new(|cx| {
                    let app = LegionApp::new(client, handle, page, cx);
                    match agent {
                        Some(agent) => app.with_agent(agent, dossier, repo),
                        None => app,
                    }
                })
            })
            .expect("opening the window");
        });
    drop(runtime);
    Ok(())
}
