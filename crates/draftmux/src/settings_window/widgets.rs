//! Form pieces of the Settings window, drawn like AppKit's grouped forms
//! (docs/prototype/settings.html).

use std::ops::Deref;

use gpui::{
    AnyElement, App, BoxShadow, ClickEvent, ElementId, FontWeight, Hsla, IntoElement, SharedString,
    Window, div, point, prelude::*, px, rgba, svg,
};

use crate::assets::{RESET_ICON, UPDOWN_ICON};
use crate::theme::{self, Palette};

pub const MONO: &str = "Menlo";

/// The app palette plus the colours only the Settings window uses, for the
/// current appearance.
#[derive(Clone, Copy)]
pub struct Colors {
    palette: Palette,
    pub window: Hsla,
    pub group: Hsla,
    pub row_line: Hsla,
    /// Ring around pop-ups, buttons and the instructions field.
    pub field_ring: Hsla,
    /// Lines between the sidebar, header and content.
    pub divider: Hsla,
    /// Background of the inherited instructions.
    pub inset: Hsla,
    pub switch_off: Hsla,
    pub chevron: Hsla,
    pub pi: Hsla,
    pub pi_soft: Hsla,
    pub ok: Hsla,
    pub warning_soft: Hsla,
}

impl Colors {
    pub fn get(cx: &App) -> Self {
        let theme = theme::get(cx);
        let palette = theme.palette;
        if theme.dark {
            Self {
                palette,
                window: hex(0x2a2a2dff),
                group: hex(0x2f2f33ff),
                row_line: hex(0xffffff12),
                field_ring: hex(0xffffff1f),
                divider: hex(0x0000004d),
                inset: hex(0xffffff0a),
                switch_off: hex(0x7878803d),
                chevron: hex(0x98989dff),
                pi: hex(0xa68bffff),
                pi_soft: hex(0xa68bff29),
                ok: hex(0x32d74bff),
                warning_soft: hex(0xffb34026),
            }
        } else {
            Self {
                palette,
                window: hex(0xececeeff),
                group: hex(0xffffffff),
                row_line: hex(0x00000013),
                field_ring: hex(0x00000029),
                divider: hex(0x0000001a),
                inset: hex(0x0000000a),
                switch_off: hex(0x7878803d),
                chevron: hex(0x888888ff),
                pi: hex(0x7c4dffff),
                pi_soft: hex(0x7c4dff1a),
                ok: hex(0x1f9d55ff),
                warning_soft: hex(0xb250001a),
            }
        }
    }
}

impl Deref for Colors {
    type Target = Palette;

    fn deref(&self) -> &Palette {
        &self.palette
    }
}

/// A colour from `0xRRGGBBAA`.
fn hex(value: u32) -> Hsla {
    rgba(value).into()
}

/// The hairline ring and soft drop shadow of a control.
fn control_shadow(c: &Colors) -> Vec<BoxShadow> {
    vec![
        BoxShadow {
            color: c.field_ring,
            offset: point(px(0.), px(0.)),
            blur_radius: px(0.),
            spread_radius: px(0.5),
            inset: false,
        },
        BoxShadow {
            color: hex(0x00000012),
            offset: point(px(0.), px(1.)),
            blur_radius: px(1.5),
            spread_radius: px(0.),
            inset: false,
        },
    ]
}

