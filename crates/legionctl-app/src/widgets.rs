//! The view renderer (DESIGN-09 Rule 0): the one place that draws a view's data, for the seeds and for the views that
//! agents make. The widgets are the closed set of `legionctl_core::views::Widget`; nothing else draws a screen. Each
//! button of a view runs its operation through [`LegionApp::act`], with the operation's own approval.

use gpui_kit::assets::IconName;
use gpui_kit::base::SelectableText;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::chart::LineChart;
use gpui_kit::component::text::TextView;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable as _, Sizable as _, StyledExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, ClipboardItem, Context, Div, ElementId, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled, div,
    px, relative,
};
use legionctl_api::{OPS, ViewButton, ViewData, ViewWidget};

use crate::LegionApp;
use crate::theme::{
    self, R_SM, S1, S2, S3, S4, S5, T_LG, T_SM, T_XL, T_XS, Tokens, badge, card, heading, icon,
};

/// The narrowest stat card; the cards share the width evenly (U9).
const STAT_WIDTH: f32 = 200.;

/// What a button does, in words: its operation's summary.
fn about(b: &ViewButton) -> String {
    OPS.iter()
        .find(|o| o.name == b.op)
        .map_or_else(|| b.label.clone(), |o| o.summary().to_string())
}

/// Text that the user can select with the mouse and copy with Ctrl+C (todo-168); `id` is unique among its siblings.
// shortcut: gpui-base 0.7.3 selects nothing in a text cut with an ellipsis (a long table cell); the row's copy button
// and the cell's tooltip have it whole; upgrade when the kit selects elided text
pub(crate) fn text(id: impl Into<SharedString>, text: impl Into<SharedString>) -> SelectableText {
    SelectableText::new(ElementId::Name(id.into()), text)
}

/// A button that puts `what` on the clipboard.
pub(crate) fn copy_button(id: impl Into<SharedString>, tip: &'static str, what: String) -> Button {
    Button::new(ElementId::Name(id.into()))
        .ghost()
        .xsmall()
        .icon(IconName::Copy)
        .tooltip(tip)
        .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(what.clone())))
}

/// The colour of a state word.
fn mark(state: &str, t: &Tokens) -> Hsla {
    match state {
        "ok" => t.ok,
        "warn" | "missing" => t.warn,
        "critical" => t.critical,
        _ => t.text,
    }
}

impl LegionApp {
    /// The view of a tab: its head (title, version and history buttons, its buttons), then its widget.
    pub(crate) fn view_tab(
        &self,
        data: &ViewData,
        width: f32,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut head = div()
            .h_flex()
            .items_start()
            .justify_between()
            .gap(S3)
            .child(
                div().v_flex().gap(S1).min_w_0().child(
                    div()
                        .text_size(T_LG)
                        .font_semibold()
                        .text_color(t.text)
                        .child(data.title.clone()),
                ),
            );
        if !data.versions.is_empty() {
            head = head.child(Self::history_bar(data, t, cx));
        }
        div()
            .id(SharedString::from(format!("view-{}", data.name)))
            .v_flex()
            .gap(S4)
            .child(head)
            .when(!data.actions.is_empty(), |d| {
                d.child(Self::buttons(&data.name, "action", &data.actions, cx))
            })
            .child(self.widget(data, width, t, cx, true))
            .test_support()
            .into_any_element()
    }

