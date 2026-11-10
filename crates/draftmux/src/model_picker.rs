//! A searchable model menu grouped by provider, for the chat's model switch
//! and the Settings window's model fields.
//!
//! The picker shows a [`ModelList`] and reports what the user picked; the
//! owner decides what picking means and closes it. Typing filters by
//! provider and model ID, ↑/↓ move the highlight, Return picks and Escape
//! dismisses.

use draftmux_pi_profiles::{Availability, ModelRef, availability};
use gpui::{
    Animation, AnimationExt, App, Context, EventEmitter, FocusHandle, Focusable, FontWeight,
    KeyBinding, KeyDownEvent, MouseButton, ScrollStrategy, SharedString, UniformListScrollHandle,
    Window, actions, div, ease_out_quint, prelude::*, px, uniform_list,
};

use crate::profiles::ModelList;
use crate::theme;

actions!(model_picker, [Dismiss]);

const KEY_CONTEXT: &str = "ModelPicker";
const WIDTH: gpui::Pixels = px(320.);
const ROW_HEIGHT: gpui::Pixels = px(24.);
/// How long a newly highlighted row takes to fade in.
const HIGHLIGHT_FADE: std::time::Duration = std::time::Duration::from_millis(120);
const VISIBLE_ROWS: usize = 14;

/// Escape is an action rather than a key in [`ModelPicker::key_down`], so it
/// beats Escape bindings of the views that host the picker, such as the
/// composer's Stop.
pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", Dismiss, Some(KEY_CONTEXT))]);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerEvent {
    Picked(ModelRef),
    Dismissed,
    /// The list failed to load and the user asked to try again.
    Retry,
}

/// One line of the menu: a provider heading or a model under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Provider(SharedString),
    Model(ModelRef),
}

/// The models matching `query`, under a heading per provider, sorted by
/// provider and ID. Each whitespace-separated word of the query must occur,
/// ignoring case, in `provider/id`.
pub fn filter_rows(models: &[ModelRef], query: &str) -> Vec<Row> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut matches: Vec<&ModelRef> = models
        .iter()
        .filter(|model| {
            let haystack = model.to_string().to_lowercase();
            words.iter().all(|word| haystack.contains(word.as_str()))
        })
        .collect();
    matches.sort();
    let mut rows = Vec::new();
    for model in matches {
        if !matches!(rows.last(), Some(Row::Model(last)) if last.provider == model.provider) {
            rows.push(Row::Provider(model.provider.clone().into()));
        }
        rows.push(Row::Model(model.clone()));
    }
    rows
}

pub struct ModelPicker {
    focus_handle: FocusHandle,
    models: ModelList,
    /// Checked in the list.
    current: Option<ModelRef>,
    /// A model a setting names, warned about when the profile lacks it.
    configured: Option<ModelRef>,
    profile_name: SharedString,
    query: String,
    rows: Vec<Row>,
    /// Index into `rows`; always a model row.
    highlight: Option<usize>,
    scroll: UniformListScrollHandle,
}

impl EventEmitter<PickerEvent> for ModelPicker {}

impl Focusable for ModelPicker {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ModelPicker {
    /// `profile_name` names the profile in messages ("isn't available in
    /// work").
    pub fn new(
        profile_name: impl Into<SharedString>,
        models: ModelList,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut picker = Self {
            focus_handle: cx.focus_handle(),
            models,
            current: None,
            configured: None,
            profile_name: profile_name.into(),
            query: String::new(),
            rows: Vec::new(),
            highlight: None,
            scroll: UniformListScrollHandle::new(),
        };
        picker.refilter();
        picker
    }

    pub fn set_models(&mut self, models: ModelList, cx: &mut Context<Self>) {
        self.models = models;
        self.refilter();
        cx.notify();
    }

    /// The model to check, and to highlight when the menu opens.
    pub fn set_current(&mut self, current: Option<ModelRef>, cx: &mut Context<Self>) {
        self.current = current;
        self.refilter();
        cx.notify();
    }

    pub fn set_configured(&mut self, configured: Option<ModelRef>, cx: &mut Context<Self>) {
        self.configured = configured;
        cx.notify();
    }

    fn refilter(&mut self) {
        self.rows = match &self.models {
            ModelList::Loaded(models) => filter_rows(models, &self.query),
            _ => Vec::new(),
        };
        let current = self.current.as_ref().and_then(|current| {
            self.rows
                .iter()
                .position(|row| matches!(row, Row::Model(m) if m == current))
        });
        self.highlight = if self.query.is_empty() { current } else { None }.or_else(|| {
            self.rows
                .iter()
                .position(|row| matches!(row, Row::Model(_)))
        });
        if let Some(ix) = self.highlight {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Center);
        }
    }