pub fn group_title(c: &Colors, title: impl Into<SharedString>) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .mt(px(18.))
        .mb(px(6.))
        .mx(px(4.))
        .text_size(px(12.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(c.muted)
        .child(title.into())
}

/// A rounded group of rows separated by hairlines.
pub fn group(c: &Colors, rows: Vec<AnyElement>) -> impl IntoElement {
    let mut group = div()
        .flex()
        .flex_col()
        .rounded(px(10.))
        .bg(c.group)
        .shadow(vec![
            BoxShadow {
                color: hex(0x0000001a),
                offset: point(px(0.), px(0.)),
                blur_radius: px(0.),
                spread_radius: px(0.5),
                inset: false,
            },
            BoxShadow {
                color: hex(0x0000000a),
                offset: point(px(0.), px(0.5)),
                blur_radius: px(1.),
                spread_radius: px(0.),
                inset: false,
            },
        ]);
    for (ix, row) in rows.into_iter().enumerate() {
        group = group.child(
            div()
                .when(ix > 0, |row| row.border_t(px(0.5)).border_color(c.row_line))
                .child(row),
        );
    }
    group
}

/// A form row: label and description on the left, control on the right.
pub fn row(
    label: impl IntoElement,
    description: Vec<AnyElement>,
    control: impl IntoElement,
) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(12.))
        .min_h(px(40.))
        .px(px(12.))
        .py(px(7.))
        .child(
            div()
                .flex_1()
                .min_w(px(150.))
                .flex()
                .flex_col()
                .child(label)
                .children(description),
        )
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(6.))
                .child(control),
        )
}

/// The row label, with the dot of an overridden field.
pub fn label(c: &Colors, text: impl Into<SharedString>, overridden: bool) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .when(overridden, |label| {
            label.child(div().size(px(6.)).rounded_full().bg(c.accent))
        })
        .child(text.into())
}

pub fn description(c: &Colors, text: impl Into<SharedString>) -> AnyElement {
    div()
        .mt(px(2.))
        .text_size(px(11.5))
        .text_color(c.muted)
        .child(text.into())
        .into_any_element()
}

pub fn warning(c: &Colors, text: impl Into<SharedString>) -> AnyElement {
    div()
        .mt(px(2.))
        .text_size(px(11.5))
        .text_color(c.warning)
        .child(format!("⚠ {}", text.into()))
        .into_any_element()
}

pub fn note(c: &Colors, text: impl Into<SharedString>) -> impl IntoElement {
    div()
        .mt(px(8.))
        .mx(px(4.))
        .text_size(px(11.5))
        .line_height(px(11.5 * 1.45))
        .text_color(c.muted)
        .child(text.into())
}

pub fn mono(text: impl Into<SharedString>) -> impl IntoElement {
    div()
        .font_family(MONO)
        .text_size(px(11.5))
        .child(text.into())
}

/// Where an inherited value comes from.
pub fn source_tag(c: &Colors, text: impl Into<SharedString>, from_pi: bool) -> impl IntoElement {
    div()
        .text_size(px(11.))
        .text_color(if from_pi { c.pi } else { c.faint })
        .whitespace_nowrap()
        .child(text.into())
}

/// A pop-up button showing `text`; muted when it shows an inherited value.
pub fn popup_button(
    c: &Colors,
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
    inherited: bool,
    width: Option<f32>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .items_center()
        .h(px(22.))
        .pl(px(8.))
        .pr(px(6.))
        .gap(px(6.))
        .rounded(px(5.5))
        .bg(c.control)
        .shadow(control_shadow(c))
        .text_color(if inherited { c.muted } else { c.text })
        .map(|button| match width {
            Some(width) => button.w(px(width)),
            None => button.max_w(px(260.)),
        })
        .child(div().flex_1().min_w_0().truncate().child(text.into()))
        .child(
            svg()
                .path(UPDOWN_ICON)
                .w(px(9.))
                .h(px(12.))
                .flex_none()
                .text_color(c.chevron),
        )
        .on_click(on_click)
}

/// A push button such as "Choose…".
pub fn push_button(
    c: &Colors,
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let pressed = c.control_hover;
    div()
        .id(id)
        .flex()
        .items_center()
        .h(px(22.))
        .px(px(9.))
        .rounded(px(5.5))
        .bg(c.control)
        .shadow(control_shadow(c))
        .whitespace_nowrap()
        .active(move |button| button.bg(pressed))
        .child(text.into())
        .on_click(on_click)
}

