//! The selection actions popover (docs/design.md, Selection menu): actions
//! for the editor selection, then the options pi returns, in a blurred popup
//! window above the selection.
//!
//! gpui can't anchor a popup to its parent on macOS, so the popover is a
//! `WindowKind::PopUp` panel placed in screen coordinates. It holds no state
//! of its own: [`SelectionPopover`] owns the state, and the panel is
//! resized, reopened or closed to match it after each frame of the main
//! window. gpui can't move a window, but a resize keeps its bottom edge, so a
//! popover above the selection grows upwards in place.
//!
//! The main window keeps keyboard focus, so the editor keeps its selection
//! highlight; the popover's keys are bound in key contexts around the editor
//! (see [`SelectionPopover::wrap`]). Only the instruction field makes the
//! panel the key window.

use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use draftmux_storage::Draft;
use editor::{Editor, EditorEvent, EditorStyle};
use gpui::{
    Action, AnyElement, AnyWindowHandle, App, Bounds, Context, Entity, Focusable, KeyBinding,
    Pixels, SharedString, Size, Subscription, Task, Window, WindowBackgroundAppearance,
    WindowBounds, WindowHandle, WindowKind, WindowOptions, actions, canvas, div, point, prelude::*,
    px, size,
};
use shell_env::ShellEnv;

use crate::resources;
use crate::selection::{self, Action as SelectionAction, Request, SelectionConfig};
use crate::theme::{self, Palette};

actions!(selection_popover, [Dismiss, Confirm, Next, Previous]);

/// Applies the option with this index.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = selection_popover, no_json)]
pub struct Pick(pub usize);

/// Key contexts around the editor while the popover is shown, and in the
/// panel while the instruction field has focus.
const POPOVER_CONTEXT: &str = "SelectionPopover";
const RESULTS_CONTEXT: &str = "SelectionResults";
const INSTRUCTION_CONTEXT: &str = "SelectionInstruction";

/// How long the selection must stay put before the popover appears, so
/// shift-arrow selections don't flash it on every step.
const SHOW_DELAY: Duration = Duration::from_millis(200);
/// Between the selection and the popover.
const GAP: Pixels = px(6.);
/// Between the popover and the edges of the editor pane.
const MARGIN: Pixels = px(8.);
const PADDING: Pixels = px(5.);
const RESULTS_WIDTH: Pixels = px(380.);
const INSTRUCTION_WIDTH: Pixels = px(320.);

const INSTRUCTION_PLACEHOLDER: &str = "e.g. less formal, mention the timeout";

pub fn init(cx: &mut App) {
    let popover = Some("SelectionPopover > Editor");
    let results = Some("SelectionResults > Editor");
    let instruction = Some("SelectionInstruction > Editor");
    let mut bindings = vec![
        KeyBinding::new("escape", Dismiss, popover),
        KeyBinding::new("up", Previous, results),
        KeyBinding::new("down", Next, results),
        KeyBinding::new("enter", Confirm, results),
        KeyBinding::new("escape", Dismiss, instruction),
        KeyBinding::new("enter", Confirm, instruction),
    ];
    for (index, key) in ["1", "2", "3", "4", "5", "6", "7", "8", "9"]
        .into_iter()
        .enumerate()
    {
        bindings.push(KeyBinding::new(key, Pick(index), results));
    }
    // Bound after the editor's keys, so these win at the same depth.
    cx.bind_keys(bindings);
}

/// The popover for one editor's selection.
pub struct SelectionPopover {
    editor: Entity<Editor>,
    main: AnyWindowHandle,
    draft: Option<Draft>,
    config: SelectionConfig,
    agent_running: bool,
    open: Option<Open>,
    panel: Option<Panel>,
    /// The popover content's size, measured by the panel after layout.
    content_size: Option<Size<Pixels>>,
    /// The editor pane in window coordinates, measured by [`Self::wrap`].
    pane: Rc<Cell<Option<Bounds<Pixels>>>>,
    pending_show: Option<Task<()>>,
    reconcile_scheduled: bool,
    /// The panel is being opened; it is set once it has.
    opening: bool,
    main_bounds: Option<Bounds<Pixels>>,
    _subscriptions: Vec<Subscription>,
}

