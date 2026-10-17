//! The graph, form and timeline widgets (DESIGN-09 G4). A graph is a tree of nodes with counts: a click opens a node
//! (its children are in the data, or the view is read again with its `expand` parameter set to the node), a click on
//! a leaf sets the view's `select` parameter (the tab reads the view again: a stack's other parts follow it), and
//! the marks of its highlight show beside each node. A form widget opens its operation's form; a timeline shows
//! events with the actor only where it changes.

use std::collections::BTreeMap;

use gpui_kit::assets::IconName;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::StyledExt as _;
use gpui_kit::component::button::Button;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use legionctl_api::{ViewData, ViewNode, ViewRenderIn, op};

use crate::shell::Tab;
use crate::theme::{S1, S2, S3, T_SM, T_XS, Tokens, badge, card, heading, icon};
use crate::{Form, LegionApp};

impl LegionApp {
    /// A graph: its nodes as a tree, what a mark means, and its edges.
    pub(crate) fn graph(
        &self,
        data: &ViewData,
        top: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut rows = Vec::new();
        self.nodes(data, &data.nodes, 0, t, cx, &mut rows);
        card(t)
            .id(SharedString::from(format!("view-{}-graph", data.name)))
            .gap(px(0.))
            .child(
                div()
                    .h_flex()
                    .justify_between()
                    .pb(S2)
                    // a view on its own has its title in the tab's head
                    .child(if top {
                        div()
                    } else {
                        heading(data.title.clone(), t)
                    })
                    .when(!data.legend.is_empty(), |d| {
                        d.child(
                            div()
                                .h_flex()
                                .gap(S1)
                                .text_size(T_XS)
                                .text_color(t.muted)
                                .child(div().size(px(8.)).rounded_full().bg(t.accent))
                                .child(data.legend.clone()),
                        )
                    }),
            )
            .children(rows)
            .when(data.nodes.is_empty(), |c| {
                c.child(
                    div()
                        .py(S2)
                        .text_size(T_SM)
                        .text_color(t.muted)
                        .child("Nothing to show"),
                )
            })
            .when(!data.edges.is_empty(), |c| {
                c.child(
                    div()
                        .pt(S3)
                        .text_size(T_XS)
                        .text_color(t.muted)
                        .child(format!("{} links", data.edges.len())),
                )
                .children(data.edges.iter().map(|e| {
                    div()
                        .text_size(T_SM)
                        .text_color(t.text2)
                        .child(format!("{} → {} ({})", e.from, e.to, e.kind))
                }))
            })
            .test_support()
            .into_any_element()
    }

