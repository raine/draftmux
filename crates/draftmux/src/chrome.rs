//! Window chrome: the unified toolbar, translucent sidebar and opaque content
//! panes of the "Native" prototype variant (docs/prototype/index.html).
//!
//! The window uses `WindowBackgroundAppearance::Blurred`, so anything painted
//! with alpha below 1 shows the blurred desktop. The root paints nothing; the
//! sidebar column paints a translucent tint and the content panes are opaque.

use crate::format::{Format, Formatter};
use draftmux_storage::Draft;
use editor::Editor;
use gpui::{
    Context, Entity, Focusable, FontWeight, MouseButton, Pixels, Point, Rgba, SharedString, Window,
    WindowControlArea, div, point, prelude::*, px, rgb, rgba,
};

const TOOLBAR_HEIGHT: Pixels = px(52.);
const SIDEBAR_WIDTH: Pixels = px(228.);
const CHAT_WIDTH: Pixels = px(340.);
/// Origin of the close button; vertically centres the buttons in the toolbar.
pub const TRAFFIC_LIGHT_POSITION: Point<Pixels> = point(px(20.), px(19.));

// Light palette of the Native variant.
const SIDEBAR_BG: u32 = 0xe8e8edbd;
const SIDEBAR_EDGE: u32 = 0x00000012;
const HAIRLINE: u32 = 0x00000016;
const TEXT: u32 = 0x1d1d1f;
const FAINT: u32 = 0x3c3c4373;
const PANE_BG: u32 = 0xffffff;
const CHAT_BG: u32 = 0xfbfbfc;

/// Placeholder draft until drafts are loaded from storage.
const SAMPLE_DRAFT: &str = "# Draft\n\nWrite the message here. Markdown such as **bold**, *emphasis*, \
`code` and [links](https://github.com) is highlighted.\n";

pub struct Chrome {
    /// Set on toolbar mouse-down; the first mouse-move then hands the drag to
    /// the window server, so a plain click never starts a move.
    should_move: bool,
    /// The draft shown in the window, set by `draftmux open`.
    draft: Option<Draft>,
    editor: Entity<Editor>,
    formatter: Entity<Formatter>,
}

impl Render for Chrome {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .text_color(rgb(TEXT))
            .on_action(cx.listener(|this, _: &Format, _, cx| {
                this.formatter
                    .update(cx, |formatter, cx| formatter.format(cx));
            }))
            .child(self.toolbar(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(
                        sidebar_column().pt_2().px_4().child(
                            div()
                                .text_size(px(11.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgba(FAINT))
                                .child("Drafts"),
                        ),
                    )
                    .child(pane(rgb(PANE_BG)).flex_1().child(self.editor.clone()))
                    .child(
                        pane(rgb(PANE_BG)).flex_1().border_l_1().child(
                            div()
                                .id("preview")
                                .size_full()
                                .overflow_y_scroll()
                                .px_6()
                                .pt_5()
                                .pb_10()
                                .child(crate::markup_view::sample()),
                        ),
                    )
                    .child(pane(rgb(CHAT_BG)).w(CHAT_WIDTH).border_l_1()),
            )
    }
}

impl Chrome {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| Editor::new(SAMPLE_DRAFT, cx));
        window.focus(&editor.focus_handle(cx), cx);
        let formatter = cx.new(|cx| Formatter::new(editor.clone(), cx));
        Self {
            should_move: false,
            draft: None,
            editor,
            formatter,
        }
    }

    pub fn focus_draft(&mut self, draft: Draft, cx: &mut Context<Self>) {
        eprintln!("draftmux: focused draft {}", draft.id);
        let dir = draft.dir.clone();
        self.formatter
            .update(cx, |formatter, cx| formatter.set_draft_dir(dir, cx));
        self.draft = Some(draft);
        cx.notify();
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("toolbar")
            .h(TOOLBAR_HEIGHT)
            .flex_none()
            .flex()
            // gpui's macOS backend ignores control areas; the handlers below
            // move the window.
            .window_control_area(WindowControlArea::Drag)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.should_move = true),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.should_move = false),
            )
            .on_mouse_down_out(cx.listener(|this, _, _, _| this.should_move = false))
            .on_mouse_move(cx.listener(|this, _, window, _| {
                if this.should_move {
                    this.should_move = false;
                    window.start_window_move();
                }
            }))
            .on_click(|event, window, _| {
                if event.click_count() == 2 {
                    window.titlebar_double_click();
                }
            })
            // Traffic lights sit over this segment.
            .child(sidebar_column())
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .pl(px(18.))
                    .pr_3()
                    .bg(rgb(CHAT_BG))
                    .border_b_1()
                    .border_color(rgba(HAIRLINE))
                    .text_size(px(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(match &self.draft {
                        Some(draft) => SharedString::from(format!("Draft {}", draft.id)),
                        None => SharedString::new_static("Draftmux"),
                    })
                    .child(div().flex_1())
                    .child(self.formatter.clone()),
            )
    }
}

/// The translucent sidebar strip, shared by the toolbar and the body so the
/// two read as one column.
fn sidebar_column() -> gpui::Div {
    div()
        .w(SIDEBAR_WIDTH)
        .flex_none()
        .bg(rgba(SIDEBAR_BG))
        .border_r_1()
        .border_color(rgba(SIDEBAR_EDGE))
}

fn pane(bg: Rgba) -> gpui::Div {
    div().h_full().min_w_0().bg(bg).border_color(rgba(HAIRLINE))
}