    /// Undo, redo, reset and pin of the view's versions.
    fn history_bar(data: &ViewData, t: &Tokens, cx: &mut Context<Self>) -> Div {
        let name = data.name.clone();
        let first = data.versions.first().copied().unwrap_or(0);
        let last = data.versions.last().copied().unwrap_or(0);
        let step = |id: &'static str,
                    glyph: IconName,
                    tip: &'static str,
                    to: &'static str,
                    enabled: bool| {
            let name = name.clone();
            Button::new(id)
                .small()
                .ghost()
                .icon(glyph)
                .disabled(!enabled)
                .tooltip(tip)
                .on_click(cx.listener(move |app, _, _, cx| app.version(&name, to, cx)))
        };
        div()
            .h_flex()
            .flex_none()
            .gap(S1)
            .text_size(T_XS)
            .text_color(t.muted)
            .child(if data.version == 0 {
                "Original".to_string()
            } else {
                format!("Version {} of {last}", data.version)
            })
            .child(step(
                "view-undo",
                IconName::Undo2,
                "Go back one version",
                "undo",
                data.version > first,
            ))
            .child(step(
                "view-redo",
                IconName::Redo2,
                "Go forward one version",
                "redo",
                data.version < last,
            ))
            .child(step(
                "view-reset",
                IconName::RotateCw,
                "Go back to the pinned version, or to the original",
                "reset",
                true,
            ))
            .child(step(
                "view-pin",
                IconName::Pin,
                "Pin this version: Reset comes back to it",
                "pin",
                true,
            ))
    }

    /// A row of buttons; one action style for every button of a view (U13).
    pub(crate) fn buttons(
        view: &str,
        part: &str,
        buttons: &[ViewButton],
        cx: &mut Context<Self>,
    ) -> Div {
        div()
            .h_flex()
            .flex_wrap()
            .gap(S2)
            .children(buttons.iter().enumerate().map(|(i, b)| {
                let button = b.clone();
                Button::new(SharedString::from(format!("view-{view}-{part}-{i}")))
                    .small()
                    .outline()
                    .label(b.label.clone())
                    .tooltip(about(b))
                    .on_click(cx.listener(move |app, _, _, cx| app.act(button.clone(), cx)))
            }))
    }

