//! A cached rendering boundary for the preview's fixed-size viewport.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use draftmux_storage::Target;
use gpui::{
    AnyElement, App, Bounds, Context, DispatchPhase, Entity, IntoElement, MouseMoveEvent, Pixels,
    Point, Render, ScrollHandle, WeakEntity, Window, canvas, div, point, prelude::*, px,
};
use markup::Document;

use crate::block_cache::BlockCache;
use crate::inline_text::{HunkClick, MarkHandlers};
use crate::markup_view::{self, Marks, Redline, RedlineColors, SelectBody};
use crate::selectable::{self, TextSelection};
use crate::theme;

pub(super) struct Preview {
    pub document: Option<Document>,
    pub fields: markup::frontmatter::Fields,
    pub copied_field: Option<String>,
    pub copied_task: gpui::Task<()>,
    pub target: Option<Target>,
    pub selection: Entity<TextSelection>,
    /// The selection while a redline shows: its text isn't the draft's, so
    /// selecting it leaves the editor alone.
    pub redline_selection: Entity<TextSelection>,
    pub scroll: ScrollHandle,
    pub cache: BlockCache,
    pub appearance: Option<(Target, bool, f32, f32)>,
    /// The agent is writing into the draft, or about to write its first
    /// draft; an empty preview shows a skeleton of the message instead of
    /// "Nothing to preview".
    pub writing: bool,
    /// Offers the GitHub file target above the document, for a GitHub
    /// comment draft whose paragraphs look wrapped.
    pub file_hint: bool,
    /// While reviewing, the redline shown instead of the document.
    pub redline: Option<PreviewRedline>,
    /// Scrolls along with this one, to the same fraction of its height.
    pub partner: Option<WeakEntity<Preview>>,
    pub marks: MarkState,
    pub _selections: [gpui::Subscription; 2],
}

/// A change under the pointer, and where the pointer is.
pub(super) type Hit = Option<(usize, Point<Pixels>)>;

/// Called with the change under the pointer.
pub(super) type HunkHover = Rc<dyn Fn(Hit, &mut Window, &mut App)>;

/// A reveal request's serial, and how many frames it has taken.
type Reveal = Option<(u64, u8)>;

/// What the redline's marks reported in the last frame, and the scrolling
/// that follows from it.
#[derive(Default)]
pub(super) struct MarkState {
    /// The change under the pointer during the current mouse move.
    hit: Rc<Cell<Hit>>,
    /// Where each change's first mark was laid out.
    placed: Rc<RefCell<HashMap<usize, Bounds<Pixels>>>>,
    /// The last reveal request handled, and how many frames it has taken.
    revealed: Reveal,
    /// The same for the requests to show the match a search is at.
    found_revealed: Reveal,
    /// The scroll offset this preview last drew at.
    offset: Point<Pixels>,
    pointer_inside: Rc<Cell<bool>>,
}

/// One side of a review parsed for the target ([`markup_view::redline`]),
/// and how its changes are drawn.
pub(super) struct PreviewRedline {
    pub document: Rc<Document>,
    pub marks: Rc<Marks>,
    /// The side's frontmatter, shown in place of the draft's.
    pub fields: markup::frontmatter::Fields,
    pub selected: Option<usize>,
    pub hovered: Option<usize>,
    /// Whether each change is kept.
    pub kept: Vec<bool>,
    /// A change to scroll into view, with a serial that differs for each
    /// request.
    pub reveal: Option<(usize, u64)>,
    pub on_click: HunkClick,
    /// Told after every mouse move which change, if any, is under the
    /// pointer, and where the pointer is.
    pub on_hover: HunkHover,
}

/// Frames a reveal may take: one to bring the block of what it shows on
/// screen, then one to place that itself.
const REVEAL_FRAMES: u8 = 3;
/// Space kept between what a reveal shows and the viewport's edge.
const REVEAL_MARGIN: Pixels = px(48.);

pub(super) struct FieldCopied(pub String);
impl gpui::EventEmitter<FieldCopied> for Preview {}

/// A task list item's checkbox was clicked: the item's path in the
/// [`markup::source_map::SourceMap`] of the previewed body.
pub(super) struct TaskToggled(pub Vec<usize>);
impl gpui::EventEmitter<TaskToggled> for Preview {}

/// The user's answer to the hint that a GitHub comment draft looks like a
/// wrapped repository file.
pub(super) enum FileHint {
    /// Preview the draft as a repository file.
    Accept,
    Dismiss,
}
impl gpui::EventEmitter<FileHint> for Preview {}

