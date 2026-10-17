//! The app's design tokens in one place: surface levels, text levels, one accent, the state colours, the border, the
//! radius and spacing scales and the type scale, with the few building blocks that use them (card, badge, icon). Dark
//! and light follow GNOME's choice through the desktop portal; [`adopt`] gives the kit's own components the same
//! colours.

use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, StyledExt as _, Theme};
use gpui_kit::{
    App, Div, FontFeatures, FontWeight, Hsla, ParentElement as _, Pixels, Styled as _, div, px,
    rgb, rgba,
};

/// The colours of one appearance.
#[derive(Debug, Clone, Copy)]
pub struct Tokens {
    /// Surfaces from back to front: the window, a side pane, a card, a raised element (the chat bar, a menu).
    pub window: Hsla,
    pub pane: Hsla,
    pub card: Hsla,
    pub raised: Hsla,
    /// Text: primary, secondary, muted.
    pub text: Hsla,
    pub text2: Hsla,
    pub muted: Hsla,
    pub accent: Hsla,
    pub ok: Hsla,
    pub warn: Hsla,
    pub critical: Hsla,
    pub border: Hsla,
}

impl Tokens {
    /// The tokens of the appearance now.
    #[must_use]
    pub fn of(cx: &App) -> Tokens {
        if cx.theme().is_dark() {
            Tokens {
                window: rgb(0x17_17_1b).into(),
                pane: rgb(0x1d_1d_22).into(),
                card: rgb(0x24_24_29).into(),
                raised: rgb(0x2d_2d_33).into(),
                text: rgb(0xf2_f2_f5).into(),
                text2: rgb(0xc3_c3_cc).into(),
                muted: rgb(0x87_87_94).into(),
                accent: rgb(0x62_a0_ea).into(),
                ok: rgb(0x57_e3_89).into(),
                warn: rgb(0xf8_e4_5c).into(),
                critical: rgb(0xff_7b_63).into(),
                border: rgba(0xff_ff_ff_17).into(),
            }
        } else {
            Tokens {
                window: rgb(0xf6_f6_f8).into(),
                pane: rgb(0xee_ee_f2).into(),
                card: rgb(0xff_ff_ff).into(),
                raised: rgb(0xff_ff_ff).into(),
                text: rgb(0x1b_1b_20).into(),
                text2: rgb(0x45_45_4e).into(),
                muted: rgb(0x74_74_80).into(),
                accent: rgb(0x1c_71_d8).into(),
                ok: rgb(0x26_a2_69).into(),
                warn: rgb(0xb5_83_00).into(),
                critical: rgb(0xc0_1c_28).into(),
                border: rgba(0x00_00_00_1a).into(),
            }
        }
    }

    /// The state colour of `value` against its warning and critical limits.
    #[must_use]
    pub fn level(&self, value: f64, warn: f64, critical: f64) -> Hsla {
        if value >= critical {
            self.critical
        } else if value >= warn {
            self.warn
        } else {
            self.ok
        }
    }
}

/// Spacing scale.
pub const S1: Pixels = px(4.);
pub const S2: Pixels = px(8.);
pub const S3: Pixels = px(12.);
pub const S4: Pixels = px(16.);
pub const S5: Pixels = px(24.);
/// Radius scale.
pub const R_SM: Pixels = px(6.);
pub const R_MD: Pixels = px(10.);
pub const R_LG: Pixels = px(14.);
/// Type scale: captions, body, titles, page titles, hero values.
pub const T_XS: Pixels = px(12.);
pub const T_SM: Pixels = px(13.);
pub const T_MD: Pixels = px(15.);
pub const T_LG: Pixels = px(20.);
pub const T_XL: Pixels = px(28.);

/// Figures of one width, so that values in columns and live values do not move.
#[must_use]
pub fn tabular() -> FontFeatures {
    FontFeatures(Arc::new(vec![("tnum".into(), 1)]))
}

/// The kit's components (buttons, tags, tab bars, scroll bars) in the app's colours; again after each change of the
/// appearance, because a change loads the kit's own theme.
pub fn adopt(cx: &mut App) {
    let t = Tokens::of(cx);
    Theme::update(cx, |theme| {
        theme.font_size = px(14.);
        theme.radius = R_SM;
        theme.radius_lg = R_MD;
        theme.background = t.window;
        theme.foreground = t.text;
        theme.muted_foreground = t.muted;
        theme.border = t.border;
        theme.popover = t.raised;
        theme.title_bar = t.pane;
        theme.title_bar_border = t.border;
        theme.primary = t.accent;
        theme.primary_hover = t.accent.opacity(0.9);
        theme.primary_active = t.accent.opacity(0.8);
        theme.primary_foreground = rgb(0xff_ff_ff).into();
        theme.button_primary = theme.primary;
        theme.button_primary_hover = theme.primary_hover;
        theme.button_primary_active = theme.primary_active;
        theme.button_primary_foreground = theme.primary_foreground;
        theme.ring = t.accent;
        theme.secondary = t.raised;
        theme.secondary_hover = t.raised.blend(t.text.opacity(0.06));
        theme.secondary_active = t.raised.blend(t.text.opacity(0.1));
        theme.tab_bar_segmented = t.card;
        theme.tab_active = t.raised;
        theme.success = t.ok;
        theme.warning = t.warn;
        theme.danger = t.critical;
    });
}

/// A card: the card surface with its border and radius.
#[must_use]
pub fn card(t: &Tokens) -> Div {
    div()
        .v_flex()
        .gap(S3)
        .p(S4)
        .rounded(R_MD)
        .border_1()
        .border_color(t.border)
        .bg(t.card)
}

/// A small badge: `text` in `color` on a tint of it.
#[must_use]
pub fn badge(text: impl Into<String>, color: Hsla) -> Div {
    div()
        .flex_none()
        .px(S2)
        .py(px(1.))
        .rounded_full()
        .bg(color.opacity(0.16))
        .text_color(color)
        .text_size(T_XS)
        .font_weight(FontWeight::MEDIUM)
        .child(text.into())
}

/// An icon of the kit's set at `size` in `color`.
#[must_use]
pub fn icon(name: IconName, size: Pixels, color: Hsla) -> Icon {
    Icon::new(name).size(size).text_color(color)
}

/// A section title in a page.
#[must_use]
pub fn heading(text: impl Into<String>, t: &Tokens) -> Div {
    div()
        .text_size(T_MD)
        .font_semibold()
        .text_color(t.text)
        .child(text.into())
}

/// A clickable element of the app that the keyboard reaches: Tab stops at it, Enter and Space click it, a screen
/// reader reads it as a button with `label`, and a ring shows when the keys are on it.
pub(crate) fn control(
    el: gpui_kit::Stateful<Div>,
    label: impl Into<gpui_kit::SharedString>,
    t: &Tokens,
) -> gpui_kit::Stateful<Div> {
    use gpui_kit::{InteractiveElement as _, StatefulInteractiveElement as _};
    let ring = t.accent;
    el.tab_index(0)
        .role(gpui_kit::Role::Button)
        .aria_label(label)
        .focus_visible(move |s| {
            s.shadow(vec![gpui_kit::BoxShadow {
                color: ring,
                offset: gpui_kit::point(px(0.), px(0.)),
                blur_radius: px(0.),
                spread_radius: px(2.),
                // inside the edge: no layout shift, and the label stays readable on a see-through background
                inset: true,
            }])
        })
}