    fn move_highlight(&mut self, down: bool, cx: &mut Context<Self>) {
        let Some(start) = self.highlight else { return };
        let next = if down {
            (start + 1..self.rows.len()).find(|&ix| matches!(self.rows[ix], Row::Model(_)))
        } else {
            (0..start)
                .rev()
                .find(|&ix| matches!(self.rows[ix], Row::Model(_)))
        };
        if let Some(ix) = next {
            self.highlight = Some(ix);
            self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
            cx.notify();
        }
    }

    fn pick(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(Row::Model(model)) = self.rows.get(ix) {
            cx.emit(PickerEvent::Picked(model.clone()));
        }
    }

    fn edit_query(&mut self, edit: impl FnOnce(&mut String), cx: &mut Context<Self>) {
        edit(&mut self.query);
        self.refilter();
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let mods = keystroke.modifiers;
        match keystroke.key.as_str() {
            "up" => self.move_highlight(false, cx),
            "down" => self.move_highlight(true, cx),
            "enter" => {
                if let Some(ix) = self.highlight {
                    self.pick(ix, cx);
                }
            }
            "backspace" if mods.platform => self.edit_query(String::clear, cx),
            "backspace" if mods.alt => self.edit_query(
                |query| {
                    let end = query.trim_end().len();
                    let start = query[..end].rfind(char::is_whitespace).map_or(0, |i| i + 1);
                    query.truncate(start);
                },
                cx,
            ),
            "backspace" => self.edit_query(
                |query| {
                    query.pop();
                },
                cx,
            ),
            "v" if mods.platform => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
                    self.edit_query(|query| query.push_str(&line), cx);
                }
            }
            _ => {
                if mods.platform || mods.control || mods.function {
                    return;
                }
                match &keystroke.key_char {
                    Some(text) if !text.chars().any(char::is_control) => {
                        self.edit_query(|query| query.push_str(text), cx)
                    }
                    _ => return,
                }
            }
        }
        cx.stop_propagation();
    }

    /// The warning for a configured model the profile doesn't have.
    fn warning(&self) -> Option<SharedString> {
        let configured = self.configured.as_ref()?;
        (availability(self.models.loaded(), configured) == Availability::Missing).then(|| {
            format!(
                "{} isn’t available in the {} profile",
                configured.id, self.profile_name
            )
            .into()
        })
    }

    /// Why the list is empty, if it is.
    fn status(&self) -> Option<(SharedString, bool)> {
        match &self.models {
            ModelList::Loading => Some(("Loading models…".into(), false)),
            ModelList::Failed(err) => Some((format!("Couldn’t list models: {err}").into(), true)),
            ModelList::Loaded(models) if models.is_empty() => Some((
                format!("No models available in the {} profile", self.profile_name).into(),
                false,
            )),
            ModelList::Loaded(_) if self.rows.is_empty() => {
                Some(("No matching models".into(), false))
            }
            ModelList::Loaded(_) => None,
        }
    }

    fn render_row(&self, ix: usize, cx: &mut Context<Self>) -> gpui::AnyElement {
        let palette = *theme::palette(cx);
        let row = div()
            .id(ix)
            .w_full()
            .h(ROW_HEIGHT)
            .flex()
            .items_center()
            .px_2();
        match &self.rows[ix] {
            Row::Provider(name) => row
                .pt_1()
                .text_size(px(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(palette.muted)
                .child(name.clone())
                .into_any_element(),
            Row::Model(model) => {
                let highlighted = self.highlight == Some(ix);
                let current = self.current.as_ref() == Some(model);
                let row = row
                    .rounded(px(5.))
                    .active(|style| style.opacity(0.8))
                    .text_size(px(13.))
                    .when(highlighted, |row| row.text_color(palette.on_accent))
                    .child(
                        div()
                            .w(px(16.))
                            .flex_none()
                            .child(if current { "✓" } else { "" }),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(model.id.clone()))
                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                        if this.highlight != Some(ix) {
                            this.highlight = Some(ix);
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| this.pick(ix, cx)));
                if !highlighted {
                    return row.into_any_element();
                }
                row.with_animation(
                    ("highlight", ix),
                    Animation::new(HIGHLIGHT_FADE).with_easing(ease_out_quint()),
                    move |row, delta| row.bg(palette.accent.opacity(delta)),
                )
                .into_any_element()
            }
        }
    }
}