impl Render for Preview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let hover = self.redline.as_ref().map(|redline| {
            (
                self.marks.hit.clone(),
                redline.on_hover.clone(),
                self.marks.pointer_inside.clone(),
            )
        });
        canvas(
            move |bounds, window, cx| {
                let mut body = entity.update(cx, |this, cx| this.body(bounds, window, cx));
                body.layout_as_root(bounds.size.into(), window, cx);
                body.prepaint_at(bounds.origin, window, cx);
                entity.update(cx, |this, cx| this.scrolled(bounds, cx));
                entity.update(cx, |this, cx| {
                    let texts = this.cache.texts();
                    this.active_selection()
                        .update(cx, |selection, cx| selection.set_texts(texts, cx));
                });
                body
            },
            move |viewport, mut body: AnyElement, window, cx| {
                // Registered before the body paints, so it hears each mouse
                // move after the marks have reported the change under the
                // pointer. Another preview may share the window, so it
                // speaks for the pointer only inside this one, and as it
                // leaves.
                if let Some((hit, on_hover, inside)) = hover.clone() {
                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                        if phase != DispatchPhase::Bubble {
                            return;
                        }
                        let now = viewport.contains(&event.position);
                        let hit = hit.take();
                        if now || inside.get() {
                            on_hover(hit.map(|(hunk, _)| (hunk, event.position)), window, cx);
                        }
                        inside.set(now);
                    });
                }
                body.paint(window, cx)
            },
        )
        .size_full()
    }
}

impl Preview {
    /// The selection of the text shown: the redline's while reviewing.
    fn active_selection(&self) -> &Entity<TextSelection> {
        if self.redline.is_some() {
            &self.redline_selection
        } else {
            &self.selection
        }
    }

    /// After each layout: scrolls a change or a found match asked for into
    /// view, and the partner along with any other scrolling.
    fn scrolled(&mut self, viewport: Bounds<Pixels>, cx: &mut Context<Self>) {
        if let Some((hunk, serial)) = self.redline.as_ref().and_then(|redline| redline.reveal) {
            let placed = self.marks.placed.borrow().get(&hunk).copied();
            let block = self
                .cache
                .top_of(|block| block.marks.iter().any(|mark| mark.mark.hunk == hunk));
            let last = self.marks.revealed;
            self.marks.revealed = self.reveal(last, serial, placed, block, viewport, cx);
        } else if self.redline.is_none()
            && let Some((serial, start, placed)) = self.selection.read(cx).found_reveal()
        {
            let block = self.cache.top_at(start.key.0);
            let last = self.marks.found_revealed;
            self.marks.found_revealed = self.reveal(last, serial, placed, block, viewport, cx);
        }
        let offset = self.scroll.offset();
        if offset == self.marks.offset {
            return;
        }
        self.marks.offset = offset;
        let Some(partner) = self.partner.as_ref().and_then(WeakEntity::upgrade) else {
            return;
        };
        let max = self.scroll.max_offset().y;
        let fraction = if max > px(0.) { -offset.y / max } else { 0. };
        partner.update(cx, |partner, cx| {
            let offset = point(
                partner.scroll.offset().x,
                -(partner.scroll.max_offset().y * fraction),
            );
            if offset != partner.marks.offset {
                partner.scroll.set_offset(offset);
                partner.marks.offset = offset;
                cx.notify();
            }
        });
    }

    /// Scrolls what request `serial` asks for into view: to its block's
    /// top, `block`, while the block is off screen and unlaid, then to
    /// where it was `placed` itself. `last` is the request handled before;
    /// returns the one handled now.
    fn reveal(
        &mut self,
        last: Reveal,
        serial: u64,
        placed: Option<Bounds<Pixels>>,
        block: Option<Pixels>,
        viewport: Bounds<Pixels>,
        cx: &mut Context<Self>,
    ) -> Reveal {
        let frames = match last {
            Some((done, frames)) if done == serial => frames,
            _ => 0,
        };
        if frames >= REVEAL_FRAMES {
            return last;
        }
        let (top, bottom) = (
            viewport.top() + REVEAL_MARGIN,
            viewport.bottom() - REVEAL_MARGIN,
        );
        let shift = match placed {
            Some(mark) if mark.top() < top => top - mark.top(),
            Some(mark) if mark.bottom() > bottom => (bottom - mark.bottom()).max(top - mark.top()),
            Some(_) => px(0.),
            None => match block {
                Some(block) => top - block,
                // Not in this version at all.
                None => px(0.),
            },
        };
        let done = placed.is_some() || shift == px(0.);
        if shift != px(0.) {
            let offset = self.scroll.offset();
            let y = (offset.y + shift).clamp(-self.scroll.max_offset().y, px(0.));
            let offset = point(offset.x, y);
            self.scroll.set_offset(offset);
            // Its own scrolling, which the partner doesn't follow.
            self.marks.offset = offset;
            cx.notify();
        }
        Some((serial, if done { REVEAL_FRAMES } else { frames + 1 }))
    }