    /// A view that cannot be read: why, with a copy button.
    fn error_card(data: &ViewData, error: &str, t: &Tokens) -> AnyElement {
        card(t)
            .id(SharedString::from(format!("view-{}-error", data.name)))
            .child(heading(data.title.clone(), t))
            .child(
                div()
                    .h_flex()
                    .items_start()
                    .gap(S2)
                    .text_size(T_SM)
                    .text_color(t.critical)
                    .child(icon(IconName::CircleX, px(16.), t.critical))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(text("error", format!("Cannot read the view: {error}"))),
                    )
                    .child(copy_button(
                        format!("view-{}-error-copy", data.name),
                        "Copy the error",
                        error.to_string(),
                    )),
            )
            .test_support()
            .into_any_element()
    }

    /// A view of the code whose folder is in no git repository: why, and the choice of one.
    fn needs_repository(data: &ViewData, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        card(t)
            .id(SharedString::from(format!("view-{}-repository", data.name)))
            .child(heading(data.title.clone(), t))
            .child(
                div()
                    .text_size(T_SM)
                    .text_color(t.text2)
                    .child(text("needs-repository", data.note.clone())),
            )
            .child(
                div().h_flex().child(
                    Button::new(SharedString::from(format!(
                        "view-{}-choose-repo",
                        data.name
                    )))
                    .small()
                    .primary()
                    .label("Choose a repository")
                    .on_click(cx.listener(|app, _, _, cx| {
                        app.choose_place(crate::Then::TabRepo, None, cx);
                    })),
                ),
            )
            .test_support()
            .into_any_element()
    }

    /// A view's widget; a part of a stack has its own title.
    pub(crate) fn widget(
        &self,
        data: &ViewData,
        width: f32,
        t: &Tokens,
        cx: &mut Context<Self>,
        top: bool,
    ) -> AnyElement {
        if data.needs_repository {
            return Self::needs_repository(data, t, cx);
        }
        if let Some(error) = &data.error {
            return Self::error_card(data, error, t);
        }
        let id = |part: &str| SharedString::from(format!("view-{}-{part}", data.name));
        match data.widget {
            ViewWidget::Stack => self.stack(data, width, t, cx),
            ViewWidget::Stat => stat(data, t),
            ViewWidget::Table | ViewWidget::List => self.table(data, t, cx, top),
            ViewWidget::Chart => chart(data, t),
            ViewWidget::Graph => self.graph(data, top, t, cx),
            ViewWidget::Form => Self::form_widget(data, t, cx),
            ViewWidget::Timeline => Self::timeline(data, t),
            ViewWidget::Tree => framed(
                data,
                top,
                t,
                div()
                    .id(id("tree"))
                    .v_flex()
                    .text_size(T_SM)
                    .font_family("monospace")
                    .children(data.rows.iter().enumerate().map(|(i, r)| {
                        div()
                            .id(id(&format!("row-{i}")))
                            .whitespace_nowrap()
                            .child(text("row", r.join(" ")))
                            .test_support()
                    }))
                    .test_support()
                    .into_any_element(),
            ),
            ViewWidget::Gauge => framed(data, top, t, gauge(data, t)),
            ViewWidget::Sparkline => framed(
                data,
                top,
                t,
                div()
                    .id(id("sparkline"))
                    .text_size(T_LG)
                    .child(legionctl_api::sparkline(&data.numbers))
                    .test_support()
                    .into_any_element(),
            ),
            ViewWidget::Markdown => framed(
                data,
                top,
                t,
                TextView::markdown(id("markdown"), data.text.clone())
                    .selectable(true)
                    .into_any_element(),
            ),
            ViewWidget::Diff => framed(
                data,
                top,
                t,
                div()
                    .id(id("diff"))
                    .v_flex()
                    .text_size(T_XS)
                    .font_family("monospace")
                    .children(data.text.lines().enumerate().map(|(i, l)| {
                        let line = div().child(text(format!("line-{i}"), l.to_string()));
                        if l.starts_with('+') {
                            line.text_color(t.ok)
                        } else if l.starts_with('-') {
                            line.text_color(t.critical)
                        } else {
                            line.text_color(t.text2)
                        }
                    }))
                    .test_support()
                    .into_any_element(),
            ),
        }
    }

    /// A stack: its parts top to bottom; stats side by side in an even grid (U9).
    fn stack(&self, data: &ViewData, width: f32, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let mut out = div()
            .id(SharedString::from(format!("view-{}-stack", data.name)))
            .v_flex()
            .gap(S4);
        let mut stats: Vec<AnyElement> = Vec::new();
        let columns = |n: usize| {
            let fit = (width / STAT_WIDTH).floor().max(1.);
            // the fewest rows, then columns that share them evenly: 5 cards in 3 columns are 3 + 2, not 4 + 1
            let fit = u16::try_from(n).unwrap_or(1).min(fit_u16(fit));
            let rows = u16::try_from(n).unwrap_or(1).div_ceil(fit.max(1));
            u16::try_from(n).unwrap_or(1).div_ceil(rows.max(1))
        };
        for part in &data.parts {
            if part.widget == ViewWidget::Stat && part.error.is_none() {
                stats.push(stat(part, t));
                continue;
            }
            if !stats.is_empty() {
                let n = stats.len();
                out = out.child(
                    div()
                        .grid()
                        .grid_cols(columns(n))
                        .gap(S3)
                        .children(std::mem::take(&mut stats)),
                );
            }
            out = out.child(self.widget(part, width, t, cx, false));
        }
        if !stats.is_empty() {
            let n = stats.len();
            out = out.child(div().grid().grid_cols(columns(n)).gap(S3).children(stats));
        }
        out.test_support().into_any_element()
    }

    /// A table or a list: groups with their counts (folded when long), state colours, and each row's buttons.
    fn table(&self, data: &ViewData, t: &Tokens, cx: &mut Context<Self>, top: bool) -> AnyElement {
        let name = data.name.clone();
        let table = data.widget == ViewWidget::Table;
        let with_buttons = data.row_actions.iter().any(|b| !b.is_empty());
        let header = table.then(|| {
            row(
                div().text_color(t.muted).font_weight(FontWeight::MEDIUM),
                &format!("view-{name}-head"),
                &data.columns,
                &[],
                false,
                t,
            )
            .when(with_buttons, |r| r.child(div().w(px(BUTTONS)).flex_none()))
            // the width of the rows' copy button
            .child(div().w(px(COPY)).flex_none())
        });
        let line = |i: usize| -> AnyElement {
            let cells = &data.rows[i];
            let marks = data.marks.get(i).map_or(&[][..], Vec::as_slice);
            let id = format!("view-{name}-row-{i}");
            let mut r = row(div().text_color(t.text2), &id, cells, marks, !table, t)
                .id(SharedString::from(id.clone()));
            if with_buttons {
                let buttons = data.row_actions.get(i).cloned().unwrap_or_default();
                r = r.child(
                    div()
                        .flex_none()
                        .w(px(BUTTONS))
                        .h_flex()
                        .flex_wrap()
                        .justify_end()
                        .gap(S1)
                        .children(buttons.iter().enumerate().map(|(j, b)| {
                            let button = b.clone();
                            Button::new(SharedString::from(format!(
                                "view-{name}-row-{i}-action-{j}"
                            )))
                            .small()
                            .outline()
                            .label(b.label.clone())
                            .tooltip(about(b))
                            .on_click(cx.listener(move |app, _, _, cx| app.act(button.clone(), cx)))
                        })),
                );
            }
            r.child(copy_button(
                format!("{id}-copy"),
                "Copy the row",
                cells.join("\t"),
            ))
            .test_support()
            .into_any_element()
        };
        let mut body = div()
            .id(SharedString::from(format!(
                "view-{name}-{}",
                if table { "table" } else { "list" }
            )))
            .v_flex()
            .children(header);
        if data.groups.is_empty() {
            body = body.children((0..data.rows.len()).map(line));
        } else {
            for group in &data.group_order {
                let rows: Vec<usize> = (0..data.rows.len())
                    .filter(|i| data.groups[*i] == *group)
                    .collect();
                let key = format!("{name}:{group}");
                let folded =
                    data.fold.is_some_and(|f| rows.len() > f) != self.screen.toggled.contains(&key);
                body = body.child(group_head(&name, group, &rows, data, folded, t, cx));
                if !folded {
                    body = body.children(rows.into_iter().map(line));
                }
            }
        }
        if data.rows.is_empty() {
            // a part that waits for a parameter says for what
            body = body.child(div().py(S2).text_size(T_SM).text_color(t.muted).child(
                if data.note.is_empty() {
                    "Nothing to show".to_string()
                } else {
                    data.note.clone()
                },
            ));
        }
        framed(data, top, t, body.test_support().into_any_element())
    }
}

