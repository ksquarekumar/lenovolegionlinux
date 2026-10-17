//! The title bar's Help menu (todo-169): About (the seed view `about` as a tab), Feedback, Keys and the FAQ
//! (todo-181, todo-184); and Ctrl+K, the
//! command palette: the chat bar with `/`, whose list has the views, the commands and the operations.

use gpui_kit::assets::IconName;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::{AnyElement, Context, IntoElement as _, Window};

use crate::LegionApp;

/// The keys of the app, one per line.
const KEYS: &str = "Ctrl+K: search the views, commands and operations (the chat bar with /)
Tab, Shift+Tab: the next or the last control; Enter or Space presses it
Up, Down in the chat bar: the lines sent before; with the list of commands open, move in the list
Tab in the chat bar: complete the command; Enter: send, or take the command picked in the list; Esc: close the list
Shift+Enter or Ctrl+J: a new line
Ctrl+V, Ctrl+Shift+V, Shift+Insert or the middle button: paste; Ctrl+C: copy; Ctrl+X: cut; Ctrl+A: select all
Esc: close a form, or discard a change that waits for Apply
PageUp, PageDown: scroll the tab; Home, End too, outside a text field
Ctrl+Tab, Ctrl+Shift+Tab: the next or the last tab; Ctrl+W: close the tab; Ctrl+Shift+Left, Right: move it
Ctrl+1 to Ctrl+4: the spaces
Ctrl+Left, Right outside a text field: the chat bar's width";

/// What a Help topic is about: a view or an operation that the app has. An app test fails for a topic whose feature
/// is gone, so the FAQ never answers for a removed feature.
#[derive(Debug, Clone, Copy)]
pub enum Feature {
    View(&'static str),
    Op(&'static str),
}

/// A question of the FAQ, its short answer and the features it names.
#[derive(Debug)]
pub struct Topic {
    pub question: &'static str,
    pub answer: &'static str,
    pub features: &'static [Feature],
}

/// The FAQ of the Help menu: short answers in the user's words; the agents handle the tricky cases.
pub const FAQ: &[Topic] = &[
    Topic {
        question: "How do I chat with an agent?",
        answer: "Click New chat in Work and type a message.",
        features: &[Feature::Op("thread.start")],
    },
    Topic {
        question: "What is a dossier?",
        answer: "A folder that keeps the record of your chats and agent work. Choose one in Dossiers.",
        features: &[Feature::View("dossiers"), Feature::Op("dossier.use")],
    },
    Topic {
        question: "What can agents change?",
        answer: "Only the views in the tabs. In a view tab, a message asks the agent to change the view. \
                 Agents cannot change the window, the menus, the chat bar or your approvals.",
        features: &[Feature::Op("views.save")],
    },
    Topic {
        question: "How do I undo a change to a view?",
        answer: "Use the arrows next to the view's title. Pin keeps a version; Reset goes back to it.",
        features: &[Feature::Op("views.version")],
    },
    Topic {
        question: "Why does a change ask for my password?",
        answer: "It changes the machine, so the system asks first.",
        features: &[Feature::Op("power.apply")],
    },
    Topic {
        question: "Where do I see problems?",
        answer: "Open Health in Machine.",
        features: &[Feature::View("health")],
    },
    Topic {
        question: "Which GPU draws the desktop?",
        answer: "Open Apps and GPU. A change applies at your next login.",
        features: &[Feature::View("apps-gpu"), Feature::Op("gpu.primary")],
    },
    Topic {
        question: "How do I tell what is wrong in the app?",
        answer: "Click Feedback, or type /feedback and your note.",
        features: &[Feature::Op("feedback.new")],
    },
    Topic {
        question: "Which version do I have?",
        answer: "Help > About legionctl.",
        features: &[Feature::View("about")],
    },
];

/// The FAQ as the text of the message card.
fn faq() -> String {
    FAQ.iter()
        .map(|t| format!("{}\n{}", t.question, t.answer))
        .collect::<Vec<_>>()
        .join("\n\n")
}

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
                    .item(item("Questions", |app, _, cx| {
                        app.state.message = Some(faq());
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
