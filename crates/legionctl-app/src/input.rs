//! The app's text fields (todo-195): the chat bar and the form fields are the kit's inputs, so paste (Ctrl+V,
//! Ctrl+Shift+V, Shift+Insert, the middle button's primary selection, the right-click menu), copy, cut, select-all,
//! undo and the cursor keys work in each. The chat bar adds its list of commands (Up, Down, Enter, Tab, Esc), its
//! history (Up and Down on its first and last line) and Tab to the next control. A form takes the keys in its first
//! field and a pending change in its card, where Enter discards; when either closes, the keys go back where they were.

use gpui_kit::base::input::{InputBaseState, InputModeKind};
use gpui_kit::component::input::{
    Enter, IndentInline, InputEvent, InputState, MoveDown, MovePageDown, MovePageUp, MoveUp,
    OutdentInline, TextareaState,
};
use gpui_kit::{App, AppContext as _, Context, Entity, Focusable as _, MouseDownEvent, Window};

use crate::LegionApp;

/// The key context of the kit's text fields.
pub(crate) const CONTEXT: &str = "Input";

/// The chat bar's key context: Ctrl+J there is a new line.
pub(crate) const CHAT: &str = "ChatBar";

/// What takes the keys over the tab: a form, a pending change, or neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Modal {
    None,
    Form,
    Pending,
}

/// The chat bar's field: Enter sends, Shift+Enter and Ctrl+J make a new line, it grows to 8 lines.
pub(crate) fn chat_field(
    window: &mut Window,
    cx: &mut Context<LegionApp>,
) -> Entity<TextareaState> {
    cx.new(|cx| {
        TextareaState::new(window, cx)
            .auto_grow(1, 8)
            .submit_on_enter(true)
    })
}

/// The middle button on a text field: the primary selection goes in at the cursor, as in the other apps of the
/// desktop.
// shortcut: the text goes in at the cursor, not under the pointer; move the cursor first if users ask
pub(crate) fn paste_primary<M: InputModeKind>(
    state: &Entity<InputBaseState<M>>,
) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
    let state = state.clone();
    move |_, window, cx| {
        let Some(text) = cx.read_from_primary().and_then(|c| c.text()) else {
            return;
        };
        state.update(cx, |s, cx| {
            s.focus(window, cx);
            s.insert(text, window, cx);
        });
    }
}