/// A group's head: its fold chevron, its name, its count and its worst state; a click folds or opens it.
fn group_head(
    name: &str,
    group: &str,
    rows: &[usize],
    data: &ViewData,
    folded: bool,
    t: &Tokens,
    cx: &Context<LegionApp>,
) -> AnyElement {
    let worst = rows
        .iter()
        .flat_map(|i| data.marks.get(*i).into_iter().flatten())
        .max_by_key(|m| match m.as_str() {
            "critical" => 3,
            "warn" => 2,
            "ok" => 1,
            _ => 0,
        })
        .filter(|m| !m.is_empty());
    let toggle = format!("{name}:{group}");
    div()
        .id(SharedString::from(format!("view-{name}-group-{group}")))
        .h_flex()
        .gap(S2)
        .pt(S3)
        .pb(S1)
        .cursor_pointer()
        .child(icon(
            if folded {
                IconName::ChevronRight
            } else {
                IconName::ChevronDown
            },
            px(14.),
            t.muted,
        ))
        .child(
            div()
                .text_size(T_SM)
                .font_semibold()
                .text_color(t.text)
                .child(group.to_string()),
        )
        .child(badge(rows.len().to_string(), t.muted))
        .children(worst.map(|m| badge(m.clone(), mark(m, t))))
        .on_click(cx.listener(move |app, _, _, cx| {
            if !app.screen.toggled.remove(&toggle) {
                app.screen.toggled.insert(toggle.clone());
            }
            cx.notify();
        }))
        .test_support()
        .into_any_element()
}

fn fit_u16(columns: f32) -> u16 {
    (1..=12u16)
        .take_while(|c| f32::from(*c) <= columns)
        .last()
        .unwrap_or(1)
}