/// The popover is shown for `range` of `text`.
struct Open {
    range: Range<usize>,
    text: String,
    mode: Mode,
    /// The one highlighted item; the mouse and the arrow keys both move it.
    highlighted: Option<usize>,
}

enum Mode {
    Actions,
    Instruction(Entity<Editor>),
    /// Dropping it cancels the call and kills pi.
    Running {
        _cancel: CancelOnDrop,
        _task: Task<()>,
    },
    Results(Vec<String>),
    Error(SharedString),
}

struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

struct Panel {
    handle: WindowHandle<PopoverPanel>,
    bounds: Bounds<Pixels>,
}

const ACTIONS: [(&str, Option<&str>); 4] = [
    ("Alternatives", Some("✦")),
    ("Shorter", None),
    ("Tighten", None),
    ("Instruction…", None),
];

impl SelectionPopover {
    pub fn new(editor: Entity<Editor>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            cx.subscribe(&editor, |this, _, event, cx| this.editor_event(event, cx)),
            cx.observe(&editor, |this, _, cx| this.schedule_reconcile(cx)),
            // gpui can't move the panel along with the window. Activation
            // changes report bounds too, unchanged.
            cx.observe_window_bounds(window, move |this, window, cx| {
                let bounds = window.bounds();
                if this.main_bounds.replace(bounds) != Some(bounds) {
                    this.dismiss(cx);
                }
            }),
            cx.observe_window_activation(window, |this, window, cx| {
                if !window.is_window_active() {
                    this.dismiss_unless_panel_active(cx);
                }
            }),
        ];
        Self {
            editor,
            main: window.window_handle(),
            draft: None,
            config: SelectionConfig::default(),
            agent_running: false,
            open: None,
            panel: None,
            content_size: None,
            pane: Rc::new(Cell::new(None)),
            pending_show: None,
            reconcile_scheduled: false,
            opening: false,
            main_bounds: Some(window.bounds()),
            _subscriptions: subscriptions,
        }
    }

    pub fn set_draft(&mut self, draft: Option<Draft>, cx: &mut Context<Self>) {
        self.draft = draft;
        self.dismiss(cx);
    }

    /// Selection actions are unavailable while the agent runs.
    pub fn set_agent_running(&mut self, running: bool, cx: &mut Context<Self>) {
        self.agent_running = running;
        if running {
            self.dismiss(cx);
        }
    }

    /// Wraps the editor pane: adds the popover's key contexts and actions,
    /// and measures the pane for placing the popover.
    pub fn wrap(this: &Entity<Self>, editor: impl IntoElement, cx: &App) -> AnyElement {
        let popover = this.read(cx);
        let pane = popover.pane.clone();
        let context = match &popover.open {
            Some(Open {
                mode: Mode::Results(_),
                ..
            }) => Some(format!("{POPOVER_CONTEXT} {RESULTS_CONTEXT}")),
            Some(_) => Some(POPOVER_CONTEXT.to_string()),
            None => None,
        };
        let entity = this.clone();
        div()
            .size_full()
            .relative()
            .map(|div| match &context {
                Some(context) => div.key_context(context.as_str()),
                None => div,
            })
            .on_action({
                let this = this.clone();
                move |_: &Dismiss, _, cx| this.update(cx, |this, cx| this.dismiss(cx))
            })
            .on_action({
                let this = this.clone();
                move |_: &Next, _, cx| this.update(cx, |this, cx| this.move_highlight(1, cx))
            })
            .on_action({
                let this = this.clone();
                move |_: &Previous, _, cx| this.update(cx, |this, cx| this.move_highlight(-1, cx))
            })
            .on_action({
                let this = this.clone();
                move |_: &Confirm, _, cx| this.update(cx, |this, cx| this.confirm(cx))
            })
            .on_action({
                let this = this.clone();
                move |pick: &Pick, _, cx| this.update(cx, |this, cx| this.apply(pick.0, cx))
            })
            .child(editor)
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, cx| {
                        pane.set(Some(bounds));
                        if entity.read(cx).reconcile_scheduled {
                            let this = entity.clone();
                            window.defer(cx, move |window, cx| {
                                this.update(cx, |this, cx| this.reconcile(window, cx))
                            });
                        }
                    },
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .into_any_element()
    }

    /// Whether a draft is focused that selection calls can fork.
    fn available(&self, cx: &App) -> bool {
        !self.agent_running
            && !self.editor.read(cx).is_read_only()
            && self
                .draft
                .as_ref()
                .is_some_and(|draft| draft.meta.pi_session_file.is_some())
    }

    fn editor_event(&mut self, event: &EditorEvent, cx: &mut Context<Self>) {
        match event {
            EditorEvent::Edited => self.dismiss(cx),
            EditorEvent::SelectionChanged(range) => {
                if self.open.as_ref().is_some_and(|open| open.range != *range) {
                    self.dismiss(cx);
                }
                self.pending_show = None;
                if !range.is_empty() && self.open.is_none() {
                    self.schedule_show(range.clone(), cx);
                }
            }
        }
    }

    /// Shows the actions once the selection has settled and the mouse
    /// button is up.
    fn schedule_show(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        self.pending_show = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(SHOW_DELAY).await;
                let Ok(selecting) = this.update(cx, |this, cx| this.editor.read(cx).is_selecting())
                else {
                    return;
                };
                if !selecting {
                    break;
                }
            }
            this.update(cx, |this, cx| {
                this.pending_show = None;
                let editor = this.editor.read(cx);
                if this.open.is_some() || editor.selected_range() != range || !this.available(cx) {
                    return;
                }
                this.open = Some(Open {
                    range,
                    text: editor.text().to_string(),
                    mode: Mode::Actions,
                    highlighted: None,
                });
                this.changed(cx);
            })
            .ok();
        }));
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        self.pending_show = None;
        if self.open.take().is_some() {
            self.changed(cx);
        }
    }

    /// The app lost focus: hide the popover, unless the focus went to the
    /// popover's own panel (the instruction field, or a click on it).
    fn dismiss_unless_panel_active(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = self.panel.as_ref().map(|panel| panel.handle) else {
            return self.dismiss(cx);
        };
        // The panel becomes key after the main window resigns.
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
            let panel_active = panel
                .update(cx, |_, window, _| window.is_window_active())
                .unwrap_or(false);
            this.update(cx, |this, cx| {
                let main_active = this
                    .main
                    .update(cx, |_, window, _| window.is_window_active())
                    .unwrap_or(false);
                if !panel_active && !main_active {
                    this.dismiss(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// The state changed: redraw and place the panel to match.
    fn changed(&mut self, cx: &mut Context<Self>) {
        self.content_size = None;
        cx.notify();
        if let Some(panel) = &self.panel {
            panel
                .handle
                .update(cx, |_, window, _| window.refresh())
                .ok();
        }
        self.schedule_reconcile(cx);
    }

    fn set_mode(&mut self, mode: Mode, highlighted: Option<usize>, cx: &mut Context<Self>) {
        if let Some(open) = &mut self.open {
            open.mode = mode;
            open.highlighted = highlighted;
            self.changed(cx);
        }
    }

    fn item_count(&self) -> usize {
        match self.open.as_ref().map(|open| &open.mode) {
            Some(Mode::Actions) => ACTIONS.len(),
            Some(Mode::Results(options)) => options.len(),
            _ => 0,
        }
    }

    fn set_highlight(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(open) = &mut self.open
            && open.highlighted != Some(index)
        {
            open.highlighted = Some(index);
            self.redraw(cx);
        }
    }

    fn move_highlight(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.item_count() as isize;
        if let Some(open) = &mut self.open
            && count > 0
        {
            let current = open
                .highlighted
                .map_or(if delta > 0 { -1 } else { 0 }, |i| i as isize);
            open.highlighted = Some((current + delta).rem_euclid(count) as usize);
            self.redraw(cx);
        }
    }

    /// Repaints the panel without changing its size.
    fn redraw(&mut self, cx: &mut Context<Self>) {
        cx.notify();
        if let Some(panel) = &self.panel {
            panel
                .handle
                .update(cx, |_, window, _| window.refresh())
                .ok();
        }
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        let Some(open) = &self.open else { return };
        match &open.mode {
            Mode::Results(_) => {
                if let Some(index) = open.highlighted {
                    self.apply(index, cx);
                }
            }
            Mode::Instruction(input) => {
                let instruction = input.read(cx).text().trim().to_string();
                if !instruction.is_empty() {
                    self.focus_main(cx);
                    self.run(SelectionAction::Instruction(instruction), cx);
                }
            }
            _ => {}
        }
    }

    fn pick_action(&mut self, index: usize, cx: &mut Context<Self>) {
        match index {
            0 => self.run(SelectionAction::Alternatives, cx),
            1 => self.run(SelectionAction::Shorter, cx),
            2 => self.run(SelectionAction::Tighten, cx),
            _ => self.start_instruction(cx),
        }
    }

    fn start_instruction(&mut self, cx: &mut Context<Self>) {
        let input = theme::editor(input_style, cx);
        self.set_mode(Mode::Instruction(input), None, cx);
        self.focus_panel(cx);
    }

    /// Makes the panel the key window with the instruction field focused,
    /// once it has been placed.
    fn focus_panel(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            this.update(cx, |this, cx| {
                let (
                    Some(panel),
                    Some(Open {
                        mode: Mode::Instruction(input),
                        ..
                    }),
                ) = (&this.panel, &this.open)
                else {
                    return;
                };
                let focus = input.focus_handle(cx);
                panel
                    .handle
                    .update(cx, |_, window, cx| {
                        window.activate_window();
                        window.focus(&focus, cx);
                    })
                    .ok();
            })
            .ok();
        })
        .detach();
    }

    fn focus_main(&self, cx: &mut Context<Self>) {
        let main = self.main;
        cx.defer(move |cx| {
            main.update(cx, |_, window, _| window.activate_window())
                .ok();
        });
    }

    fn run(&mut self, action: SelectionAction, cx: &mut Context<Self>) {
        let Some(open) = &self.open else { return };
        let Some(meta) = self.draft.as_ref().map(|draft| &draft.meta) else {
            return;
        };
        let Some(request) = Request::new(meta, open.text.clone(), open.range.clone(), action)
        else {
            return;
        };
        let pi = match ShellEnv::global(cx) {
            None => Err("Reading the login shell environment…".to_string()),
            Some(env) => env
                .pi()
                .map(|pi| env.command(pi))
                .map_err(|err| err.to_string()),
        };
        let extension = resources::pi_extension().map_err(|err| format!("{err:#}"));
        let (pi, extension) = match (pi, extension) {
            (Ok(pi), Ok(extension)) => (pi, extension),
            (Err(err), _) | (_, Err(err)) => {
                return self.set_mode(Mode::Error(err.into()), None, cx);
            }
        };

        let cancel = Arc::new(AtomicBool::new(false));
        let config = self.config.clone();
        let call = cx.background_executor().spawn({
            let cancel = cancel.clone();
            async move {
                selection::run(
                    pi,
                    &extension,
                    &request,
                    &config,
                    &std::env::temp_dir(),
                    &cancel,
                )
            }
        });
        let task = cx.spawn(async move |this, cx| {
            let result = call.await;
            this.update(cx, |this, cx| match result {
                Ok(options) => this.set_mode(Mode::Results(options), Some(0), cx),
                Err(err) => this.set_mode(Mode::Error(err.into()), None, cx),
            })
            .ok();
        });
        self.set_mode(
            Mode::Running {
                _cancel: CancelOnDrop(cancel),
                _task: task,
            },
            None,
            cx,
        );
    }

    /// Replaces the selection with option `index` as one undo step, if the
    /// text is still what the options were made for.
    fn apply(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(open) = self.open.take() else { return };
        let Mode::Results(options) = &open.mode else {
            self.open = Some(open);
            return;
        };
        let Some(option) = options.get(index) else {
            self.open = Some(open);
            return;
        };
        self.changed(cx);
        self.focus_main(cx);
        self.editor.update(cx, |editor, cx| {
            if editor.text() == open.text && editor.selected_range() == open.range {
                editor.replace_range(open.range.clone(), option, cx);
            }
        });
    }

    fn schedule_reconcile(&mut self, cx: &mut Context<Self>) {
        if self.reconcile_scheduled
            || (self.open.is_none() && self.panel.is_none() && !self.opening)
        {
            return;
        }
        self.reconcile_scheduled = true;
        // The editor pane's wrapper reconciles after the main window's next
        // draw, when the editor's layout and the selection's bounds are
        // current (see `wrap`).
        let main = self.main;
        cx.defer(move |cx| {
            main.update(cx, |_, window, _| window.refresh()).ok();
        });
    }

    /// Opens, resizes, reopens or closes the panel to show the current state
    /// above the selection.
    fn reconcile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reconcile_scheduled = false;
        if self.opening {
            return;
        }
        let target = self.placement(window, cx);
        match (&mut self.panel, target) {
            (None, None) => {}
            (Some(panel), Some(bounds)) if panel.bounds == bounds => {}
            (Some(panel), Some(bounds)) if panel.bounds.bottom_left() == bounds.bottom_left() => {
                panel.bounds = bounds;
                panel
                    .handle
                    .update(cx, |_, window, _| window.resize(bounds.size))
                    .ok();
            }
            (_, target) => {
                if let Some(panel) = self.panel.take() {
                    panel
                        .handle
                        .update(cx, |_, window, _| window.remove_window())
                        .ok();
                }
                if let Some(bounds) = target {
                    self.open_panel(bounds, window, cx);
                }
            }
        }
    }

    /// Where the panel goes, in screen coordinates: above the selection's
    /// first visible row, or below its last when there's no room above.
    /// `None` when the popover is hidden or the selection is scrolled away.
    fn placement(&self, window: &Window, cx: &App) -> Option<Bounds<Pixels>> {
        self.open.as_ref()?;
        let pane = self.pane.get()?;
        let rows: Vec<_> = self
            .editor
            .read(cx)
            .selection_bounds()
            .into_iter()
            .filter(|row| row.bottom() > pane.top() && row.top() < pane.bottom())
            .collect();
        let (first, last) = (rows.first()?, rows.last()?);
        let size = self.content_size.unwrap_or_else(|| self.estimated_size());

        let above = first.top() - GAP - size.height;
        let y = if above >= pane.top() + MARGIN {
            above
        } else {
            last.bottom() + GAP
        };
        let max_x = (pane.right() - size.width - MARGIN).max(pane.left() + MARGIN);
        let x = (first.left() - px(6.)).clamp(pane.left() + MARGIN, max_x);
        let origin = window.bounds().origin + point(x, y);
        Some(Bounds::new(origin.map(|v| v.round()), size))
    }

    /// The size of the first frame, before the content has been measured.
    fn estimated_size(&self) -> Size<Pixels> {
        match self.open.as_ref().map(|open| &open.mode) {
            Some(Mode::Results(_)) => size(RESULTS_WIDTH, px(120.)),
            Some(Mode::Instruction(_)) => size(INSTRUCTION_WIDTH + PADDING * 2., px(64.)),
            _ => size(px(330.), px(36.)),
        }
    }

    fn open_panel(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: None,
            focus: false,
            show: true,
            kind: WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            display_id: window.display(cx).map(|display| display.id()),
            window_background: WindowBackgroundAppearance::Blurred,
            ..Default::default()
        };
        self.opening = true;
        let popover = cx.entity();
        // The panel renders this entity as it opens, so it opens after the
        // current update.
        cx.defer(move |cx| {
            let opened = cx.open_window(options, |_, cx| {
                cx.new(|_| PopoverPanel {
                    popover: popover.clone(),
                })
            });
            popover.update(cx, |this, cx| {
                this.opening = false;
                match opened {
                    Ok(handle) => {
                        this.panel = Some(Panel { handle, bounds });
                        this.focus_panel(cx);
                        // The state may have changed while it opened.
                        this.schedule_reconcile(cx);
                    }
                    Err(err) => {
                        eprintln!("draftmux: couldn't open the selection popover: {err:#}");
                        this.open = None;
                    }
                }
            });
        });
    }

    /// Called by the panel after layout with the content's natural size.
    fn measured(&mut self, measured: Size<Pixels>, cx: &mut Context<Self>) {
        let measured = measured.map(|v| v.ceil());
        if self.content_size != Some(measured) {
            self.content_size = Some(measured);
            self.schedule_reconcile(cx);
        }
    }

    fn content(&self, this: &Entity<Self>, cx: &App) -> AnyElement {
        let Some(open) = &self.open else {
            return div().into_any_element();
        };
        let palette = *theme::palette(cx);
        let item = |index: usize| {
            let highlighted = open.highlighted == Some(index);
            div()
                .id(("item", index))
                .rounded(px(5.))
                .when(highlighted, |item| {
                    item.bg(palette.accent).text_color(palette.on_accent)
                })
                .on_mouse_move({
                    let this = this.clone();
                    move |_, _, cx| this.update(cx, |this, cx| this.set_highlight(index, cx))
                })
        };
        match &open.mode {
            Mode::Actions => div()
                .flex()
                .gap(px(2.))
                .children(
                    ACTIONS
                        .iter()
                        .enumerate()
                        .flat_map(|(index, (label, icon))| {
                            let separator = (index == ACTIONS.len() - 1).then(|| {
                                div()
                                    .w(px(1.))
                                    .my(px(4.))
                                    .mx(px(2.))
                                    .bg(palette.menu_line)
                                    .into_any_element()
                            });
                            let button =
                                item(index)
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .px(px(9.))
                                    .py(px(5.))
                                    .whitespace_nowrap()
                                    .children(icon.map(|icon| {
                                        div().text_size(px(11.)).child(icon.to_string())
                                    }))
                                    .child(*label)
                                    .on_click({
                                        let this = this.clone();
                                        move |_, _, cx| {
                                            this.update(cx, |this, cx| this.pick_action(index, cx))
                                        }
                                    })
                                    .into_any_element();
                            separator.into_iter().chain([button])
                        }),
                )
                .into_any_element(),
            Mode::Instruction(input) => {
                let empty = input.read(cx).text().is_empty();
                div()
                    .w(INSTRUCTION_WIDTH)
                    .child(
                        div()
                            .relative()
                            .h(px(30.))
                            .child(input.clone())
                            .when(empty, |field| {
                                field.child(
                                    div()
                                        .absolute()
                                        .top(px(6.))
                                        .left(px(8.))
                                        .text_color(palette.faint)
                                        .child(INSTRUCTION_PLACEHOLDER),
                                )
                            }),
                    )
                    .child(footer(&["↵ run", "esc cancel"], &palette))
                    .into_any_element()
            }
            Mode::Running { .. } => div()
                .px(px(10.))
                .py(px(8.))
                .text_color(palette.muted)
                .whitespace_nowrap()
                .child("Generating alternatives…")
                .into_any_element(),
            Mode::Results(options) => {
                let keys = format!("1–{} or ↵ apply", options.len());
                div()
                    .w(RESULTS_WIDTH - PADDING * 2.)
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .children(options.iter().enumerate().map(|(index, option)| {
                        let highlighted = open.highlighted == Some(index);
                        item(index)
                            .flex()
                            .gap(px(8.))
                            .px(px(8.))
                            .py(px(7.))
                            .line_height(px(19.))
                            .child(
                                div()
                                    .w(px(10.))
                                    .flex_none()
                                    .text_size(px(11.))
                                    .text_color(if highlighted {
                                        palette.on_accent_muted
                                    } else {
                                        palette.faint
                                    })
                                    .child((index + 1).to_string()),
                            )
                            .child(div().flex_1().min_w_0().child(option.trim().to_string()))
                            .on_click({
                                let this = this.clone();
                                move |_, _, cx| this.update(cx, |this, cx| this.apply(index, cx))
                            })
                    }))
                    .child(footer(&[keys.as_str(), "↑↓ move", "esc cancel"], &palette))
                    .into_any_element()
            }
            Mode::Error(message) => div()
                .max_w(RESULTS_WIDTH - PADDING * 2.)
                .px(px(10.))
                .py(px(8.))
                .text_color(palette.muted)
                .child(message.clone())
                .into_any_element(),
        }
    }
}