/// The reset button of an overridden field.
pub fn reset_button(
    c: &Colors,
    id: impl Into<ElementId>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let (hover, text) = (c.selected_row, c.text);
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .size(px(20.))
        .rounded(px(5.))
        .text_color(c.muted)
        .hover(move |button| button.bg(hover).text_color(text))
        .child(svg().path(RESET_ICON).size(px(12.)).text_color(c.muted))
        .on_click(on_click)
}

/// An on/off switch; faded when it shows an inherited value.
pub fn switch(
    c: &Colors,
    id: impl Into<ElementId>,
    on: bool,
    inherited: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex_none()
        .w(px(32.))
        .h(px(19.))
        .p(px(2.))
        .rounded_full()
        .bg(if on { c.accent } else { c.switch_off })
        .when(inherited, |switch| switch.opacity(0.55))
        .flex()
        .when(on, |switch| switch.justify_end())
        .child(
            div()
                .size(px(15.))
                .rounded_full()
                .bg(c.on_accent)
                .shadow(vec![BoxShadow {
                    color: hex(0x00000040),
                    offset: point(px(0.), px(1.)),
                    blur_radius: px(2.),
                    spread_radius: px(0.),
                    inset: false,
                }]),
        )
        .on_click(on_click)
}

/// A small rounded label such as "Loaded".
pub fn badge(c: &Colors, text: impl Into<SharedString>, kind: BadgeKind) -> impl IntoElement {
    let (bg, fg) = match kind {
        BadgeKind::Plain => (c.selected_row, c.muted),
        BadgeKind::Pi => (c.pi_soft, c.pi),
        BadgeKind::Warning => (c.warning_soft, c.warning),
    };
    div()
        .text_size(px(10.5))
        .px(px(6.))
        .py(px(1.))
        .rounded_full()
        .bg(bg)
        .text_color(fg)
        .whitespace_nowrap()
        .child(text.into())
}

#[derive(Clone, Copy)]
pub enum BadgeKind {
    Plain,
    Pi,
    Warning,
}

/// One item of a pop-up menu.
pub struct MenuItem<A> {
    pub label: SharedString,
    pub checked: bool,
    /// `None` draws a separator.
    pub action: Option<A>,
}

/// The open menu's panel: a check column and one row per item.
pub fn menu_panel<A: Clone + 'static>(
    c: &Colors,
    items: &[MenuItem<A>],
    on_pick: impl Fn(A, &mut Window, &mut App) + Clone + 'static,
) -> impl IntoElement {
    let mut panel = div()
        .flex()
        .flex_col()
        .min_w(px(180.))
        .max_w(px(360.))
        .p(px(4.))
        .rounded(px(7.))
        .bg(c.menu)
        .shadow(vec![
            BoxShadow {
                color: hex(0x00000033),
                offset: point(px(0.), px(0.)),
                blur_radius: px(0.),
                spread_radius: px(0.5),
                inset: false,
            },
            BoxShadow {
                color: hex(0x00000029),
                offset: point(px(0.), px(6.)),
                blur_radius: px(18.),
                spread_radius: px(0.),
                inset: false,
            },
        ]);
    let (accent, on_accent) = (c.accent, c.on_accent);
    for (ix, item) in items.iter().enumerate() {
        panel = panel.child(match &item.action {
            None => div()
                .my(px(4.))
                .mx(px(8.))
                .h(px(0.5))
                .bg(c.menu_line)
                .into_any_element(),
            Some(action) => {
                let action = action.clone();
                let on_pick = on_pick.clone();
                div()
                    .id(("menu-item", ix))
                    .flex()
                    .items_center()
                    .h(px(22.))
                    .pr(px(10.))
                    .rounded(px(4.))
                    .text_color(c.text)
                    .hover(move |item| item.bg(accent).text_color(on_accent))
                    .child(
                        div()
                            .w(px(20.))
                            .flex_none()
                            .text_center()
                            .child(if item.checked { "✓" } else { "" }),
                    )
                    .child(div().truncate().child(item.label.clone()))
                    .on_click(move |_, window, cx| on_pick(action.clone(), window, cx))
                    .into_any_element()
            }
        });
    }
    panel
}