impl LegionApp {
    /// The chat bar's text changed by the user: the tab keeps it; the list of commands opens again with no pick.
    pub(crate) fn chat_event(
        &mut self,
        _: &Entity<TextareaState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, InputEvent::Change) {
            self.take_typed(cx);
        }
    }

    /// The field's text into the tab, before a key of the bar acts on it: its change event comes after the key when
    /// both are in one update.
    fn take_typed(&mut self, cx: &mut Context<Self>) {
        let text = self.chat.read(cx).value().to_string();
        if text != self.chat_synced.0 {
            self.chat_synced.0.clone_from(&text);
            *self.screen.chat_mut() = text;
            self.screen.list.pick = None;
            self.screen.list.closed = false;
            self.screen.history.typed();
            cx.notify();
        }
    }

    /// Before each frame: the chat bar shows the tab's text, a new form gets its text fields, and the keys go to a
    /// new form's first field or a new pending change's safe button, and back when it closes.
    pub(crate) fn settle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // the app changed the tab's text (a recall, a completion, another tab): the field shows it; a change the user
        // made and whose event has not come yet stays
        let (text, hint) = (self.screen.chat().to_string(), self.chat_hint());
        let (push, hint_changed) = (text != self.chat_synced.0, hint != self.chat_synced.1);
        self.chat.update(cx, |s, cx| {
            if push {
                s.set_value(text.clone(), window, cx);
                s.set_selected_range(text.len()..text.len(), cx);
            }
            if hint_changed {
                s.set_placeholder(hint.clone(), window, cx);
            }
        });
        self.chat_synced = (text, hint);
        let mut first = None;
        if let Some(form) = self.state.form.as_mut() {
            for f in &form.fields {
                if !matches!(
                    f.kind,
                    legionctl_api::form::Kind::Bool | legionctl_api::form::Kind::Choice(_)
                ) && !form.inputs.contains_key(&f.name)
                {
                    let value = form.values.get(&f.name).cloned().unwrap_or_default();
                    let placeholder = if f.default.is_empty() {
                        String::new()
                    } else {
                        format!("default {}", f.default)
                    };
                    let input = cx.new(|cx| {
                        InputState::new(window, cx)
                            .placeholder(placeholder)
                            .default_value(value)
                    });
                    form.inputs.insert(f.name.clone(), input);
                }
            }
            first = form
                .fields
                .iter()
                .find(|f| f.required || form.all)
                .and_then(|f| form.inputs.get(&f.name))
                .map(|i| i.focus_handle(cx));
        }
        let now = match (&self.state.form, &self.state.pending) {
            (Some(_), _) => Modal::Form,
            (None, Some(_)) => Modal::Pending,
            (None, None) => Modal::None,
        };
        if now == self.modal {
            return;
        }
        if self.modal == Modal::None {
            self.back = window.focused(cx);
        }
        match now {
            Modal::Form => {
                if let Some(first) = first {
                    first.focus(window, cx);
                }
            }
            // the card takes the keys: Enter and Esc discard, Tab goes to Discard, then Apply
            Modal::Pending => self.pending_focus.focus(window, cx),
            Modal::None => self
                .back
                .take()
                .unwrap_or_else(|| self.chat_focus.clone())
                .focus(window, cx),
        }
        self.modal = now;
    }

    /// The chat bar's hint when it is empty.
    fn chat_hint(&self) -> String {
        let agent = self.screen.agent().unwrap_or_else(|| "the agent".into());
        match self.tab() {
            Some(crate::Tab::Chat | crate::Tab::Thread { .. }) => {
                format!("Message {agent}, or / for commands")
            }
            _ => format!("Ask {agent} about this view, or /"),
        }
    }

    /// Up or Down in the chat bar: they move in the list of commands when it shows (and the text is not a recalled
    /// line), else on the first (Up) or last (Down) line they recall the lines sent before; else the cursor moves.
    fn chat_vertical(&mut self, up: bool, cx: &mut Context<Self>) {
        self.take_typed(cx);
        // a recalled line keeps the arrows in the history, also when it starts a command
        let n = self.screen.completions().len().min(crate::thread::SHOWN);
        if n > 0 && !self.screen.history.recalling() {
            let pick = match (self.screen.list.pick, up) {
                (None, true) => n - 1,
                (None, false) => 0,
                (Some(i), true) => (i + n - 1) % n,
                (Some(i), false) => (i + 1) % n,
            };
            self.screen.list.pick = Some(pick);
            self.list_scroll.scroll_to_item(pick);
        } else {
            let state = self.chat.read(cx);
            let line = state.cursor_position().line as usize;
            let last = state.value().split('\n').count() - 1;
            if (up && line > 0) || (!up && line < last) {
                cx.propagate();
                return;
            }
            self.recall(up);
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// Tab in the chat bar: complete the picked command (else the first that the text starts), else the next control.
    fn chat_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.take_typed(cx);
        let entries = self.screen.completions();
        let chat = self.screen.chat().to_string();
        let entry = match self.screen.list.pick {
            Some(i) => entries.get(i),
            None => entries.first().filter(|e| e.text.starts_with(&chat)),
        };
        match entry {
            Some(e) => {
                *self.screen.chat_mut() = format!("{} ", e.text.trim_end());
                self.screen.list.pick = None;
            }
            None => window.focus_next(cx),
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// Enter in the chat bar: the picked command goes into the bar; else the line goes.
    fn chat_enter(&mut self, enter: &Enter, cx: &mut Context<Self>) {
        if enter.shift {
            cx.propagate();
            return;
        }
        self.take_typed(cx);
        match self.screen.list.pick.take() {
            Some(i) => {
                if let Some(e) = self.screen.completions().get(i) {
                    *self.screen.chat_mut() = format!("{} ", e.text.trim_end());
                }
            }
            None => self.submit(cx),
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// The chat bar's field with its keys and the middle button.
    pub(crate) fn chat_input(&self, cx: &mut Context<Self>) -> impl gpui_kit::IntoElement + use<> {
        use gpui_kit::base::TestSupportExt as _;
        use gpui_kit::component::Sizable as _;
        use gpui_kit::{InteractiveElement as _, ParentElement as _, Styled as _};
        gpui_kit::div()
            .id("chat-text")
            .key_context(CHAT)
            .flex_1()
            .min_w_0()
            .capture_action(cx.listener(|app, _: &MoveUp, _, cx| app.chat_vertical(true, cx)))
            .capture_action(cx.listener(|app, _: &MoveDown, _, cx| app.chat_vertical(false, cx)))
            .capture_action(
                cx.listener(|app, _: &IndentInline, window, cx| app.chat_tab(window, cx)),
            )
            .capture_action(cx.listener(|_, _: &OutdentInline, window, cx| {
                window.focus_prev(cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|app, enter: &Enter, _, cx| app.chat_enter(enter, cx)))
            .capture_action(cx.listener(|app, _: &MovePageUp, _, cx| {
                app.page(true);
                cx.stop_propagation();
                cx.notify();
            }))
            .capture_action(cx.listener(|app, _: &MovePageDown, _, cx| {
                app.page(false);
                cx.stop_propagation();
                cx.notify();
            }))
            .on_mouse_down(gpui_kit::MouseButton::Middle, paste_primary(&self.chat))
            .child(
                gpui_kit::component::input::Textarea::new(&self.chat)
                    .appearance(false)
                    .small()
                    .aria_label("Message"),
            )
            .test_support()
    }
}