impl Render for ModelPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let list_height = ROW_HEIGHT * self.rows.len().min(VISIBLE_ROWS) as f32;
        let palette = *theme::palette(cx);
        div()
            .id("model-picker")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(PickerEvent::Dismissed)))
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(PickerEvent::Dismissed)))
            .w(WIDTH)
            .flex()
            .flex_col()
            .p_1()
            .gap_1()
            .bg(palette.menu)
            .rounded(px(8.))
            .border_1()
            .border_color(palette.hairline)
            .shadow_lg()
            .text_color(palette.text)
            .child(
                div()
                    .h(px(28.))
                    .mx_1()
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .bg(palette.menu_field)
                    .border_1()
                    .border_color(palette.hairline)
                    .text_size(px(13.))
                    .when(!self.query.is_empty(), |field| {
                        field.child(SharedString::from(self.query.clone()))
                    })
                    .child(div().w(px(1.)).h(px(15.)).bg(palette.accent))
                    .when(self.query.is_empty(), |field| {
                        field.child(div().text_color(palette.faint).child("Search models"))
                    }),
            )
            .when_some(self.warning(), |menu, warning| {
                menu.child(
                    div()
                        .px_2()
                        .py_1()
                        .text_size(px(12.))
                        .text_color(palette.warning)
                        .child(SharedString::from(format!("⚠ {warning}"))),
                )
            })
            .when_some(self.status(), |menu, (status, retry)| {
                menu.child(
                    div()
                        .px_2()
                        .py_1()
                        .flex()
                        .gap_2()
                        .text_size(px(12.))
                        .text_color(palette.muted)
                        .child(div().flex_1().child(status))
                        .when(retry, |row| {
                            row.child(
                                div()
                                    .id("retry")
                                    .active(|style| style.opacity(0.8))
                                    .text_color(palette.accent)
                                    .child("Retry")
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|_, _, _, cx| cx.emit(PickerEvent::Retry)),
                                    ),
                            )
                        }),
                )
            })
            .when(!self.rows.is_empty(), |menu| {
                menu.child(
                    uniform_list(
                        "models",
                        self.rows.len(),
                        cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                            range.map(|ix| this.render_row(ix, cx)).collect()
                        }),
                    )
                    .track_scroll(&self.scroll)
                    .h(list_height),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A catalog the size of a real profile: 120 models over 6 providers.
    fn catalog() -> Vec<ModelRef> {
        let providers = [
            "portkey",
            "anthropic",
            "openai",
            "google",
            "deepseek",
            "minimax",
        ];
        (0..120)
            .map(|i| {
                let provider = providers[i % providers.len()];
                ModelRef::new(provider, format!("model-{i:03}"))
            })
            .collect()
    }

    fn models(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .filter_map(|row| match row {
                Row::Model(model) => Some(model.to_string()),
                Row::Provider(_) => None,
            })
            .collect()
    }

    #[test]
    fn groups_by_provider_in_order() {
        let rows = filter_rows(&catalog(), "");
        assert_eq!(rows.len(), 126);
        let headings: Vec<_> = rows
            .iter()
            .filter_map(|row| match row {
                Row::Provider(name) => Some(name.to_string()),
                Row::Model(_) => None,
            })
            .collect();
        assert_eq!(
            headings,
            [
                "anthropic",
                "deepseek",
                "google",
                "minimax",
                "openai",
                "portkey"
            ]
        );
        assert_eq!(rows[1], Row::Model(ModelRef::new("anthropic", "model-001")));
    }

    #[test]
    fn every_word_must_match_and_empty_groups_go() {
        let list = [
            ModelRef::new("anthropic", "claude-opus-5-5"),
            ModelRef::new("portkey", "claude-opus-5-5"),
            ModelRef::new("portkey", "claude-sonnet-5-5"),
            ModelRef::new("openai", "gpt-6"),
        ];
        assert_eq!(
            models(&filter_rows(&list, "OPUS")),
            ["anthropic/claude-opus-5-5", "portkey/claude-opus-5-5"]
        );
        assert_eq!(
            filter_rows(&list, "port opus"),
            [Row::Provider("portkey".into()), Row::Model(list[1].clone())]
        );
        assert!(filter_rows(&list, "gemini").is_empty());
    }
}