fn footer(hints: &[&str], palette: &Palette) -> impl IntoElement {
    div()
        .flex()
        .gap(px(10.))
        .mt(px(4.))
        .px(px(8.))
        .pt(px(6.))
        .pb(px(3.))
        .border_t_1()
        .border_color(palette.menu_line)
        .text_size(px(11.))
        .text_color(palette.faint)
        .whitespace_nowrap()
        .children(hints.iter().map(|hint| hint.to_string()))
}

/// The root view of the popup panel; renders [`SelectionPopover`]'s state.
struct PopoverPanel {
    popover: Entity<SelectionPopover>,
}

impl Render for PopoverPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let popover = self.popover.clone();
        let content = popover.read(cx).content(&popover, cx);
        let palette = theme::palette(cx);
        let (background, text) = (palette.menu_tint, palette.text);
        div()
            .size_full()
            .bg(background)
            .text_color(text)
            .text_size(px(13.))
            .font_family(".SystemUIFont")
            .key_context(INSTRUCTION_CONTEXT)
            .on_action({
                let popover = popover.clone();
                move |_: &Dismiss, _, cx| {
                    popover.update(cx, |this, cx| {
                        this.focus_main(cx);
                        this.dismiss(cx);
                    })
                }
            })
            .on_action({
                let popover = popover.clone();
                move |_: &Confirm, _, cx| popover.update(cx, |this, cx| this.confirm(cx))
            })
            // A click makes the panel key; give the keyboard back to the
            // editor unless the instruction field needs it.
            .on_mouse_up(gpui::MouseButton::Left, {
                let popover = popover.clone();
                move |_, _, cx| {
                    popover.update(cx, |this, cx| {
                        if !matches!(
                            this.open.as_ref().map(|open| &open.mode),
                            Some(Mode::Instruction(_))
                        ) {
                            this.focus_main(cx);
                        }
                    })
                }
            })
            .child(
                // Laid out at its natural size, then measured to size the
                // panel.
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .p(PADDING)
                    .child(content)
                    .child(
                        canvas(
                            move |bounds, _, cx| {
                                popover.update(cx, |this, cx| this.measured(bounds.size, cx))
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    ),
            )
    }
}

/// The instruction field: the UI font with small padding.
fn input_style(palette: &Palette) -> EditorStyle {
    EditorStyle {
        font: gpui::font(".SystemUIFont"),
        font_size: px(13.),
        line_height: px(18.),
        padding_x: px(8.),
        padding_top: px(6.),
        padding_bottom: px(6.),
        ..palette.editor_style()
    }
}
