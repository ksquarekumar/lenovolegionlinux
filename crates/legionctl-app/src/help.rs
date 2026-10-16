//! The title bar's Help menu (todo-169): About (the seed view `about` as a tab), Feedback and Keys; and Ctrl+K, the
//! command palette: the chat bar with `/`, whose list has the views, the commands and the operations.

use gpui_kit::assets::IconName;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::{AnyElement, Context, IntoElement as _, Window};

use crate::LegionApp;

/// The keys of the app, one per line.
const KEYS: &str = "Ctrl+K: search the views, commands and operations (the chat bar with /)
Up, Down in the chat bar: the lines sent before
Tab: complete the chat line; with nothing to complete, go to the tab (Up, Down scroll it) and back
PageUp, PageDown, Home, End: scroll the tab
Ctrl+Tab, Ctrl+Shift+Tab: the next or the last tab; Ctrl+W: close the tab; Ctrl+Shift+Left, Right: move it
Ctrl+1 to Ctrl+4: the spaces
Enter: send; Shift+Enter or Ctrl+J: a new line; Ctrl+Left, Right: the chat bar's width
Ctrl+C: copy the selected text";

impl LegionApp {
    /// The Help menu of the title bar.
    pub(crate) fn help_menu(cx: &mut Context<Self>) -> AnyElement {
        let app = cx.entity().downgrade();
        Button::new("help")
            .ghost()
            .xsmall()
            .icon(IconName::Info)
            .label("Help")
            .dropdown_menu(move |menu, _, _| {
                let item = |label: &'static str, run: fn(&mut LegionApp, &mut Window, &mut Context<LegionApp>)| {
                    let app = app.clone();
                    PopupMenuItem::new(label).on_click(move |_, window, cx| {
                        // the menu had the focus: the keys go to the chat bar again
                        let _ = app.update(cx, |app, cx| {
                            app.chat_focus.focus(window, cx);
                            run(app, window, cx);
                        });
                    })
                };
                menu.item(item("About legionctl", |app, _, cx| app.open_view("about", cx)))
                    .item(item("Feedback", LegionApp::ask_feedback))
                    .item(item("Keys", |app, _, cx| {
                        app.state.message = Some(KEYS.into());
                        cx.notify();
                    }))
            })
            .into_any_element()
    }

    /// Ctrl+K: the chat bar with `/`, so its list shows everything there is to open or run.
    pub(crate) fn palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        *self.screen.chat_mut() = "/".into();
        self.chat_focus.focus(window, cx);
    }
}