/// A widget in a card with the view's title (a part of a stack); a view on its own has its title in the tab's head.
fn framed(data: &ViewData, top: bool, t: &Tokens, body: AnyElement) -> AnyElement {
    card(t)
        .when(!top, |c| c.child(heading(data.title.clone(), t)))
        .child(body)
        .into_any_element()
}

/// The width of a table's buttons column: the same on each row, so that the cells line up (U7).
const BUTTONS: f32 = 240.;
/// The width of a row's copy button.
const COPY: f32 = 20.;

/// One row of cells: the first wider, the rest even; in a table a long value truncates (a path in its middle) and its
/// tooltip has it whole (U12); a list's row wraps.
fn row(base: Div, id: &str, cells: &[String], marks: &[String], wrap: bool, t: &Tokens) -> Div {
    base.h_flex()
        .items_center()
        .gap(S3)
        .py(px(5.))
        .border_b_1()
        .border_color(t.border)
        .text_size(T_SM)
        .font_features(theme::tabular())
        .children(cells.iter().enumerate().map(|(i, c)| {
            let color = marks.get(i).filter(|m| !m.is_empty()).map(|m| mark(m, t));
            let whole = c.clone();
            // a path keeps its file name in sight: the cut is in the middle
            let path = c.contains('/') && !c.contains(' ');
            div()
                .id(SharedString::from(format!("{id}-cell-{i}")))
                .when(i == 0, |d| d.flex_grow(2.).flex_basis(px(0.)))
                .when(i > 0, |d| d.flex_grow(1.).flex_basis(px(0.)))
                .min_w_0()
                .when(!wrap, |d| {
                    d.truncate()
                        .when(path, Styled::text_ellipsis_middle)
                        .tooltip(move |window, cx| Tooltip::new(whole.clone()).build(window, cx))
                })
                .when_some(color, Styled::text_color)
                .child(text("text", c.clone()))
                .test_support()
        }))
}