    /// The fields shown above the body: the review side's while reviewing.
    fn shown_fields(&self) -> &markup::frontmatter::Fields {
        self.redline
            .as_ref()
            .map_or(&self.fields, |redline| &redline.fields)
    }

    fn copy_field(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(value) = self.shown_fields().get(key) else {
            return;
        };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(value.clone()));
        self.copied_field = Some(key.to_owned());
        cx.emit(FieldCopied(key.to_owned()));
        self.copied_task = cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(1500))
                .await;
            this.update(cx, |this, cx| {
                this.copied_field = None;
                cx.notify();
            })
            .ok();
        });
        cx.notify();
    }

    /// The frontmatter's fields, each with its review changes drawn on its
    /// value by `redline`.
    fn file_hint(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        if !self.file_hint || self.redline.is_some() {
            return None;
        }
        let palette = theme::get(cx).palette;
        Some(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .mx(px(-4.))
                .mb(px(20.))
                .pl(px(12.))
                .pr(px(6.))
                .py(px(7.))
                .rounded(px(10.))
                .bg(palette.accent_soft)
                .text_size(px(12.5))
                .line_height(px(17.))
                .text_color(palette.text)
                .child(div().flex_1().min_w_0().child(
                    "Wrapped lines show as line breaks in a GitHub comment. \
                     Is this a repository file, such as README.md?",
                ))
                .child(
                    div()
                        .id("file-hint-accept")
                        .flex_none()
                        .h(px(22.))
                        .px(px(9.))
                        .flex()
                        .items_center()
                        .rounded(px(6.))
                        .bg(palette.accent)
                        .text_color(palette.on_accent)
                        .text_size(px(12.))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .hover(|style| style.opacity(0.9))
                        .active(|style| style.opacity(0.8))
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(|_, _, _, cx| {
                            cx.stop_propagation();
                            cx.emit(FileHint::Accept);
                        }))
                        .child("Preview as File"),
                )
                .child(
                    div()
                        .id("file-hint-dismiss")
                        .aria_label("Dismiss")
                        .flex_none()
                        .size(px(22.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(6.))
                        .hover(|style| style.bg(palette.hover))
                        .active(|style| style.opacity(0.8))
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(|_, _, _, cx| {
                            cx.stop_propagation();
                            cx.emit(FileHint::Dismiss);
                        }))
                        .child(
                            gpui::svg()
                                .path(crate::assets::CLOSE_ICON)
                                .size(px(10.))
                                .text_color(palette.muted),
                        ),
                ),
        )
    }

    fn fields_card(&self, redline: Option<&Redline>, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let fields = self.shown_fields();
        if fields.is_empty() {
            return None;
        }
        let palette = theme::get(cx).palette;
        let compact_label =
            fields.len() == 1 && (fields.contains_key("title") || fields.contains_key("subject"));
        Some(
            div()
                .rounded(px(10.))
                .bg(palette.card)
                .border_1()
                .border_color(palette.card_ring)
                .shadow(vec![super::shadow(palette.card_drop, 1., 3., 0.)])
                .mx(px(-4.))
                .mb(px(20.))
                .children(fields.iter().enumerate().map(|(index, (key, value))| {
                    let group: gpui::SharedString = format!("field-{index}").into();
                    let copied = self.copied_field.as_ref() == Some(key);
                    let copy_key = key.clone();
                    let mut chars = key.chars();
                    let label = chars
                        .next()
                        .map(|first| {
                            first.to_uppercase().collect::<String>()
                                + &chars.as_str().to_lowercase()
                        })
                        .unwrap_or_default();
                    div()
                        .group(group.clone())
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .pl(px(12.))
                        .pr(px(6.))
                        .py(px(7.))
                        .text_size(px(13.))
                        .text_color(palette.text)
                        .when(index > 0, |row| {
                            row.border_t_1().border_color(palette.hairline)
                        })
                        .child(
                            div()
                                .when(!compact_label, |label| label.w(px(64.)))
                                .flex_shrink_0()
                                .text_color(palette.muted)
                                .child(label),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .when(key == "title" || key == "subject", |value| {
                                    value.font_weight(gpui::FontWeight::SEMIBOLD)
                                })
                                .child(match redline {
                                    Some(redline) => {
                                        markup_view::field_value(value, index, redline)
                                            .into_any_element()
                                    }
                                    None => value.clone().into_any_element(),
                                }),
                        )
                        .child(
                            div()
                                .id(("copy-field", index))
                                .focusable()
                                .tab_index(0)
                                .aria_label(format!("Copy {key}"))
                                .cursor_pointer()
                                .size(px(28.))
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(5.))
                                .opacity(if copied { 1. } else { 0. })
                                .group_hover(group, |style| style.opacity(1.))
                                .focus_visible(|style| style.opacity(1.).bg(palette.hover))
                                .hover(|style| style.bg(palette.hover))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.copy_field(&copy_key, cx);
                                }))
                                .child(
                                    gpui::svg()
                                        .path(if copied {
                                            crate::assets::TOAST_DONE_ICON
                                        } else {
                                            crate::assets::COPY_ICON
                                        })
                                        .size(px(14.))
                                        .text_color(if copied {
                                            palette.toast_done
                                        } else {
                                            palette.muted
                                        }),
                                ),
                        )
                })),
        )
    }

    fn body(
        &mut self,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let _timing = editor::timing::span("preview-elements");
        let frame = editor::timing::span("preview-frame");
        let palette = theme::palette(cx);
        let (found, found_current) = (palette.found, palette.found_current);
        self.selection.update(cx, |selection, _| {
            selection.set_found_colors(found, found_current)
        });
        let theme = theme::get(cx);
        let appearance = self
            .target
            .map(|target| (target, theme.dark, window.scale_factor(), theme.zoom));
        if self.appearance != appearance {
            self.cache.clear();
            self.appearance = appearance;
        }
        let document = match &self.redline {
            Some(redline) => Some(&*redline.document),
            None => self.document.as_ref(),
        };
        if document.is_none_or(|doc| doc.blocks.is_empty()) {
            self.cache.reset();
        }
        // px_6 is 1.5 rem on each side of the scrolling viewport.
        let width = (bounds.size.width - window.rem_size() * 3.).max(px(0.));
        self.marks.placed.borrow_mut().clear();
        let redline = self.redline.as_ref().map(|redline| {
            let hit = self.marks.hit.clone();
            let placed = self.marks.placed.clone();
            Rc::new(Redline {
                marks: redline.marks.clone(),
                selected: redline.selected,
                hovered: redline.hovered,
                kept: redline.kept.clone(),
                colors: RedlineColors::new(&theme.palette),
                handlers: Rc::new(MarkHandlers {
                    click: redline.on_click.clone(),
                    hover: Rc::new(move |hunk, position| hit.set(Some((hunk, position)))),
                    placed: Rc::new(move |hunk, bounds| {
                        placed.borrow_mut().entry(hunk).or_insert(bounds);
                    }),
                }),
            })
        });
        let (body, background) = match (document, self.target) {
            (Some(doc), Some(target)) if !doc.blocks.is_empty() => {
                let mut style = markup_view::style(target, theme.dark).zoomed(theme.zoom);
                style.redline = redline.clone();
                let preview = cx.entity().downgrade();
                style.on_task = Some(Rc::new(move |path, _, cx| {
                    preview
                        .update(cx, |_, cx| cx.emit(TaskToggled(path.to_vec())))
                        .ok();
                }));
                let style = Rc::new(style);
                let select = Some(SelectBody {
                    state: self.active_selection().clone(),
                    body: 0,
                    color: theme.palette.selection,
                    registry: None,
                });
                (
                    markup_view::render_preview(
                        doc,
                        style.clone(),
                        &self.scroll,
                        select,
                        (&mut self.cache, width),
                        window,
                        cx,
                    ),
                    Some(style.background),
                )
            }
            _ if self.writing
                && let Some(target) = self.target =>
            {
                let style = markup_view::style(target, theme.dark).zoomed(theme.zoom);
                (
                    super::skeleton::render(target, &style),
                    Some(style.background),
                )
            }
            _ => (
                div()
                    // pt_5 and pb_10 take 3.75 rem of the viewport's height.
                    .h((bounds.size.height - window.rem_size() * 3.75).max(px(0.)))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(14.))
                    .text_color(theme.palette.faint)
                    .child("Nothing to preview")
                    .into_any_element(),
                None,
            ),
        };

        selectable::area(self.active_selection(), cx)
            .id("preview")
            .size_full()
            .when_some(background, |this, background| this.bg(background))
            .overflow_y_scroll()
            .restrict_scroll_to_axis()
            .track_scroll(&self.scroll)
            .px_6()
            .pt_5()
            .pb_10()
            .children(self.file_hint(cx))
            .children(self.fields_card(redline.as_deref(), cx))
            .child(body)
            .child(canvas(|_, _, _| (), move |_, _, _, _| drop(frame)).absolute())
            .into_any_element()
    }
}