    /// The rows of `nodes` at `depth`, each open node followed by its children.
    fn nodes(
        &self,
        data: &ViewData,
        nodes: &[ViewNode],
        depth: usize,
        t: &Tokens,
        cx: &mut Context<Self>,
        out: &mut Vec<AnyElement>,
    ) {
        for n in nodes {
            let key = format!("{}:{}", data.name, n.id);
            let open = self.screen.opened.contains(&key);
            let selected = data
                .select
                .as_ref()
                .and_then(|p| data.params.get(p))
                .is_some_and(|v| *v == n.id);
            let state = match n.state.as_str() {
                "ok" => Some(t.ok),
                "warn" => Some(t.warn),
                "critical" => Some(t.critical),
                _ => None,
            };
            let node = n.clone();
            let view = data.clone();
            out.push(
                crate::theme::control(
                    div().id(SharedString::from(format!("node-{key}"))),
                    n.label.clone(),
                    t,
                )
                .h_flex()
                .gap(S2)
                .pl(px(f32::from(u16::try_from(depth).unwrap_or(0)) * 18.))
                .py(px(3.))
                .rounded(crate::theme::R_SM)
                .cursor_pointer()
                .text_size(T_SM)
                .when(selected, |d| d.bg(t.accent.opacity(0.14)))
                .when(!selected, |d| d.hover(|s| s.bg(t.raised)))
                .child(if n.leaf {
                    div().w(px(14.)).flex_none()
                } else {
                    div().flex_none().child(icon(
                        if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        },
                        px(14.),
                        t.muted,
                    ))
                })
                .children(state.map(|c| div().size(px(8.)).flex_none().rounded_full().bg(c)))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(t.text)
                        .child(n.label.clone()),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(T_XS)
                        .text_color(t.muted)
                        .child(n.kind.clone()),
                )
                .children(n.count.map(|c| badge(c.to_string(), t.muted)))
                .children(n.marked.then(|| badge("changed", t.accent)))
                .on_click(cx.listener(move |app, _, _, cx| app.click_node(&view, &node, cx)))
                .test_support()
                .into_any_element(),
            );
            if open {
                let lazy = self.screen.lazy.get(&key);
                match (n.children.is_empty(), lazy) {
                    (false, _) => self.nodes(data, &n.children, depth + 1, t, cx, out),
                    (true, Some(Ok(children))) => self.nodes(data, children, depth + 1, t, cx, out),
                    (true, Some(Err(e))) => out.push(
                        div()
                            .pl(px(f32::from(u16::try_from(depth + 1).unwrap_or(0)) * 18.))
                            .text_size(T_SM)
                            .text_color(t.critical)
                            .child(format!("Cannot read it: {e}"))
                            .into_any_element(),
                    ),
                    (true, None) => out.push(
                        div()
                            .pl(px(f32::from(u16::try_from(depth + 1).unwrap_or(0)) * 18.))
                            .text_size(T_SM)
                            .text_color(t.muted)
                            .child("reading…")
                            .into_any_element(),
                    ),
                }
            }
        }
    }

    /// A click on a node: a leaf sets the view's `select` parameter on the tab; another node opens or closes, and
    /// its children are read once when the data has none.
    pub fn click_node(&mut self, data: &ViewData, node: &ViewNode, cx: &mut Context<Self>) {
        if node.leaf {
            if let Some(param) = &data.select {
                self.set_param(param, &node.id, cx);
            }
            return;
        }
        let key = format!("{}:{}", data.name, node.id);
        if !self.screen.opened.remove(&key) {
            self.screen.opened.insert(key.clone());
            if let (true, Some(param), false) = (
                node.children.is_empty(),
                &data.expand,
                self.screen.lazy.contains_key(&key),
            ) {
                let mut params: BTreeMap<String, String> = data.params.clone();
                params.insert(param.clone(), node.id.clone());
                let client = self.client.clone();
                let input = ViewRenderIn {
                    name: data.name.clone(),
                    params: params
                        .into_iter()
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect(),
                    repo: legionctl_api::ClientPath(self.tab_repo()),
                };
                let task =
                    self.operator(
                        cx,
                        async move { client.call::<op::ViewsRender>(&input).await },
                    );
                cx.spawn(async move |this, cx| {
                    let result = task.await;
                    let _ = this.update(cx, |app: &mut LegionApp, cx| {
                        let children = match result {
                            Ok(v) => v.error.map_or(Ok(v.nodes), Err),
                            Err(e) => Err(format!("{e:#}")),
                        };
                        app.screen.lazy.insert(key, children);
                        cx.notify();
                    });
                })
                .detach();
            }
        }
        cx.notify();
    }

    /// Set a parameter of the tab's view; the tab reads the view again with it.
    fn set_param(&mut self, param: &str, value: &str, cx: &mut Context<Self>) {
        let Some(Tab::View { params, .. }) = self.screen.tabs.get_mut(self.screen.active) else {
            return;
        };
        params.retain(|p| !p.starts_with(&format!("{param}=")));
        params.push(format!("{param}={value}"));
        self.open_tab(self.screen.tabs[self.screen.active].clone(), cx);
    }

    /// A form widget: its operation, its required fields, and the button that opens its form.
    pub(crate) fn form_widget(data: &ViewData, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let Some(button) = data.actions.first().cloned() else {
            return div().into_any_element();
        };
        let Some(info) = legionctl_api::OPS.iter().find(|o| o.name == button.op) else {
            return div()
                .text_color(t.critical)
                .child(format!("Unknown operation {}", button.op))
                .into_any_element();
        };
        let fields = legionctl_api::form::fields(info);
        let required: Vec<String> = fields
            .iter()
            .filter(|f| f.required)
            .map(|f| crate::forms::label(&f.name))
            .collect();
        card(t)
            .id(SharedString::from(format!("view-{}-form", data.name)))
            .child(heading(data.title.clone(), t))
            .child(
                div()
                    .text_size(T_SM)
                    .text_color(t.muted)
                    .child(info.summary()),
            )
            .child(
                div()
                    .text_size(T_SM)
                    .text_color(t.text2)
                    .child(if required.is_empty() {
                        "All fields are optional".to_string()
                    } else {
                        format!("Required: {}", required.join(", "))
                    }),
            )
            .child(
                div().h_flex().child(
                    Button::new(SharedString::from(format!("view-{}-fill", data.name)))
                        .small()
                        .outline()
                        .label(button.label.clone())
                        .on_click(cx.listener(move |app, _, _, cx| {
                            app.state.pending = None;
                            app.state.form = Some(Form::new(
                                button.clone(),
                                crate::Then::Run,
                                fields.clone(),
                                legionctl_api::form::texts(&button.input),
                                false,
                            ));
                            cx.notify();
                        })),
                ),
            )
            .test_support()
            .into_any_element()
    }

    /// A timeline: time, actor (only where it changes), kind and text of each event.
    pub(crate) fn timeline(data: &ViewData, t: &Tokens) -> AnyElement {
        let now = crate::thread::unix_now();
        let mut last = String::new();
        card(t)
            .id(SharedString::from(format!("view-{}-timeline", data.name)))
            .gap(px(0.))
            .child(div().pb(S2).child(heading(data.title.clone(), t)))
            .children(data.rows.iter().enumerate().map(|(i, r)| {
                let changed = r[1] != last;
                last.clone_from(&r[1]);
                let when = r[0]
                    .parse::<i64>()
                    .map_or_else(|_| r[0].clone(), |at| crate::thread::ago(now, at));
                div()
                    .id(SharedString::from(format!("view-{}-row-{i}", data.name)))
                    .v_flex()
                    .pt(if changed { S2 } else { px(2.) })
                    .when(changed, |d| {
                        d.child(
                            div()
                                .text_size(T_XS)
                                .font_semibold()
                                .text_color(t.text)
                                .child(r[1].clone()),
                        )
                    })
                    .child(
                        div()
                            .h_flex()
                            .gap(S2)
                            .text_size(T_SM)
                            .child(badge(r[2].clone(), t.accent))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_color(t.text2)
                                    .child(r[3].clone()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(T_XS)
                                    .text_color(t.muted)
                                    .child(when),
                            ),
                    )
                    .test_support()
            }))
            .test_support()
            .into_any_element()
    }
}