/// A stat: its title, its value with unit in its state colour, its note (or the cause of a missing value) and trend.
fn stat(data: &ViewData, t: &Tokens) -> AnyElement {
    let color = mark(&data.level, t);
    let missing = data.level == "missing";
    card(t)
        .id(SharedString::from(format!("view-{}-stat", data.name)))
        .gap(S1)
        .min_w_0()
        .child(
            div()
                .text_size(T_XS)
                .text_color(t.muted)
                .truncate()
                .child(text("title", data.title.clone())),
        )
        .child(
            div()
                .h_flex()
                .items_baseline()
                .gap(S1)
                .child(
                    div()
                        .text_size(if data.text.chars().count() > 9 {
                            T_LG
                        } else {
                            T_XL
                        })
                        .font_weight(FontWeight::SEMIBOLD)
                        .font_features(theme::tabular())
                        .text_color(if data.level.is_empty() { t.text } else { color })
                        .whitespace_nowrap()
                        .child(text("value", data.text.clone())),
                )
                .child(
                    div()
                        .text_size(T_SM)
                        .text_color(t.muted)
                        .child(text("unit", data.unit.clone())),
                ),
        )
        .child(
            div()
                .h_flex()
                .items_start()
                .gap(S1)
                .text_size(T_XS)
                .text_color(if missing { t.warn } else { t.muted })
                .when(missing, |d| {
                    d.child(icon(IconName::TriangleAlert, px(12.), t.warn))
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(text("note", data.note.clone())),
                ),
        )
        .when(data.numbers.len() > 1, |c| {
            let points: Vec<(String, f64)> = data
                .numbers
                .iter()
                .enumerate()
                .map(|(i, v)| (i.to_string(), *v))
                .collect();
            c.child(
                div().h(px(32.)).w_full().child(
                    LineChart::new(points)
                        .id(SharedString::from(format!("trend-{}", data.name)))
                        .x(|p: &(String, f64)| p.0.clone())
                        .y(|p: &(String, f64)| p.1)
                        .stroke(if data.level.is_empty() {
                            t.accent
                        } else {
                            color
                        })
                        .x_axis(false)
                        .y_axis(false)
                        .grid(false)
                        .interactive(false)
                        .appear(false),
                ),
            )
        })
        .test_support()
        .into_any_element()
}

/// A chart: one card per series (a chart never mixes kinds, U11), with both axes, x as times when it is one.
fn chart(data: &ViewData, t: &Tokens) -> AnyElement {
    let charts =
        data.series.iter().enumerate().map(|(i, s)| {
            let points: Vec<(String, f64)> = s
                .labels
                .iter()
                .zip(&s.points)
                .map(|(x, (_, y))| (x.clone(), *y))
                .collect();
            let (low, high) = s
                .points
                .iter()
                .fold((f64::MAX, f64::MIN), |(l, h), p| (l.min(p.1), h.max(p.1)));
            let pad = ((high - low) * 0.2).max(high.abs() * 0.02).max(1.);
            div()
                .v_flex()
                .gap(S2)
                .child(
                    div()
                        .h_flex()
                        .justify_between()
                        .text_size(T_SM)
                        .child(div().font_semibold().text_color(t.text).child(
                            if s.name.is_empty() {
                                "–".to_string()
                            } else {
                                s.name.clone()
                            },
                        ))
                        .child(
                            div()
                                .text_size(T_XS)
                                .text_color(t.muted)
                                .child(format!("{} runs, {low:.0} to {high:.0}", s.points.len())),
                        ),
                )
                .child(
                    div().h(px(150.)).w_full().child(
                        LineChart::new(points)
                            .id(SharedString::from(format!("chart-{}-{i}", data.name)))
                            .x(|p: &(String, f64)| p.0.clone())
                            .y(|p: &(String, f64)| p.1)
                            .y_domain(low - pad, high + pad)
                            .y_axis(true)
                            .y_tick_format(|v| format!("{v:.0}"))
                            .x_tick_count(4)
                            .stroke(t.accent)
                            .dot(),
                    ),
                )
        });
    card(t)
        .id(SharedString::from(format!("view-{}-chart", data.name)))
        .gap(S5)
        .child(heading(data.title.clone(), t))
        .children(charts)
        .when(data.series.is_empty(), |c| {
            c.child(
                div()
                    .text_size(T_SM)
                    .text_color(t.muted)
                    .child("No point yet"),
            )
        })
        .test_support()
        .into_any_element()
}

/// A gauge: a bar filled to the value's part of `max`, and the numbers.
fn gauge(data: &ViewData, t: &Tokens) -> AnyElement {
    let value = data.numbers.first().copied().unwrap_or_default();
    let ratio = value / data.max;
    let percent = (1..=100u16)
        .take_while(|i| f64::from(*i) <= ratio * 100.0)
        .count();
    let fraction = f32::from(u16::try_from(percent).unwrap_or(100)) / 100.;
    div()
        .id(SharedString::from(format!("view-{}-gauge", data.name)))
        .v_flex()
        .gap_1()
        .child(
            div().h(px(8.)).w_full().rounded(R_SM).bg(t.raised).child(
                div()
                    .h_full()
                    .w(relative(fraction))
                    .rounded(R_SM)
                    .bg(t.accent),
            ),
        )
        .child(
            div()
                .text_size(T_XS)
                .child(text("value", format!("{value} / {}", data.max))),
        )
        .test_support()
        .into_any_element()
}

/// The message under a tab: the result of the last action, or why it failed.
pub(crate) fn message(message: &str, t: &Tokens) -> Div {
    card(t)
        .gap(S1)
        .text_size(T_SM)
        .text_color(t.text2)
        .child(
            div()
                .h_flex()
                .gap(S2)
                .text_color(t.muted)
                .text_size(T_XS)
                .child(icon(IconName::Info, px(14.), t.muted))
                .child(div().flex_1().child("Last result"))
                .child(copy_button(
                    "message-copy",
                    "Copy the result",
                    message.to_string(),
                )),
        )
        .children(
            message
                .lines()
                .enumerate()
                .map(|(i, l)| div().child(text(format!("line-{i}"), l.to_string()))),
        )
}
