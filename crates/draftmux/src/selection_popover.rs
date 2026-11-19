//! The selection actions popover: actions for the editor selection, then the
//! options the draft's agent returns, in a blurred popup window above the selection. A selection made in the preview selects its
//! markdown in the editor, and the popover then sits above the preview's
//! selection instead.
//!
//! Comment swaps the items for a field: what's written there goes to the
//! chat's agent with the selections quoted, as if sent from the chat, and
//! the popover closes. With several selections (see
//! [`Editor::extra_selections`]) the popover shows their count and only
//! Comment: the selection actions are prompts written for one selection, so
//! they don't apply to several.
//!
//! The popover is a separate panel window. On Wayland, where windows can't
//! be placed in screen coordinates, it's a popup anchored to the main window
//! at its edge facing the selection. gpui can't anchor popups on macOS or
//! X11, so there it's a `WindowKind::PopUp` panel placed in screen
//! coordinates. It holds no state of its own: [`SelectionPopover`] owns the
//! state, and the panel is resized, reopened or closed to match it after
//! each frame of the main window. gpui can't move a window, but a resize
//! keeps a `PopUp`'s bottom edge and an anchored popup's anchored edge, so
//! the popover grows in place while that edge stays put.
//!
//! The main window keeps keyboard focus, so the editor keeps its selection
//! highlight; the popover's keys are bound in key contexts around the editor
//! (see [`SelectionPopover::wrap`]). A click makes the panel key only until
//! the mouse button is released. The comment field is the exception: the
//! panel stays key while it's shown, and the main window keeps the editor
//! focused behind it.
//!
//! The panel floats above other apps' windows, so it closes while the app
//! is inactive and opens again once the main window is active. Losing focus
//! never dismisses the popover itself: its actions, running call and options
//! stay until the user picks one or dismisses it.
//!
//! Continuing the sentence at the cursor runs the same agent call about the
//! empty selection there. `ContinueOptions` shows its options in the
//! popover, placed at the caret. `ContinueSentence` streams one
//! continuation into the editor's ghost (see [`Editor::set_ghost`]), which
//! the user accepts or dismisses in the editor; the call ends with the
//! ghost, which goes when focus leaves the editor but stays while the app is
//! inactive. Starting either ends the other.

use std::cell::Cell;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use draftmux_storage::{Backend, Draft, Store, Target};
use editor::{Editor, EditorEvent, EditorStyle};
use gpui::{
    Action, Animation, AnimationExt, AnyElement, AnyWindowHandle, App, Bounds, Context, Div,
    ElementId, Entity, EventEmitter, FocusHandle, Focusable, Hsla, KeyBinding, Pixels,
    SharedString, Size, Stateful, Subscription, Task, Window, WindowBounds, WindowHandle,
    WindowKind, WindowOptions, actions, canvas, div, ease_out_quint, font, point,
    popup::{
        PopupAnchor, PopupConstraintAdjustment, PopupGravity, PopupNotSupportedError, PopupOptions,
    },
    prelude::*,
    px,
};
use shell_env::ShellEnv;

use crate::app_settings::AppSettings;
use crate::assets::{COMMENT_ICON, SEND_ICON};
use crate::markup_view::{self, MarkupStyle};
use crate::resources;
use crate::selectable::TextSelection;
use crate::selection::{
    self, Action as SelectionAction, Agent, Backend as SelectionBackend, Request, SelectionConfig,
    Warm,
};
use crate::theme::{self, Palette};

actions!(
    selection_popover,
    [
        Dismiss,
        Confirm,
        Next,
        Previous,
        Back,
        ContinueSentence,
        ContinueOptions
    ]
);

/// Applies the option with this index.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = selection_popover, no_json)]
pub struct Pick(pub usize);

/// Key contexts around the editor while the popover is shown.
const POPOVER_CONTEXT: &str = "SelectionPopover";
const RESULTS_CONTEXT: &str = "SelectionResults";
/// Around the comment field, in the panel.
const COMMENT_CONTEXT: &str = "SelectionComment";
/// Around the draft's editor, always: the continue keys are bound there and
/// not in other editors.
const DRAFT_EDITOR_CONTEXT: &str = "DraftEditor";

/// How long the selection must stay put before the popover appears, so
/// shift-arrow selections don't flash it on every step.
const SHOW_DELAY: Duration = Duration::from_millis(200);
/// Between the selection and the popover.
const GAP: Pixels = px(6.);
/// Between the popover and the edges of the editor pane.
const MARGIN: Pixels = px(8.);
const PADDING: Pixels = px(5.);
/// Highlighted items follow the panel's rounded corners: concentric with
/// them, inset by the padding.
const ITEM_RADIUS: Pixels = px(crate::platform::POPUP_RADIUS - 5.);
const RESULTS_WIDTH: Pixels = px(380.);
const COMMENT_WIDTH: Pixels = px(340.);
/// How long a newly highlighted item takes to fade in.
const HIGHLIGHT_FADE: Duration = Duration::from_millis(120);
/// How long the running, results and error states take to fade in.
const MODE_FADE: Duration = Duration::from_millis(160);

pub fn init(cx: &mut App) {
    let mut bindings = Vec::new();
    // The editor, or the preview when the selection was made there.
    for focused in ["Editor", crate::selectable::KEY_CONTEXT] {
        let popover = Some(format!("{POPOVER_CONTEXT} > {focused}"));
        let results = Some(format!("{RESULTS_CONTEXT} > {focused}"));
        bindings.extend([
            KeyBinding::new("escape", Dismiss, popover.as_deref()),
            KeyBinding::new("up", Previous, popover.as_deref()),
            KeyBinding::new("down", Next, popover.as_deref()),
            KeyBinding::new("enter", Confirm, popover.as_deref()),
            KeyBinding::new("up", Previous, results.as_deref()),
            KeyBinding::new("down", Next, results.as_deref()),
            KeyBinding::new("enter", Confirm, results.as_deref()),
        ]);
        for (index, key) in ["1", "2", "3", "4", "5", "6", "7", "8", "9"]
            .into_iter()
            .enumerate()
        {
            bindings.push(KeyBinding::new(key, Pick(index), results.as_deref()));
        }
    }
    // Enter sends the comment; shift-enter still inserts a newline.
    let comment = Some(format!("{COMMENT_CONTEXT} > Editor"));
    bindings.extend([
        KeyBinding::new("enter", Confirm, comment.as_deref()),
        KeyBinding::new("escape", Back, comment.as_deref()),
    ]);
    let draft_editor = Some(format!("{DRAFT_EDITOR_CONTEXT} > Editor"));
    bindings.extend([
        KeyBinding::new(
            editor::secondary!("enter"),
            ContinueSentence,
            draft_editor.as_deref(),
        ),
        KeyBinding::new(
            editor::secondary!("shift-enter"),
            ContinueOptions,
            draft_editor.as_deref(),
        ),
    ]);
    // Bound after the editor's keys, so these win at the same depth.
    cx.bind_keys(bindings);
}

pub enum SelectionPopoverEvent {
    /// The text the user wrote in the comment field about the editor's
    /// selections: send it to the chat's agent with them quoted.
    Comment(String),
    /// The user replaced the selection with an option from the draft's
    /// agent: the draft read `before`, now reads `text`, and the option
    /// came from the action named `prompt`.
    OptionApplied {
        before: String,
        text: String,
        prompt: SharedString,
    },
    /// Continuing the sentence into the ghost failed, for this reason.
    ContinueFailed(String),
}

impl EventEmitter<SelectionPopoverEvent> for SelectionPopover {}

/// The popover for one editor's selection.
pub struct SelectionPopover {
    editor: Entity<Editor>,
    main: AnyWindowHandle,
    draft: Option<Draft>,
    open: Option<Open>,
    panel: Option<Panel>,
    /// The popover content's size, measured by the panel after layout.
    content_size: Option<Size<Pixels>>,
    /// The state that is fading in and when it started.
    faded: Option<(&'static str, Instant)>,
    /// Options that arrived while the panel is open, shown once the panel
    /// has their size.
    staged: Option<Staged>,
    /// The editor pane in window coordinates, measured by [`Self::wrap`].
    pane: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The preview's selection, which the editor's mirrors while it has one.
    preview: Entity<TextSelection>,
    /// The preview pane in window coordinates, measured by
    /// [`Self::wrap_preview`].
    preview_pane: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// Remembers whether the user has added a selection, after which the
    /// hint on how to is hidden.
    store: Store,
    show_add_hint: bool,
    pending_show: Option<Task<()>>,
    /// Follows the comment field's text while it's shown.
    comment_observer: Option<Subscription>,
    reconcile_scheduled: bool,
    /// The panel is being opened; it is set once it has.
    opening: bool,
    /// The app lost focus: the panel, which would float above other apps,
    /// stays closed until the main window is active again. The popover keeps
    /// its state meanwhile, so a running call goes on and its options wait.
    hidden: bool,
    main_bounds: Option<Bounds<Pixels>>,
    /// The name of the action that was run last, which the edit history
    /// shows for an option applied from it.
    action_name: SharedString,
    /// The call streaming into the editor's ghost.
    ghost: Option<GhostCall>,
    /// An agent started when the popover opened, for the next call to take.
    warm: Arc<Mutex<Option<Warm>>>,
    _subscriptions: Vec<Subscription>,
}

/// A continuation streaming into the editor's ghost, which shows what of it
/// the user hasn't accepted yet. Dropping it cancels the call.
struct GhostCall {
    /// The continuation's start the user has accepted.
    accepted: String,
    /// The agent has finished.
    done: bool,
    cancel: CancelOnDrop,
    _task: Task<()>,
}

impl GhostCall {
    fn new(cancel: CancelOnDrop, task: Task<()>) -> Self {
        Self {
            accepted: String::new(),
            done: false,
            cancel,
            _task: task,
        }
    }
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
    Actions(Vec<draftmux_settings::SelectionAction>),
    /// This many ranges are selected; the only item is Comment.
    Several(usize),
    /// A message to the draft's agent about the selections, being written.
    Comment(Entity<Editor>),
    /// Dropping it cancels the call and kills the agent.
    Running {
        /// What is being done, shown beside the spinner.
        label: SharedString,
        /// Complete replacements that are not yet safe to apply.
        previews: Vec<String>,
        _cancel: CancelOnDrop,
        _task: Task<()>,
    },
    Results(Vec<String>),
    Error(SharedString),
}

impl Mode {
    /// The index of the Comment item among the items.
    fn comment_item(&self) -> Option<usize> {
        match self {
            Self::Actions(actions) => Some(actions.len() + 1),
            Self::Several(_) => Some(0),
            _ => None,
        }
    }

    /// The options that can be applied: the results, or the complete
    /// previews of a call still running.
    fn options(&self) -> Option<&[String]> {
        match self {
            Self::Results(options) => Some(options),
            Self::Running { previews, .. } if !previews.is_empty() => Some(previews),
            _ => None,
        }
    }

    fn is_request(&self, request: &Arc<AtomicBool>) -> bool {
        matches!(self, Self::Running { _cancel, .. } if Arc::ptr_eq(&_cancel.0, request))
    }
}

struct CancelOnDrop(Arc<AtomicBool>);

/// Options for the running request, held back until the panel is their
/// size: drawn at once they would show clipped at the old size for a frame,
/// since the panel resizes only after they are measured.
struct Staged {
    options: Vec<String>,
    /// The request finished and these are its results.
    done: bool,
    /// Their size, measured by a hidden copy in the panel.
    size: Option<Size<Pixels>>,
}

/// How long staged options wait for the panel to take their size before
/// they show anyway.
const STAGE_TIMEOUT: Duration = Duration::from_millis(250);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

struct Panel {
    handle: WindowHandle<PopoverPanel>,
    bounds: Bounds<Pixels>,
    /// The edge a resize keeps in place.
    pinned: Edge,
    /// The edge that faces the selection.
    side: Edge,
    /// The window, where the platform resizes it in place of gpui.
    native: Option<Rc<crate::platform::NativeWindow>>,
    /// Hides the popover when the panel, key after a click on it, loses
    /// focus to another app.
    _activation: Subscription,
}

/// A horizontal edge of the panel.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Edge {
    Top,
    Bottom,
}

impl Edge {
    /// The left corner of `bounds` on this edge.
    fn corner(self, bounds: Bounds<Pixels>) -> gpui::Point<Pixels> {
        match self {
            Edge::Top => bounds.origin,
            Edge::Bottom => bounds.bottom_left(),
        }
    }
}

impl SelectionPopover {
    pub fn new(
        editor: Entity<Editor>,
        preview: Entity<TextSelection>,
        store: Store,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor_focus = editor.read(cx).focus_handle(cx);
        let preview_focus = preview.read(cx).focus_handle(cx);
        // Focus moving out of both panes, to the chat's composer for one,
        // dismisses the popover; the selection itself stays.
        let focus_out = |handle: &FocusHandle, window: &mut Window, cx: &mut Context<Self>| {
            let panes = [editor_focus.clone(), preview_focus.clone()];
            cx.on_focus_out(handle, window, move |this, _, window, cx| {
                if !panes.iter().any(|pane| pane.contains_focused(window, cx)) {
                    this.dismiss(cx);
                }
            })
        };
        let subscriptions = vec![
            focus_out(&editor_focus, window, cx),
            focus_out(&preview_focus, window, cx),
            // Leaving the app keeps the editor focused: the ghost stays.
            cx.on_focus_out(&editor_focus, window, {
                let editor_focus = editor_focus.clone();
                move |this, _, window, cx| {
                    if !editor_focus.contains_focused(window, cx) {
                        this.end_ghost(cx);
                    }
                }
            }),
            cx.subscribe(&editor, |this, _, event, cx| this.editor_event(event, cx)),
            cx.observe(&editor, |this, _, cx| this.schedule_reconcile(cx)),
            cx.observe(&AppSettings::global(cx), |this, settings, cx| {
                let actions = settings.read(cx).settings.selection_actions();
                if let Some(Open {
                    mode: Mode::Actions(current),
                    highlighted,
                    ..
                }) = &mut this.open
                    && *current != actions
                {
                    *current = actions;
                    *highlighted = None;
                    this.changed(cx);
                }
            }),
            // gpui can't move the panel along with the window. Activation
            // changes report bounds too, unchanged.
            cx.observe_window_bounds(window, move |this, window, cx| {
                let bounds = window.bounds();
                if this.main_bounds.replace(bounds) != Some(bounds) {
                    this.dismiss(cx);
                }
            }),
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.unhide(cx);
                } else {
                    this.hide_unless_app_active(cx);
                }
            }),
        ];
        Self {
            editor,
            main: window.window_handle(),
            draft: None,
            open: None,
            panel: None,
            content_size: None,
            staged: None,
            faded: None,
            pane: Rc::new(Cell::new(None)),
            preview,
            preview_pane: Rc::new(Cell::new(None)),
            show_add_hint: !store.selection_added(),
            store,
            pending_show: None,
            comment_observer: None,
            reconcile_scheduled: false,
            opening: false,
            hidden: false,
            main_bounds: Some(window.bounds()),
            action_name: SharedString::default(),
            ghost: None,
            warm: Arc::default(),
            _subscriptions: subscriptions,
        }
    }

    pub fn set_draft(&mut self, draft: Option<Draft>, cx: &mut Context<Self>) {
        self.draft = draft;
        self.dismiss(cx);
        self.end_ghost(cx);
        self.drop_warm(cx);
    }

    /// Starts the draft's agent in the background, if it isn't already, so
    /// the call the user picks next doesn't wait for it to load the session.
    fn prestart(&mut self, cx: &mut Context<Self>) {
        let (backend, cwd, config) = match self.backend(cx) {
            Ok(call) => call,
            Err(_) => return,
        };
        let slot = self.warm.clone();
        cx.background_executor()
            .spawn(async move {
                if let Err(err) =
                    selection::prestart(&slot, backend, &cwd, &config, &std::env::temp_dir())
                {
                    eprintln!("draftmux: starting the selection agent ahead: {err}");
                }
            })
            .detach();
    }

    /// Ends the started agent, which would wait for a call that won't come.
    fn drop_warm(&mut self, cx: &mut Context<Self>) {
        let warm = self
            .warm
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(warm) = warm {
            // Waiting for it to exit blocks.
            cx.background_executor()
                .spawn(async move { drop(warm) })
                .detach();
        }
    }

    /// Takes the current draft's newer metadata, such as its agent's model,
    /// for later calls, leaving the popover as it is.
    pub fn refresh_draft(&mut self, draft: Option<Draft>) {
        if self.draft.as_ref().map(|d| &d.id) == draft.as_ref().map(|d| &d.id) {
            self.draft = draft;
        }
    }

    /// Wraps the editor pane: adds the popover's key contexts and actions,
    /// and measures the pane for placing the popover.
    pub fn wrap(this: &Entity<Self>, editor: impl IntoElement, cx: &App) -> AnyElement {
        let pane = this.read(cx).pane.clone();
        Self::wrap_pane(this, editor, pane, false, cx)
    }

    /// Wraps the preview pane like [`Self::wrap`] wraps the editor's. The
    /// preview scrolls without the editor knowing, so while the popover is
    /// over the preview it is placed again after every frame.
    pub fn wrap_preview(this: &Entity<Self>, preview: impl IntoElement, cx: &App) -> AnyElement {
        let pane = this.read(cx).preview_pane.clone();
        Self::wrap_pane(this, preview, pane, true, cx)
    }

    fn wrap_pane(
        this: &Entity<Self>,
        child: impl IntoElement,
        pane: Rc<Cell<Option<Bounds<Pixels>>>>,
        preview: bool,
        cx: &App,
    ) -> AnyElement {
        let popover = this.read(cx);
        let mut context = match &popover.open {
            Some(open) if open.mode.options().is_some() => {
                Some(format!("{POPOVER_CONTEXT} {RESULTS_CONTEXT}"))
            }
            Some(_) => Some(POPOVER_CONTEXT.to_string()),
            None => None,
        };
        if !preview {
            context = Some(match context {
                Some(context) => format!("{DRAFT_EDITOR_CONTEXT} {context}"),
                None => DRAFT_EDITOR_CONTEXT.to_string(),
            });
        }
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
            .on_action({
                let this = this.clone();
                move |_: &ContinueSentence, _, cx| {
                    this.update(cx, |this, cx| this.continue_sentence(cx))
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &ContinueOptions, _, cx| {
                    this.update(cx, |this, cx| this.continue_options(cx))
                }
            })
            .child(child)
            // Before the panel opens, a hidden copy of its content in the
            // pane measures it, so the panel opens at its final size.
            .when(
                popover.open.is_some()
                    && popover.panel.is_none()
                    && popover.content_size.is_none()
                    && popover.in_preview(cx) == preview,
                |pane| {
                    // Style the measured element itself: an auto-sized wrapper
                    // with only an absolute child has no intrinsic width.
                    let mut copy = measured_content(this.clone(), popover.content(this, cx))
                        .text_size(px(13.))
                        .font_family(editor::platform::ui_font());
                    copy.style().visibility = Some(gpui::Visibility::Hidden);
                    pane.child(copy)
                },
            )
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, cx| {
                        pane.set(Some(bounds));
                        let popover = entity.read(cx);
                        let follow = preview && popover.open.is_some() && popover.in_preview(cx);
                        if popover.reconcile_scheduled || follow {
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

    /// Whether the selection was made in the preview, so the popover goes
    /// there. Continuing at the cursor is always in the editor.
    fn in_preview(&self, cx: &App) -> bool {
        self.open.as_ref().is_none_or(|open| !open.range.is_empty())
            && self.preview.read(cx).range().is_some()
    }

    /// Whether a draft is focused that selection calls can fork: any
    /// Claude draft, and a pi draft with a session.
    fn available(&self, cx: &App) -> bool {
        !self.editor.read(cx).is_read_only()
            && self
                .draft
                .as_ref()
                .is_some_and(|draft| match &draft.meta.backend {
                    Backend::Pi(pi) => pi.session_file.is_some(),
                    Backend::Claude(_) => true,
                })
    }

    /// The name of the agent the draft runs on, which comments go to.
    fn agent(&self) -> &'static str {
        self.draft
            .as_ref()
            .map_or(Agent::Pi, |draft| Agent::of(&draft.meta.backend))
            .name()
    }

    fn editor_event(&mut self, event: &EditorEvent, cx: &mut Context<Self>) {
        match event {
            EditorEvent::GhostAccepted(text) => self.ghost_accepted(text, cx),
            EditorEvent::GhostDismissed => self.ghost = None,
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
            // Adding or dropping a selection changes what the popover offers.
            EditorEvent::ExtraSelectionsChanged => {
                if self.show_add_hint && !self.editor.read(cx).extra_selections().is_empty() {
                    self.show_add_hint = false;
                    if let Err(err) = self.store.mark_selection_added() {
                        eprintln!("draftmux: remembering the added selection: {err:#}");
                    }
                }
                self.dismiss(cx);
                let range = self.editor.read(cx).selected_range();
                if !range.is_empty() {
                    self.schedule_show(range, cx);
                }
            }
        }
    }

    /// Shows the actions once the selection has settled, the mouse button
    /// is up and the editor's context menu is closed.
    fn schedule_show(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        self.pending_show = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(SHOW_DELAY).await;
                let Ok(selecting) = this.update(cx, |this, cx| {
                    let editor = this.editor.read(cx);
                    editor.is_selecting()
                        || editor.is_context_menu_open()
                        || this.preview.read(cx).is_dragging()
                }) else {
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
                let mode = this.items_mode(cx);
                this.open = Some(Open {
                    range,
                    text: editor.text().to_string(),
                    mode,
                    highlighted: None,
                });
                this.changed(cx);
                this.prestart(cx);
            })
            .ok();
        }));
    }

    /// The items for the editor's selections: the actions for one, Comment
    /// for several.
    fn items_mode(&self, cx: &App) -> Mode {
        match self.editor.read(cx).selected_ranges().len() {
            1 => Mode::Actions(
                AppSettings::global(cx)
                    .read(cx)
                    .settings
                    .selection_actions(),
            ),
            count => Mode::Several(count),
        }
    }

    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        self.pending_show = None;
        self.comment_observer = None;
        self.hidden = false;
        if self.open.take().is_some() {
            // Closed now rather than after the main window's next draw: a
            // window covered by another app may not draw until it's shown
            // again, which would leave the empty panel on screen meanwhile.
            self.close_panel(cx);
            self.changed(cx);
        }
    }

    fn close_panel(&mut self, cx: &mut Context<Self>) {
        if let Some(panel) = self.panel.take() {
            // Deferred, since the panel's own events close it, and a window
            // can't be updated while it handles one.
            cx.defer(move |cx| {
                panel
                    .handle
                    .update(cx, |_, window, _| window.remove_window())
                    .ok();
            });
        }
    }

    /// A window lost focus: hide the popover if the focus left the app.
    /// Clicking the panel moves focus from the main window to the panel and
    /// back without the app losing it; checking either window's focus while
    /// that settles would catch both inactive and hide the popover under
    /// the click.
    ///
    /// Only the panel closes. The popover stays open, with its options or
    /// running call, until the user picks an option or dismisses it.
    fn hide_unless_app_active(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
            this.update(cx, |this, cx| {
                if !crate::platform::app_active(cx) {
                    this.hide(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Closes the panel and keeps it closed until [`Self::unhide`].
    fn hide(&mut self, cx: &mut Context<Self>) {
        if self.open.is_some() && !self.hidden {
            self.hidden = true;
            // Closed now: a main window covered by another app may not draw
            // to reconcile until it's shown again.
            self.close_panel(cx);
        }
    }

    /// The main window is active again: show the hidden popover.
    fn unhide(&mut self, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.hidden) {
            self.schedule_reconcile(cx);
        }
    }

    /// The state changed: redraw and place the panel to match.
    fn changed(&mut self, cx: &mut Context<Self>) {
        self.content_size = None;
        self.redraw(cx);
        self.schedule_reconcile(cx);
    }

    fn set_mode(&mut self, mode: Mode, highlighted: Option<usize>, cx: &mut Context<Self>) {
        self.comment_observer = None;
        if let Some(open) = &mut self.open {
            let key = fade_key(&mode);
            if key != fade_key(&open.mode) {
                self.faded = key.map(|key| (key, Instant::now()));
            }
            open.mode = mode;
            open.highlighted = highlighted;
            self.changed(cx);
        }
    }

    fn item_count(&self) -> usize {
        match self.open.as_ref().map(|open| &open.mode) {
            Some(Mode::Actions(actions)) => actions.len() + 2,
            Some(Mode::Several(_)) => 1,
            Some(mode) => mode.options().map_or(0, <[String]>::len),
            None => 0,
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
            Mode::Results(_) | Mode::Running { .. } => {
                if let Some(index) = open.highlighted {
                    self.apply(index, cx);
                }
            }
            Mode::Actions(_) => {
                if let Some(index) = open.highlighted {
                    self.pick_action(index, cx);
                }
            }
            Mode::Several(_) if open.highlighted.is_some() => self.start_comment(cx),
            Mode::Comment(_) => self.send_comment(cx),
            _ => {}
        }
    }

    fn pick_action(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(Open {
            mode: Mode::Actions(actions),
            ..
        }) = &self.open
        else {
            return;
        };
        if index == actions.len() + 1 {
            self.start_comment(cx);
        } else if index == 0 {
            self.action_name = "Alternatives".into();
            self.run(
                SelectionAction::Alternatives,
                "Generating alternatives…".into(),
                cx,
            );
        } else if let Some(action) = actions.get(index - 1) {
            let label = format!("{}…", action.name).into();
            self.action_name = action.name.clone().into();
            self.run(
                SelectionAction::Instruction(action.prompt.clone()),
                label,
                cx,
            );
        }
    }

    /// Swaps the items for the comment field and gives it the keyboard.
    fn start_comment(&mut self, cx: &mut Context<Self>) {
        let field = theme::editor(comment_style, cx);
        // Typing grows the field, and the panel with it.
        let observer = cx.subscribe(&field, |this, _, event, cx| {
            if matches!(event, EditorEvent::Edited) {
                this.changed(cx);
            }
        });
        self.set_mode(Mode::Comment(field), None, cx);
        self.comment_observer = Some(observer);
        self.focus_comment(cx);
    }

    fn commenting(&self) -> bool {
        matches!(
            self.open,
            Some(Open {
                mode: Mode::Comment(_),
                ..
            })
        )
    }

    /// Makes the panel key with the comment field focused, if it's shown.
    /// The main window keeps the editor focused, so the selection stays
    /// painted.
    fn focus_comment(&self, cx: &mut Context<Self>) {
        let (
            Some(panel),
            Some(Open {
                mode: Mode::Comment(field),
                ..
            }),
        ) = (&self.panel, &self.open)
        else {
            return;
        };
        let (handle, focus) = (panel.handle, field.focus_handle(cx));
        cx.defer(move |cx| {
            handle
                .update(cx, |_, window, cx| {
                    window.activate_window();
                    window.focus(&focus, cx);
                })
                .ok();
        });
    }

    /// Leaves the comment field for the items it replaced, with Comment
    /// highlighted.
    fn back(&mut self, cx: &mut Context<Self>) {
        if !self.commenting() {
            return;
        }
        let mode = self.items_mode(cx);
        let highlighted = mode.comment_item();
        self.set_mode(mode, highlighted, cx);
        self.focus_main(cx);
    }

    /// Hands the comment to the chat and closes the popover.
    fn send_comment(&mut self, cx: &mut Context<Self>) {
        let Some(Open {
            mode: Mode::Comment(field),
            ..
        }) = &self.open
        else {
            return;
        };
        let text = field.read(cx).text().trim().to_owned();
        if text.is_empty() {
            return;
        }
        self.dismiss(cx);
        self.focus_main(cx);
        cx.emit(SelectionPopoverEvent::Comment(text));
    }

    fn focus_main(&self, cx: &mut Context<Self>) {
        let main = self.main;
        cx.defer(move |cx| {
            main.update(cx, |_, window, _| window.activate_window())
                .ok();
        });
    }

    fn run(&mut self, action: SelectionAction, label: SharedString, cx: &mut Context<Self>) {
        let Some(open) = &self.open else { return };
        let (text, range) = (open.text.clone(), open.range.clone());
        let call = self.call(
            text,
            range,
            action,
            |this, request, options, cx| this.options_arrived(request, options, false, cx),
            |this, request, result, cx| {
                if !this
                    .open
                    .as_ref()
                    .is_some_and(|open| open.mode.is_request(request))
                {
                    return;
                }
                match result {
                    Ok(options) => this.options_arrived(request, options, true, cx),
                    Err(err) => this.set_mode(Mode::Error(err.into()), None, cx),
                }
            },
            cx,
        );
        match call {
            Ok((cancel, task)) => self.set_mode(
                Mode::Running {
                    label,
                    previews: Vec::new(),
                    _cancel: cancel,
                    _task: task,
                },
                None,
                cx,
            ),
            Err(err) => self.set_mode(Mode::Error(err.into()), None, cx),
        }
    }

    /// The format of the current draft, which its options are shown in.
    fn target(&self) -> Target {
        self.draft
            .as_ref()
            .map_or(Target::Github, |draft| draft.meta.target)
    }

    /// The agent a call on the current draft runs on, the draft's working
    /// directory and the call's settings.
    fn backend(&self, cx: &App) -> Result<(SelectionBackend, PathBuf, SelectionConfig), String> {
        let draft = self.draft.as_ref().ok_or("No draft is open.")?;
        let meta = &draft.meta;
        // The count is an app preference, read on every call; the rest is
        // what the draft kept at creation.
        let alternatives = AppSettings::global(cx).read(cx).settings.alternatives();
        let config = SelectionConfig::for_draft(meta, alternatives);
        let env = ShellEnv::global(cx).ok_or("Reading the login shell environment…")?;
        let backend = match &meta.backend {
            Backend::Pi(pi) => SelectionBackend::Pi {
                pi: env.command(env.pi().map_err(|err| err.to_string())?),
                extension: resources::pi_extension().map_err(|err| format!("{err:#}"))?,
                session_file: pi
                    .session_file
                    .clone()
                    .ok_or("This draft has no pi session yet.")?,
                agent_dir: pi.agent_dir.clone(),
            },
            Backend::Claude(claude) => SelectionBackend::Claude {
                program: env.claude().map_err(|err| err.to_string())?.to_path_buf(),
                env: env.vars().to_vec(),
                session: crate::chat::draft_session(draft, claude),
            },
        };
        Ok((backend, meta.cwd.clone(), config))
    }

    /// Starts the draft's agent on `action` about `range` of `text`. Its
    /// previews go to `update` and its result to `done`, each with the
    /// call's cancel flag, which identifies it. Dropping the returned guard
    /// cancels it.
    #[allow(clippy::type_complexity)]
    fn call(
        &mut self,
        text: String,
        range: Range<usize>,
        action: SelectionAction,
        update: fn(&mut Self, &Arc<AtomicBool>, Vec<String>, &mut Context<Self>),
        done: fn(&mut Self, &Arc<AtomicBool>, Result<Vec<String>, String>, &mut Context<Self>),
        cx: &mut Context<Self>,
    ) -> Result<(CancelOnDrop, Task<()>), String> {
        let (backend, _, config) = self.backend(cx)?;
        let meta = &self.draft.as_ref().ok_or("No draft is open.")?.meta;
        let request = Request::new(meta, text, range, action);
        let warm = self
            .warm
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();

        let cancel = Arc::new(AtomicBool::new(false));
        let (send, updates) = async_channel::unbounded();
        let call = cx.background_executor().spawn({
            let cancel = cancel.clone();
            async move {
                selection::run(
                    backend,
                    &request,
                    &config,
                    &std::env::temp_dir(),
                    warm,
                    &cancel,
                    |options| {
                        let _ = send.try_send(options);
                    },
                )
            }
        });
        let request = cancel.clone();
        let task = cx.spawn(async move |this, cx| {
            while let Ok(options) = updates.recv().await {
                if this
                    .update(cx, |this, cx| update(this, &request, options, cx))
                    .is_err()
                {
                    return;
                }
            }
            let result = call.await;
            this.update(cx, |this, cx| done(this, &request, result, cx))
                .ok();
        });
        Ok((CancelOnDrop(cancel), task))
    }

    /// Collapses the editor's selections to the end of the selection, where
    /// a continuation goes, and returns the caret; `None` when the draft
    /// can't be continued now.
    fn continue_at(&mut self, cx: &mut Context<Self>) -> Option<usize> {
        if self.draft.is_none() || self.editor.read(cx).is_read_only() {
            return None;
        }
        Some(self.editor.update(cx, |editor, cx| {
            let end = editor.selected_range().end;
            editor.set_extra_selections(Vec::new(), cx);
            editor.select_range(end..end, cx);
            end
        }))
    }

    /// Shows options for continuing the sentence at the cursor.
    fn continue_options(&mut self, cx: &mut Context<Self>) {
        self.end_ghost(cx);
        self.dismiss(cx);
        let Some(at) = self.continue_at(cx) else {
            return;
        };
        // After the collapse's events, which would dismiss the popover.
        let this = cx.entity();
        cx.defer(move |cx| {
            this.update(cx, |this, cx| {
                let editor = this.editor.read(cx);
                if editor.selected_range() != (at..at) {
                    return;
                }
                this.open = Some(Open {
                    range: at..at,
                    text: editor.text().to_string(),
                    mode: Mode::Error(SharedString::default()),
                    highlighted: None,
                });
                this.action_name = "Continue".into();
                this.run(
                    SelectionAction::Continue { text: false },
                    "Continuing…".into(),
                    cx,
                );
            })
        });
    }

    /// Streams a continuation of the sentence at the cursor into the
    /// editor's ghost; a ghost already showing is accepted first.
    fn continue_sentence(&mut self, cx: &mut Context<Self>) {
        self.ghost = None;
        self.editor.update(cx, |editor, cx| editor.accept_ghost(cx));
        self.dismiss(cx);
        let Some(at) = self.continue_at(cx) else {
            return;
        };
        let text = self.editor.read(cx).text().to_string();
        let call = self.call(
            text,
            at..at,
            SelectionAction::Continue { text: true },
            |this, request, options, cx| {
                let text = options.into_iter().next().unwrap_or_default();
                this.ghost_arrived(request, &text, false, cx);
            },
            |this, request, result, cx| match result {
                Ok(options) => {
                    let text = options.into_iter().next().unwrap_or_default();
                    this.ghost_arrived(request, &text, true, cx);
                }
                Err(err) => this.ghost_failed(request, err, cx),
            },
            cx,
        );
        match call {
            Ok((cancel, task)) => {
                self.editor
                    .update(cx, |editor, cx| editor.set_ghost(at, "", cx));
                self.ghost = Some(GhostCall::new(cancel, task));
            }
            Err(err) => cx.emit(SelectionPopoverEvent::ContinueFailed(err)),
        }
    }

    fn is_ghost(&self, request: &Arc<AtomicBool>) -> bool {
        self.ghost
            .as_ref()
            .is_some_and(|ghost| Arc::ptr_eq(&ghost.cancel.0, request))
    }

    /// The ghost's call has the continuation as far as `text`, all of it
    /// when `done`: the ghost shows what the user hasn't accepted of it. A
    /// retry starting over shows the waiting ghost; text that no longer
    /// starts with what was accepted ends it.
    fn ghost_arrived(
        &mut self,
        request: &Arc<AtomicBool>,
        text: &str,
        done: bool,
        cx: &mut Context<Self>,
    ) {
        if !self.is_ghost(request) {
            return;
        }
        let Some(ghost) = &mut self.ghost else { return };
        ghost.done = done;
        let rest = match text.strip_prefix(ghost.accepted.as_str()) {
            Some(rest) => rest,
            None if ghost.accepted.starts_with(text) && !done => "",
            None => return self.end_ghost(cx),
        };
        if done && rest.is_empty() {
            return self.end_ghost(cx);
        }
        self.editor.update(cx, |editor, cx| {
            if let Some(at) = editor.ghost().map(|ghost| ghost.offset) {
                editor.set_ghost(at, rest, cx);
            }
        });
    }

    /// The ghost's call failed: the ghost goes, and `Chrome` says why.
    fn ghost_failed(&mut self, request: &Arc<AtomicBool>, err: String, cx: &mut Context<Self>) {
        if self.is_ghost(request) {
            self.end_ghost(cx);
            cx.emit(SelectionPopoverEvent::ContinueFailed(err));
        }
    }

    /// The user accepted `text` from the ghost; once all of a finished
    /// continuation is in, or the ghost is gone, its call ends.
    fn ghost_accepted(&mut self, text: &str, cx: &mut Context<Self>) {
        let Some(ghost) = &mut self.ghost else { return };
        ghost.accepted.push_str(text);
        let empty = self
            .editor
            .read(cx)
            .ghost()
            .map(|ghost| ghost.text.is_empty());
        if empty.is_none() || (ghost.done && empty == Some(true)) {
            self.end_ghost(cx);
        }
    }

    /// Ends the ghost's call and removes the ghost.
    fn end_ghost(&mut self, cx: &mut Context<Self>) {
        if self.ghost.take().is_some() {
            self.editor.update(cx, |editor, cx| editor.clear_ghost(cx));
        }
    }

    /// Replaces the selection with option `index` as one undo step, if the
    /// text is still what the options were made for. A preview applies as
    /// well: it is complete, and taking it ends the call, so nothing the
    /// agent sends later reaches the text.
    fn apply(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(open) = self.open.take() else { return };
        let Some(option) = open.mode.options().and_then(|options| options.get(index)) else {
            self.open = Some(open);
            return;
        };
        self.staged = None;
        self.changed(cx);
        self.focus_main(cx);
        let text = self.editor.update(cx, |editor, cx| {
            if editor.text() != open.text || editor.selected_range() != open.range {
                return None;
            }
            editor.replace_range(open.range.clone(), option, cx);
            let inserted = open.range.start..editor.selected_range().end;
            editor.flash(vec![inserted], cx);
            Some(editor.text().to_string())
        });
        if let Some(text) = text {
            cx.emit(SelectionPopoverEvent::OptionApplied {
                before: open.text,
                text,
                prompt: self.action_name.clone(),
            });
        }
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
        // Wait for the content's size rather than show a guess and correct
        // it, which reopens the panel wherever it moves.
        if self.opening || (self.open.is_some() && self.content_size.is_none()) {
            return;
        }
        let target = self.placement(window, cx);
        match (&mut self.panel, target) {
            (None, None) => {}
            (Some(panel), Some((bounds, _))) if panel.bounds == bounds => {}
            // Where the platform moves the panel, it goes anywhere in
            // place: reopening it would show both panels for a moment,
            // since gpui closes a window later.
            (Some(panel), Some((bounds, side))) if panel.native.is_some() => {
                let from = std::mem::replace(&mut panel.bounds, bounds);
                panel.side = side;
                panel.pinned = side;
                let native = panel.native.clone().expect("checked above");
                let handle = panel.handle;
                // Drawing the panel at its new size needs the app, so the
                // move waits for this update to end.
                cx.spawn(async move |this, cx| {
                    let current = this
                        .read_with(cx, |this, _| {
                            this.panel
                                .as_ref()
                                .is_some_and(|panel| panel.handle == handle)
                        })
                        .unwrap_or(false);
                    if current {
                        crate::platform::move_popup(&native, from, bounds, side == Edge::Top);
                    }
                })
                .detach();
            }
            (Some(panel), Some((bounds, _)))
                if panel.pinned.corner(panel.bounds) == panel.pinned.corner(bounds) =>
            {
                panel.bounds = bounds;
                panel
                    .handle
                    .update(cx, |_, window, _| window.resize(bounds.size))
                    .ok();
            }
            (_, target) => {
                self.close_panel(cx);
                if let Some((bounds, side)) = target {
                    self.open_panel(bounds, side, window, cx);
                }
            }
        }
    }

    /// Where the panel goes, in screen coordinates: above the selection's
    /// first visible row, or below its last when there's no room above, with
    /// the edge that faces the selection. `None` when the popover is hidden
    /// or the selection is scrolled away.
    fn placement(&self, window: &Window, cx: &App) -> Option<(Bounds<Pixels>, Edge)> {
        if self.hidden {
            return None;
        }
        self.open.as_ref()?;
        let (pane, rows) = if self.in_preview(cx) {
            let rows = self.preview.read(cx).selection_rects().to_vec();
            (self.preview_pane.get()?, rows)
        } else if self.open.as_ref()?.range.is_empty() {
            let caret = self.editor.read(cx).caret_bounds();
            (self.pane.get()?, caret.into_iter().collect())
        } else {
            (self.pane.get()?, self.editor.read(cx).selection_bounds())
        };
        let rows: Vec<_> = rows
            .into_iter()
            .filter(|row| row.bottom() > pane.top() && row.top() < pane.bottom())
            .collect();
        let (first, last) = (rows.first()?, rows.last()?);
        let size = self.content_size?;

        // The options grow as they come in, so their side is chosen for the
        // height they're expected to reach, and once below the selection
        // they stay there rather than jump back.
        let options = self.options_height(cx);
        let side = self.panel.as_ref().map(|panel| panel.side);
        let needed = options.map_or(size.height, |options| options.max(size.height));
        // The comment field, already above, may grow past the pane's top
        // up to the window's, so typing a newline in it doesn't flip it.
        let top = if options.is_none() && side == Some(Edge::Bottom) {
            px(0.)
        } else {
            pane.top()
        };
        let stays_below = options.is_some() && side == Some(Edge::Top);
        let (y, side) = if !stays_below && first.top() - GAP - needed >= top + MARGIN {
            (first.top() - GAP - size.height, Edge::Bottom)
        } else {
            (last.bottom() + GAP, Edge::Top)
        };
        let max_x = (pane.right() - size.width - MARGIN).max(pane.left() + MARGIN);
        let x = (first.left() - px(6.)).clamp(pane.left() + MARGIN, max_x);
        let origin = window.bounds().origin + point(x, y);
        Some((Bounds::new(origin.map(|v| v.round()), size), side))
    }

    /// While options are coming in or shown, the height the list is
    /// expected to reach: each of the options the actions ask for about as
    /// long as the selection.
    fn options_height(&self, cx: &App) -> Option<Pixels> {
        let open = self.open.as_ref()?;
        if !matches!(open.mode, Mode::Running { .. } | Mode::Results(_)) {
            return None;
        }
        let count = AppSettings::global(cx)
            .read(cx)
            .settings
            .alternatives()
            .max(1);
        let chars = open
            .text
            .get(open.range.clone())
            .map_or(0, |text| text.chars().count());
        // A row's text is the list's width less the padding, the number
        // and the gaps, at about this many points per character.
        let width = RESULTS_WIDTH - PADDING * 2. - px(8.) * 3. - px(10.);
        let per_line = (width.as_f32() / 6.5).floor().max(1.) as usize;
        let lines = chars.div_ceil(per_line).clamp(1, 8);
        let row = px(7.) * 2. + px(19.) * lines as f32;
        let rows = row * count as f32 + px(2.) * (count - 1) as f32;
        // The footer and the panel's padding.
        Some(rows + px(32.) + PADDING * 2.)
    }

    /// Opens the panel at `bounds`, pinned at the `side` edge where the
    /// platform can anchor it there.
    fn open_panel(
        &mut self,
        bounds: Bounds<Pixels>,
        side: Edge,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let display_id = window.display(cx).map(|display| display.id());
        let options = move |kind| WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: None,
            focus: false,
            show: true,
            kind,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            display_id,
            window_background: crate::platform::popup_background(),
            ..Default::default()
        };
        // Growing away from the selection from the corner on the `side`
        // edge, so a resize keeps that edge in place.
        let anchored = WindowKind::AnchoredPopup(PopupOptions {
            parent: window.window_handle(),
            anchor_rect: Bounds::new(
                side.corner(bounds) - window.bounds().origin,
                gpui::size(px(1.), px(1.)),
            ),
            anchor: PopupAnchor::TopLeft,
            gravity: match side {
                Edge::Top => PopupGravity::BottomRight,
                Edge::Bottom => PopupGravity::TopRight,
            },
            constraint_adjustment: PopupConstraintAdjustment::SLIDE_X
                | PopupConstraintAdjustment::SLIDE_Y,
            offset: point(px(0.), px(0.)),
            grab: false,
        });
        self.opening = true;
        let popover = cx.entity();
        // The panel renders this entity as it opens, so it opens after the
        // current update.
        cx.defer(move |cx| {
            let mut activation = None;
            let mut native = None;
            let mut open = |kind, cx: &mut App| {
                cx.open_window(options(kind), |window, cx| {
                    // The comment field makes the panel key.
                    crate::platform::keep_inactive_appearance(window);
                    // The panel grows away from the selection.
                    native = crate::platform::prepare_popup(window, side == Edge::Top);
                    activation = Some(popover.update(cx, |_, cx| {
                        cx.observe_window_activation(window, |this, window, cx| {
                            if !window.is_window_active() {
                                this.hide_unless_app_active(cx);
                            }
                        })
                    }));
                    cx.new(|_| PopoverPanel {
                        popover: popover.clone(),
                    })
                })
            };
            let opened = match open(anchored, cx) {
                // gpui resizes a `PopUp` keeping its bottom edge; the
                // platform's resize keeps either.
                Err(err) if err.is::<PopupNotSupportedError>() => {
                    open(WindowKind::PopUp, cx).map(|handle| {
                        let pinned = if native.is_some() { side } else { Edge::Bottom };
                        (handle, pinned)
                    })
                }
                opened => opened.map(|handle| (handle, side)),
            };
            popover.update(cx, |this, cx| {
                this.opening = false;
                match opened {
                    Ok((handle, pinned)) => {
                        this.panel = Some(Panel {
                            handle,
                            bounds,
                            pinned,
                            side,
                            native: native.map(Rc::new),
                            _activation: activation.expect("set when the window opens"),
                        });
                        // Dismissed or hidden while it opened.
                        if this.open.is_none() || this.hidden {
                            return this.close_panel(cx);
                        }
                        // Reopened elsewhere while the comment field shows.
                        this.focus_comment(cx);
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

    /// Shows `options` from `request`, its results when it's `done`. While
    /// the panel is open they wait in [`Staged`] for it to take their size.
    fn options_arrived(
        &mut self,
        request: &Arc<AtomicBool>,
        options: Vec<String>,
        done: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(open) = &self.open else { return };
        if !open.mode.is_request(request) {
            return;
        }
        if self.panel.is_none() {
            self.staged = None;
            return self.show_options(options, done, cx);
        }
        self.staged = Some(Staged {
            options,
            done,
            size: None,
        });
        self.redraw(cx);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(STAGE_TIMEOUT).await;
            this.update(cx, |this, cx| {
                if this
                    .staged
                    .as_ref()
                    .is_some_and(|staged| staged.done == done)
                {
                    this.unstage(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Shows `options`: the results when `done`, else previews. A
    /// highlighted preview stays highlighted while it is still there.
    fn show_options(&mut self, options: Vec<String>, done: bool, cx: &mut Context<Self>) {
        let highlighted = self
            .open
            .as_ref()
            .and_then(|open| open.highlighted)
            .filter(|&index| index < options.len());
        if done {
            let highlighted = highlighted.or(Some(0));
            return self.set_mode(Mode::Results(options), highlighted, cx);
        }
        if let Some(Open {
            mode: Mode::Running { previews, .. },
            highlighted: shown,
            ..
        }) = &mut self.open
            && *previews != options
        {
            *previews = options;
            *shown = highlighted;
            self.changed(cx);
        }
    }

    /// Shows the staged options; the panel has their size, or stopped
    /// waiting for it.
    fn unstage(&mut self, cx: &mut Context<Self>) {
        let Some(staged) = self.staged.take() else {
            return;
        };
        let size = staged.size;
        self.show_options(staged.options, staged.done, cx);
        // The panel is already their size; skip measuring them again.
        if size.is_some() && self.content_size.is_none() {
            self.content_size = size;
        }
    }

    /// Called by the panel with the staged options' size. The panel takes
    /// it, and shows them in its first frame at that size.
    fn measured_staged(&mut self, measured: Size<Pixels>, cx: &mut Context<Self>) {
        let measured = measured.map(|v| v.ceil());
        let Some(staged) = &mut self.staged else {
            return;
        };
        if staged.size == Some(measured) {
            return;
        }
        staged.size = Some(measured);
        if self.content_size == Some(measured) {
            // Drawing them needs no resize.
            let this = cx.entity();
            cx.defer(move |cx| this.update(cx, |this, cx| this.unstage(cx)));
        } else {
            self.content_size = Some(measured);
            self.schedule_reconcile(cx);
        }
    }

    /// Called by the panel before it draws, at `size`: the staged options
    /// show once it is their size.
    fn panel_sized(&mut self, size: Size<Pixels>, cx: &mut Context<Self>) {
        let ready =
            self.staged.as_ref().and_then(|staged| staged.size) == Some(size.map(|v| v.ceil()));
        if ready {
            self.unstage(cx);
        }
    }

    /// The staged options laid out as they will show, for measuring.
    fn staged_content(&self, cx: &App) -> Option<AnyElement> {
        let staged = self.staged.as_ref()?;
        let Some(Open {
            mode: Mode::Running { label, .. },
            ..
        }) = &self.open
        else {
            return None;
        };
        Some(previews(label, &staged.options, self.target(), cx))
    }

    /// Called by the panel after layout with the content's natural size.
    fn measured(&mut self, measured: Size<Pixels>, cx: &mut Context<Self>) {
        // The staged options' size is the one the panel is taking.
        if self
            .staged
            .as_ref()
            .is_some_and(|staged| staged.size.is_some())
        {
            return;
        }
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
        let (target, dark) = (self.target(), theme::get(cx).dark);
        let item = |index: usize| {
            div()
                .id(("item", index))
                .active(|style| style.opacity(0.8))
                .rounded(ITEM_RADIUS)
                .on_mouse_move({
                    let this = this.clone();
                    move |_, _, cx| this.update(cx, |this, cx| this.set_highlight(index, cx))
                })
        };
        // The highlight fades in when it lands on an item and drops off when
        // it leaves, so sweeping across items doesn't leave a trail. The text
        // fades with it, since white text shows before the accent does.
        let highlight = |index: usize, item: Stateful<Div>| -> AnyElement {
            if open.highlighted != Some(index) {
                return item.into_any_element();
            }
            item.with_animation(
                ("item-highlight", index),
                Animation::new(HIGHLIGHT_FADE).with_easing(ease_out_quint()),
                move |item, delta| {
                    item.bg(palette.accent.opacity(delta)).text_color(mix(
                        palette.text,
                        palette.on_accent,
                        delta,
                    ))
                },
            )
            .into_any_element()
        };
        let comment_item = |index: usize| {
            let item = item(index)
                .flex_none()
                .flex()
                .items_center()
                .gap(px(6.))
                .px(px(9.))
                .py(px(5.))
                .whitespace_nowrap()
                .child(tinted(
                    gpui::svg().path(COMMENT_ICON).size(px(13.)),
                    ("comment-icon-highlight", index),
                    open.highlighted == Some(index),
                    (palette.text, palette.on_accent),
                ))
                .child("Comment")
                .on_click({
                    let this = this.clone();
                    move |_, _, cx| this.update(cx, |this, cx| this.start_comment(cx))
                });
            highlight(index, item)
        };
        let divider = || {
            div()
                .flex_none()
                .w(px(1.))
                .h(px(16.))
                .mx(px(3.))
                .bg(palette.menu_line)
        };
        let content = match &open.mode {
            Mode::Actions(actions) => div()
                .flex()
                .items_center()
                .gap(px(2.))
                .flex_nowrap()
                .children(
                    std::iter::once(("Alternatives", Some("✦")))
                        .chain(actions.iter().map(|action| (action.name.as_str(), None)))
                        .enumerate()
                        .map(|(index, (label, icon))| {
                            let item =
                                item(index)
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .px(px(9.))
                                    .py(px(5.))
                                    .whitespace_nowrap()
                                    .children(icon.map(|icon| {
                                        div().text_size(px(11.)).child(icon.to_string())
                                    }))
                                    .child(div().max_w(px(140.)).truncate().child(label.to_owned()))
                                    .on_click({
                                        let this = this.clone();
                                        move |_, _, cx| {
                                            this.update(cx, |this, cx| this.pick_action(index, cx))
                                        }
                                    });
                            highlight(index, item)
                        }),
                )
                .child(divider())
                .child(comment_item(actions.len() + 1))
                // Until the user has added a selection, nothing else shows
                // that they can.
                .when(self.show_add_hint, |actions| {
                    actions.child(
                        div()
                            .flex_none()
                            .pl(px(4.))
                            .pr(px(7.))
                            .text_size(px(11.))
                            .text_color(palette.faint)
                            .whitespace_nowrap()
                            .child(editor::by_platform!("⌘-drag to add", "Ctrl-drag to add")),
                    )
                })
                .into_any_element(),
            Mode::Several(count) => div()
                .flex()
                .items_center()
                .gap(px(2.))
                .flex_nowrap()
                .whitespace_nowrap()
                .child(
                    div()
                        .px(px(9.))
                        .py(px(5.))
                        .text_color(palette.muted)
                        .child(format!("{count} selections")),
                )
                .child(divider())
                .child(comment_item(0))
                .into_any_element(),
            Mode::Comment(field) => {
                let empty = field.read(cx).text().trim().is_empty();
                let agent = self.agent();
                let placeholder: SharedString = match self.editor.read(cx).selected_ranges().len() {
                    1 => format!("Tell {agent} what to change here…").into(),
                    n => format!("Tell {agent} what to do with these {n} selections…").into(),
                };
                let send = format!("{} send to {agent}", editor::by_platform!("↵", "Enter"));
                let focus = field.focus_handle(cx);
                div()
                    .w(COMMENT_WIDTH - PADDING * 2.)
                    .flex()
                    .flex_col()
                    .key_context(COMMENT_CONTEXT)
                    .on_action({
                        let this = this.clone();
                        move |_: &Confirm, _, cx| this.update(cx, |this, cx| this.send_comment(cx))
                    })
                    .on_action({
                        let this = this.clone();
                        move |_: &Back, _, cx| this.update(cx, |this, cx| this.back(cx))
                    })
                    .child(
                        div()
                            .flex()
                            .items_end()
                            .gap(px(6.))
                            .p(px(4.))
                            .pl(px(9.))
                            .rounded(ITEM_RADIUS)
                            .bg(palette.menu_field)
                            .border_1()
                            .border_color(palette.hairline)
                            // The field is as wide as its text; a press beside
                            // it still focuses it.
                            .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                                window.focus(&focus, cx)
                            })
                            .child(
                                div()
                                    .relative()
                                    .flex_1()
                                    .min_w_0()
                                    .child(field.clone())
                                    .when(empty, |input| {
                                        input.child(
                                            div()
                                                .absolute()
                                                .top(px(2.))
                                                .left_0()
                                                .text_color(palette.faint)
                                                .whitespace_nowrap()
                                                .child(placeholder),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .id("send-comment")
                                    .size(px(22.))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(6.))
                                    .bg(if empty {
                                        palette.send_disabled
                                    } else {
                                        palette.accent
                                    })
                                    .when(!empty, |button| {
                                        button.active(|style| style.opacity(0.8)).on_click({
                                            let this = this.clone();
                                            move |_, _, cx| {
                                                this.update(cx, |this, cx| this.send_comment(cx))
                                            }
                                        })
                                    })
                                    .child(
                                        gpui::svg()
                                            .path(SEND_ICON)
                                            .size(px(12.))
                                            .text_color(palette.on_accent),
                                    ),
                            ),
                    )
                    // The field's edge sets the hints apart, so they need no
                    // rule.
                    .child(
                        hints(
                            &[
                                &send,
                                editor::by_platform!("⇧↵ newline", "Shift+Enter newline"),
                                "esc back",
                            ],
                            &palette,
                        )
                        .mt(px(6.))
                        .pb(px(3.)),
                    )
                    .into_any_element()
            }
            // Previews take the rows the options will, and apply like them;
            // finishing brings in the keys and the full text color.
            Mode::Running {
                label,
                previews: options,
                ..
            } if !options.is_empty() => div()
                .w(RESULTS_WIDTH - PADDING * 2.)
                .flex()
                .flex_col()
                .gap(px(2.))
                .children(options.iter().enumerate().map(|(index, option)| {
                    let number = tinted(
                        option_number(index),
                        ("number-highlight", index),
                        open.highlighted == Some(index),
                        (palette.faint, palette.on_accent_muted),
                    );
                    let marks = if open.highlighted == Some(index) {
                        palette.on_accent
                    } else {
                        palette.muted
                    };
                    let text = option_text(target, option, index, marks, dark);
                    let item = option_row(item(index), number, text)
                        .text_color(palette.muted)
                        .on_click({
                            let this = this.clone();
                            move |_, _, cx| this.update(cx, |this, cx| this.apply(index, cx))
                        });
                    highlight(index, item)
                }))
                .child(running_footer(label, &palette))
                .into_any_element(),
            Mode::Running { label, .. } => div()
                .px(px(10.))
                .py(px(8.))
                .flex()
                .items_center()
                .gap(px(8.))
                .text_color(palette.muted)
                .whitespace_nowrap()
                .child(crate::chat::spinner(&palette).size(px(13.)))
                .child(label.clone())
                .into_any_element(),
            Mode::Results(options) => {
                let keys = format!(
                    "1–{} or {} apply",
                    options.len(),
                    editor::by_platform!("↵", "Enter")
                );
                div()
                    .w(RESULTS_WIDTH - PADDING * 2.)
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .children(options.iter().enumerate().map(|(index, option)| {
                        let number = tinted(
                            option_number(index),
                            ("number-highlight", index),
                            open.highlighted == Some(index),
                            (palette.faint, palette.on_accent_muted),
                        );
                        let marks = if open.highlighted == Some(index) {
                            palette.on_accent
                        } else {
                            palette.text
                        };
                        let text = option_text(target, option, index, marks, dark);
                        let item = option_row(item(index), number, text).on_click({
                            let this = this.clone();
                            move |_, _, cx| this.update(cx, |this, cx| this.apply(index, cx))
                        });
                        highlight(index, item)
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
        };
        match self.fade_in() {
            Some(opacity) => div().opacity(opacity).child(content).into_any_element(),
            None => content,
        }
    }

    /// How far the current state has faded in, while it is fading. The
    /// time is kept here rather than in the panel's element state, so a
    /// panel reopened elsewhere goes on from where the last one was.
    fn fade_in(&self) -> Option<f32> {
        let (_, since) = self.faded?;
        let delta = since.elapsed().as_secs_f32() / MODE_FADE.as_secs_f32();
        (delta < 1.).then(|| ease_out_quint()(delta))
    }
}

/// The states that fade in when they replace another, by what they show:
/// the actions show with the panel, and previews and then the options
/// replace the spinner in place without fading in again.
fn fade_key(mode: &Mode) -> Option<&'static str> {
    match mode {
        Mode::Actions(_) | Mode::Several(_) => None,
        Mode::Comment(_) => Some("comment"),
        Mode::Running { .. } | Mode::Results(_) => Some("options"),
        Mode::Error(_) => Some("error"),
    }
}

/// `options` that aren't final yet, laid out as the running state shows
/// them, with `label` and a spinner below, for measuring.
fn previews(label: &SharedString, options: &[String], target: Target, cx: &App) -> AnyElement {
    let palette = theme::palette(cx);
    let dark = theme::get(cx).dark;
    div()
        .w(RESULTS_WIDTH - PADDING * 2.)
        .flex()
        .flex_col()
        .gap(px(2.))
        .children(options.iter().enumerate().map(|(index, option)| {
            let number = option_number(index).text_color(palette.faint);
            let text = option_text(target, option, index, palette.muted, dark);
            option_row(div().id(("preview", index)), number, text).text_color(palette.muted)
        }))
        .child(running_footer(label, palette))
        .into_any_element()
}

/// Below previews: the spinner with what is running, and the keys.
fn running_footer(label: &SharedString, palette: &Palette) -> gpui::Div {
    rule(
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .px(px(8.))
            .text_size(px(11.))
            .text_color(palette.faint)
            .whitespace_nowrap()
            .child(crate::chat::spinner(palette))
            .child(label.clone())
            .child(div().ml(px(4.)).child("esc cancel")),
        palette,
    )
}

/// A row of the options, numbered.
fn option_row(row: Stateful<Div>, number: impl IntoElement, text: AnyElement) -> Stateful<Div> {
    row.flex()
        .gap(px(8.))
        .px(px(8.))
        .py(px(7.))
        .line_height(px(19.))
        .child(number)
        .child(div().flex_1().min_w_0().child(text))
}

/// An option formatted as the draft's target shows it, in the row's text
/// color: lines wrapped in the source flow as one paragraph, and bullets
/// and links take `marks`.
fn option_text(target: Target, option: &str, index: usize, marks: Hsla, dark: bool) -> AnyElement {
    let document = option_document(target, option);
    markup_view::render_in_parent_color(&document, &option_style(marks, dark), ("text", index))
}

/// An option parsed in the draft's format. Markdown joins the lines of a
/// paragraph, which an option keeps from a hard-wrapped selection.
fn option_document(target: Target, option: &str) -> markup::Document {
    let option = option.trim();
    match target {
        Target::Github | Target::GithubFile | Target::Rich => markup::github::parse_file(option),
        Target::Slack => markup::slack::parse(option),
        Target::Plain => markup::plain::parse(option),
    }
}

/// Markdown at the popover's text size and line height, its blocks spaced
/// for a row.
fn option_style(marks: Hsla, dark: bool) -> MarkupStyle {
    let mut style = MarkupStyle::github(dark);
    style.text_size = px(13.);
    style.line_height = 19. / 13.;
    style.text_color = marks;
    style.link_color = marks;
    style.link_underline = true;
    style.heading_scale = [1.; 6];
    style.heading_rule_max_level = 0;
    style.heading_gap = px(6.);
    style.block_gap = px(6.);
    style.text_gap = px(6.);
    style.list_indent = px(18.);
    style.bullet.left = px(6.);
    style.bullet.top = px(7.);
    style.code_padding = px(6.);
    style
}

fn option_number(index: usize) -> Div {
    div()
        .w(px(10.))
        .flex_none()
        .text_size(px(11.))
        .child((index + 1).to_string())
}

/// `element` in the first of `colors`, or fading to the second with an
/// item's highlight while it's `highlighted`.
fn tinted<E: Styled + IntoElement + 'static>(
    element: E,
    id: impl Into<ElementId>,
    highlighted: bool,
    (from, to): (Hsla, Hsla),
) -> AnyElement {
    if !highlighted {
        return element.text_color(from).into_any_element();
    }
    element
        .with_animation(
            id,
            Animation::new(HIGHLIGHT_FADE).with_easing(ease_out_quint()),
            move |element, delta| element.text_color(mix(from, to, delta)),
        )
        .into_any_element()
}

/// `from` moved `t` of the way to `to`, alpha included.
fn mix(from: Hsla, to: Hsla, t: f32) -> Hsla {
    let (a, b) = (from.to_rgb(), to.to_rgb());
    let lerp = |x: f32, y: f32| x + (y - x) * t;
    gpui::Rgba {
        r: lerp(a.r, b.r),
        g: lerp(a.g, b.g),
        b: lerp(a.b, b.b),
        a: lerp(a.a, b.a),
    }
    .into()
}

fn footer(keys: &[&str], palette: &Palette) -> gpui::Div {
    rule(hints(keys, palette), palette)
}

/// `row` under a rule, as the popover's last line.
fn rule(row: gpui::Div, palette: &Palette) -> gpui::Div {
    row.mt(px(4.))
        .pt(px(6.))
        .pb(px(3.))
        .border_t_1()
        .border_color(palette.menu_line)
}

fn hints(keys: &[&str], palette: &Palette) -> gpui::Div {
    div()
        .flex()
        .gap(px(10.))
        .px(px(8.))
        .text_size(px(11.))
        .text_color(palette.faint)
        .whitespace_nowrap()
        .children(keys.iter().map(|hint| hint.to_string()))
}

/// The comment field: the UI font at the popover's size, as tall as its
/// text.
fn comment_style(palette: &Palette) -> EditorStyle {
    EditorStyle {
        font: font(editor::platform::ui_font()),
        font_size: px(13.),
        line_height: px(19.),
        padding_x: px(0.),
        padding_top: px(2.),
        padding_bottom: px(1.),
        text: palette.text,
        heading: palette.text,
        fit_content: true,
        ..palette.editor_style()
    }
}

/// The root view of the popup panel; renders [`SelectionPopover`]'s state.
struct PopoverPanel {
    popover: Entity<SelectionPopover>,
}

impl Render for PopoverPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let popover = self.popover.clone();
        let size = window.viewport_size();
        popover.update(cx, |popover, cx| popover.panel_sized(size, cx));
        let content = popover.read(cx).content(&popover, cx);
        if popover.read(cx).fade_in().is_some() {
            window.request_animation_frame();
        }
        let staged = popover.read(cx).staged_content(cx).map(|staged| {
            let mut copy = measured(staged, {
                let popover = popover.clone();
                move |size, cx| popover.update(cx, |this, cx| this.measured_staged(size, cx))
            });
            copy.style().visibility = Some(gpui::Visibility::Hidden);
            copy
        });
        let palette = theme::palette(cx);
        let (background, text) = (crate::platform::menu_color(palette), palette.text);
        div()
            .size_full()
            .bg(background)
            .text_color(text)
            .text_size(px(13.))
            .font_family(editor::platform::ui_font())
            // A click makes the panel key; give the keyboard back to the
            // editor, unless the comment field has it.
            .on_mouse_up(gpui::MouseButton::Left, {
                let popover = popover.clone();
                move |_, _, cx| {
                    popover.update(cx, |this, cx| {
                        if !this.commenting() {
                            this.focus_main(cx);
                        }
                    })
                }
            })
            .child(measured_content(popover, content))
            .children(staged)
    }
}

/// `content` laid out at its natural size, then measured to size the
/// panel.
fn measured_content(popover: Entity<SelectionPopover>, content: AnyElement) -> gpui::Div {
    measured(content, move |size, cx| {
        popover.update(cx, |this, cx| this.measured(size, cx))
    })
}

/// `content` laid out at its natural size, which goes to `report`.
fn measured(content: AnyElement, report: impl Fn(Size<Pixels>, &mut App) + 'static) -> gpui::Div {
    div()
        .absolute()
        .top_0()
        .left_0()
        .p(PADDING)
        .child(content)
        .child(
            canvas(
                move |bounds, _, cx| report(bounds.size, cx),
                |_, _, _, _| {},
            )
            .absolute()
            .size_full(),
        )
}

#[cfg(test)]
mod tests {
    #[test]
    fn options_show_formatted_with_wrapped_lines_joined() {
        let document = super::option_document(
            draftmux_storage::Target::Github,
            "The editor keeps **its colors**\n  while the agent edits.\n",
        );
        let [markup::Block::Paragraph(spans)] = document.blocks.as_slice() else {
            panic!("expected one paragraph: {document:?}");
        };
        let text: String = spans.iter().map(|span| span.text.as_str()).collect();
        assert_eq!(text, "The editor keeps its colors while the agent edits.");
        assert!(
            spans
                .iter()
                .any(|span| span.style.strong && span.text == "its colors")
        );
    }

    use super::*;

    #[gpui::test]
    fn a_preview_applies_as_one_undo_and_ends_the_call(cx: &mut gpui::TestAppContext) {
        let config = tempfile::tempdir().unwrap();
        cx.update(|cx| crate::app_settings::init(config.path().to_owned(), cx));
        let window = cx.add_window(|_, _| gpui::EmptyView);
        let (editor, popover) = window
            .update(cx, |_, window, cx| {
                let editor = cx.new(|cx| Editor::new("Hi there\n", cx));
                let preview = cx.new(TextSelection::new);
                let store = Store::new(config.path().join("drafts"));
                let popover =
                    cx.new(|cx| SelectionPopover::new(editor.clone(), preview, store, window, cx));
                (editor, popover)
            })
            .unwrap();
        let applied = Rc::new(std::cell::RefCell::new(Vec::new()));
        cx.update(|cx| {
            let applied = applied.clone();
            cx.subscribe(&popover, move |_, event, _| {
                if let SelectionPopoverEvent::OptionApplied {
                    before,
                    text,
                    prompt,
                } = event
                {
                    applied
                        .borrow_mut()
                        .push((before.clone(), text.clone(), prompt.to_string()));
                }
            })
            .detach();
        });
        let call = Arc::new(AtomicBool::new(false));
        cx.update(|cx| {
            editor.update(cx, |editor, cx| editor.select_range(3..9, cx));
            popover.update(cx, |popover, cx| {
                popover.action_name = "Alternatives".into();
                popover.open = Some(Open {
                    text: "Hi there\n".into(),
                    range: 3..9,
                    highlighted: None,
                    mode: Mode::Running {
                        label: "Generating alternatives…".into(),
                        previews: Vec::new(),
                        _cancel: CancelOnDrop(call.clone()),
                        _task: Task::ready(()),
                    },
                });
                // Nothing to apply until a preview is complete.
                assert_eq!(popover.item_count(), 0);
                popover.move_highlight(1, cx);
                popover.confirm(cx);
                popover.apply(0, cx);
                assert_eq!(popover.editor.read(cx).text(), "Hi there\n");

                popover.options_arrived(&call, vec!["friend\n".into()], false, cx);
                assert_eq!(popover.item_count(), 1);
                popover.move_highlight(1, cx);
                popover.confirm(cx);
                assert_eq!(popover.editor.read(cx).text(), "Hi friend\n");
                assert!(call.load(Ordering::Relaxed), "applying ends the call");
                // What the call sends after that is ignored.
                popover.options_arrived(&call, vec!["pal\n".into()], true, cx);
                assert!(popover.open.is_none());
                assert_eq!(popover.editor.read(cx).text(), "Hi friend\n");
            });
            editor.update(cx, |editor, cx| {
                assert!(editor.undo(cx));
                assert_eq!(editor.text(), "Hi there\n");
                assert!(!editor.undo(cx));
            });
        });
        // Only the applied option is reported, for the edit history.
        assert_eq!(
            *applied.borrow(),
            [(
                "Hi there\n".to_owned(),
                "Hi friend\n".to_owned(),
                "Alternatives".to_owned()
            )]
        );
    }

    #[gpui::test]
    fn comment_sends_its_text_and_esc_goes_back_to_the_items(cx: &mut gpui::TestAppContext) {
        let config = tempfile::tempdir().unwrap();
        cx.update(|cx| crate::app_settings::init(config.path().to_owned(), cx));
        let window = cx.add_window(|_, _| gpui::EmptyView);
        let (editor, popover) = window
            .update(cx, |_, window, cx| {
                let editor = cx.new(|cx| Editor::new("Hi there\n", cx));
                let preview = cx.new(TextSelection::new);
                let store = Store::new(config.path().join("drafts"));
                let popover =
                    cx.new(|cx| SelectionPopover::new(editor.clone(), preview, store, window, cx));
                (editor, popover)
            })
            .unwrap();
        let sent = Rc::new(std::cell::RefCell::new(Vec::new()));
        cx.update(|cx| {
            let sent = sent.clone();
            cx.subscribe(&popover, move |_, event, _| {
                if let SelectionPopoverEvent::Comment(text) = event {
                    sent.borrow_mut().push(text.clone())
                }
            })
            .detach();
            editor.update(cx, |editor, cx| editor.select_range(3..8, cx));
            let field = cx.new(|cx| Editor::new("", cx));
            popover.update(cx, |popover, cx| {
                let Mode::Actions(actions) = popover.items_mode(cx) else {
                    panic!("one selection has the actions");
                };
                // After Alternatives and the actions.
                let comment = Some(actions.len() + 1);
                popover.open = Some(Open {
                    text: "Hi there\n".into(),
                    range: 3..8,
                    highlighted: None,
                    mode: Mode::Comment(field.clone()),
                });
                assert_eq!(popover.item_count(), 0);
                // Nothing written, nothing sent.
                popover.confirm(cx);
                assert!(popover.commenting());
                popover.back(cx);
                assert!(matches!(
                    popover.open,
                    Some(Open { mode: Mode::Actions(_), highlighted, .. }) if highlighted == comment
                ));
                popover.set_mode(Mode::Comment(field.clone()), None, cx);
            });
            field.update(cx, |field, cx| field.set_text("  Make it formal\n", cx));
            popover.update(cx, |popover, cx| popover.confirm(cx));
            assert!(popover.read(cx).open.is_none());
        });
        assert_eq!(*sent.borrow(), ["Make it formal"]);
    }

    #[gpui::test]
    fn newer_metadata_keeps_a_running_call_and_another_draft_ends_it(
        cx: &mut gpui::TestAppContext,
    ) {
        let config = tempfile::tempdir().unwrap();
        cx.update(|cx| crate::app_settings::init(config.path().to_owned(), cx));
        let window = cx.add_window(|_, _| gpui::EmptyView);
        let popover = window
            .update(cx, |_, window, cx| {
                let editor = cx.new(|cx| Editor::new("Hi there\n", cx));
                let preview = cx.new(TextSelection::new);
                let store = Store::new(config.path().join("drafts"));
                cx.new(|cx| SelectionPopover::new(editor, preview, store, window, cx))
            })
            .unwrap();
        let draft = |id: &str, model: Option<&str>| Draft {
            id: id.into(),
            dir: "/drafts/x".into(),
            meta: draftmux_storage::Meta {
                target: draftmux_storage::Target::Github,
                syntax: None,
                status: draftmux_storage::Status::Active,
                title: String::new(),
                source: None,
                cwd: "/repo".into(),
                backend: Backend::Claude(draftmux_storage::ClaudeSession {
                    session_id: "0196f1d2-7c1e-7a40-9b8e-3d1c2b4a5f60".parse().unwrap(),
                    transcript: None,
                    config_dir: None,
                    fork: None,
                    model: model.map(str::to_owned),
                    effort: None,
                    selection_model: None,
                }),
                note: None,
                first_draft_pending: false,
                settings: Default::default(),
                file: None,
                created_at: Default::default(),
                updated_at: Default::default(),
            },
        };
        let cancel = Arc::new(AtomicBool::new(false));
        cx.update(|cx| {
            popover.update(cx, |popover, cx| {
                popover.set_draft(Some(draft("a", None)), cx);
                assert_eq!(popover.agent(), "Claude Code");
                popover.open = Some(Open {
                    text: "Hi there\n".into(),
                    range: 3..9,
                    highlighted: None,
                    mode: Mode::Running {
                        label: "Generating alternatives…".into(),
                        previews: Vec::new(),
                        _cancel: CancelOnDrop(cancel.clone()),
                        _task: Task::ready(()),
                    },
                });
                popover.refresh_draft(Some(draft("a", Some("opus"))));
                assert!(popover.open.is_some());
                let meta = &popover.draft.as_ref().unwrap().meta;
                assert_eq!(
                    SelectionConfig::for_draft(meta, 3).model.as_deref(),
                    Some("opus")
                );
                // Not the draft the popover is for.
                popover.refresh_draft(Some(draft("b", Some("haiku"))));
                assert_eq!(popover.draft.as_ref().unwrap().id, "a");
                assert!(!cancel.load(Ordering::Relaxed));

                popover.set_draft(Some(draft("b", None)), cx);
                assert!(popover.open.is_none());
            });
        });
        assert!(cancel.load(Ordering::Relaxed));
    }

    #[gpui::test]
    fn options_from_the_running_request_show_and_stale_ones_dont(cx: &mut gpui::TestAppContext) {
        let config = tempfile::tempdir().unwrap();
        cx.update(|cx| crate::app_settings::init(config.path().to_owned(), cx));
        let window = cx.add_window(|_, _| gpui::EmptyView);
        let popover = window
            .update(cx, |_, window, cx| {
                let editor = cx.new(|cx| Editor::new("Hi there\n", cx));
                let preview = cx.new(TextSelection::new);
                let store = Store::new(config.path().join("drafts"));
                cx.new(|cx| SelectionPopover::new(editor, preview, store, window, cx))
            })
            .unwrap();
        let request = Arc::new(AtomicBool::new(false));
        let stale = Arc::new(AtomicBool::new(false));
        cx.update(|cx| {
            popover.update(cx, |popover, cx| {
                popover.open = Some(Open {
                    text: "Hi there\n".into(),
                    range: 3..8,
                    highlighted: None,
                    mode: Mode::Running {
                        label: "Generating alternatives…".into(),
                        previews: Vec::new(),
                        _cancel: CancelOnDrop(request.clone()),
                        _task: Task::ready(()),
                    },
                });
                let previews = |popover: &SelectionPopover| match &popover.open {
                    Some(Open {
                        mode: Mode::Running { previews, .. },
                        ..
                    }) => previews.clone(),
                    _ => panic!("not running"),
                };
                popover.options_arrived(&stale, vec!["stale".into()], false, cx);
                assert!(previews(popover).is_empty());
                popover.options_arrived(&request, vec!["first".into()], false, cx);
                assert_eq!(previews(popover), ["first"]);
                assert!(!request.load(Ordering::Relaxed));

                popover.options_arrived(&request, vec!["a".into(), "b".into()], true, cx);
                let open = popover.open.as_ref().unwrap();
                assert!(matches!(&open.mode, Mode::Results(options) if options == &["a", "b"]));
                assert_eq!(open.highlighted, Some(0));
                // The request is over: dropping it cancelled it, and late
                // options are ignored.
                assert!(request.load(Ordering::Relaxed));
                popover.options_arrived(&request, vec!["late".into()], true, cx);
                let open = popover.open.as_ref().unwrap();
                assert!(matches!(&open.mode, Mode::Results(options) if options == &["a", "b"]));
            });
        });
    }

    #[gpui::test]
    fn leaving_the_app_hides_the_options_until_it_is_back(cx: &mut gpui::TestAppContext) {
        let config = tempfile::tempdir().unwrap();
        cx.update(|cx| crate::app_settings::init(config.path().to_owned(), cx));
        let window = cx.add_window(|_, _| gpui::EmptyView);
        let popover = window
            .update(cx, |_, window, cx| {
                let editor = cx.new(|cx| Editor::new("Hi there\n", cx));
                let preview = cx.new(TextSelection::new);
                let store = Store::new(config.path().join("drafts"));
                cx.new(|cx| SelectionPopover::new(editor, preview, store, window, cx))
            })
            .unwrap();
        let cx = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            window.activate_window();
            popover.update(cx, |popover, _| {
                popover.open = Some(Open {
                    text: "Hi there\n".into(),
                    range: 3..8,
                    highlighted: Some(1),
                    mode: Mode::Results(vec!["a".into(), "b".into()]),
                });
            });
        });
        cx.run_until_parked();
        let options_shown = |cx: &mut gpui::VisualTestContext| {
            popover.read_with(cx, |popover, _| {
                matches!(
                    &popover.open,
                    Some(Open { mode: Mode::Results(options), highlighted: Some(1), .. })
                        if options == &["a", "b"]
                )
            })
        };

        cx.deactivate_window();
        cx.executor().advance_clock(Duration::from_millis(200));
        cx.run_until_parked();
        assert!(options_shown(cx));
        assert!(popover.read_with(cx, |popover, _| popover.hidden));

        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        assert!(options_shown(cx));
        assert!(popover.read_with(cx, |popover, _| !popover.hidden));

        // Esc still closes it.
        popover.update(cx, |popover, cx| popover.dismiss(cx));
        assert!(popover.read_with(cx, |popover, _| popover.open.is_none()));
    }

    fn claude_draft() -> Draft {
        Draft {
            id: "a".into(),
            dir: "/drafts/x".into(),
            meta: draftmux_storage::Meta {
                target: draftmux_storage::Target::Github,
                syntax: None,
                status: draftmux_storage::Status::Active,
                title: String::new(),
                source: None,
                cwd: "/repo".into(),
                backend: Backend::Claude(draftmux_storage::ClaudeSession {
                    session_id: "0196f1d2-7c1e-7a40-9b8e-3d1c2b4a5f60".parse().unwrap(),
                    transcript: None,
                    config_dir: None,
                    fork: None,
                    model: None,
                    selection_model: None,
                    effort: None,
                }),
                note: None,
                first_draft_pending: false,
                settings: Default::default(),
                file: None,
                created_at: Default::default(),
                updated_at: Default::default(),
            },
        }
    }

    /// A focused editor as the window's root, with a popover for it.
    fn continue_setup<'a>(
        text: &str,
        config: &std::path::Path,
        cx: &'a mut gpui::TestAppContext,
    ) -> (
        Entity<Editor>,
        Entity<SelectionPopover>,
        &'a mut gpui::VisualTestContext,
    ) {
        cx.update(|cx| {
            crate::app_settings::init(config.to_owned(), cx);
            editor::init(cx);
            init(cx);
        });
        let (editor, cx) = cx.add_window_view(|_, cx| Editor::new(text, cx));
        let popover = cx.update(|window, cx| {
            window.focus(&editor.focus_handle(cx), cx);
            let preview = cx.new(TextSelection::new);
            let store = Store::new(config.join("drafts"));
            cx.new(|cx| SelectionPopover::new(editor.clone(), preview, store, window, cx))
        });
        cx.run_until_parked();
        (editor, popover, cx)
    }

    #[gpui::test]
    fn the_ghost_shows_what_is_not_yet_accepted_and_ends_when_replies_diverge(
        cx: &mut gpui::TestAppContext,
    ) {
        let config = tempfile::tempdir().unwrap();
        let (editor, popover, cx) = continue_setup("The fox", config.path(), cx);
        let call = Arc::new(AtomicBool::new(false));
        let stale = Arc::new(AtomicBool::new(false));
        cx.update(|_, cx| {
            editor.update(cx, |editor, cx| {
                editor.select_range(7..7, cx);
                editor.set_ghost(7, "", cx);
            });
            popover.update(cx, |popover, cx| {
                popover.ghost = Some(GhostCall::new(CancelOnDrop(call.clone()), Task::ready(())));
                popover.ghost_arrived(&call, " jumps", false, cx);
                popover.ghost_arrived(&stale, " nope", false, cx);
            });
        });
        let ghost = |cx: &mut gpui::VisualTestContext| {
            editor.read_with(cx, |editor, _| editor.ghost().map(|g| g.text.clone()))
        };
        assert_eq!(ghost(cx).as_deref(), Some(" jumps"));

        // Accepting a word edits the draft but keeps the call.
        cx.simulate_keystrokes(editor::by_platform!("alt-right", "ctrl-right"));
        cx.update(|_, cx| {
            popover.update(cx, |popover, cx| {
                assert_eq!(popover.ghost.as_ref().unwrap().accepted, " jumps");
                popover.ghost_arrived(&call, " jumps over.", false, cx);
            })
        });
        assert_eq!(ghost(cx).as_deref(), Some(" over."));
        assert_eq!(
            editor.read_with(cx, |e, _| e.text().to_owned()),
            "The fox jumps"
        );
        // A retry starts over: the ghost waits; then it says something else.
        cx.update(|_, cx| popover.update(cx, |p, cx| p.ghost_arrived(&call, "", false, cx)));
        assert_eq!(ghost(cx).as_deref(), Some(""));
        cx.update(|_, cx| popover.update(cx, |p, cx| p.ghost_arrived(&call, " leaps.", false, cx)));
        assert_eq!(ghost(cx), None);
        assert!(call.load(Ordering::Relaxed), "the call was cancelled");
        assert_eq!(
            editor.read_with(cx, |e, _| e.text().to_owned()),
            "The fox jumps"
        );
    }

    #[gpui::test]
    fn a_finished_ghost_ends_once_accepted_and_failures_are_reported(
        cx: &mut gpui::TestAppContext,
    ) {
        let config = tempfile::tempdir().unwrap();
        let (editor, popover, cx) = continue_setup("Done.", config.path(), cx);
        let failures = Rc::new(std::cell::RefCell::new(Vec::new()));
        cx.update(|_, cx| {
            let failures = failures.clone();
            cx.subscribe(&popover, move |_, event, _| {
                if let SelectionPopoverEvent::ContinueFailed(reason) = event {
                    failures.borrow_mut().push(reason.clone());
                }
            })
            .detach();
        });
        let call = Arc::new(AtomicBool::new(false));
        cx.update(|_, cx| {
            editor.update(cx, |editor, cx| {
                editor.select_range(5..5, cx);
                editor.set_ghost(5, "", cx);
            });
            popover.update(cx, |popover, cx| {
                popover.ghost = Some(GhostCall::new(CancelOnDrop(call.clone()), Task::ready(())));
                popover.ghost_arrived(&call, " Next.", true, cx);
            });
        });
        cx.simulate_keystrokes("tab");
        assert_eq!(
            editor.read_with(cx, |e, _| e.text().to_owned()),
            "Done. Next."
        );
        assert!(popover.read_with(cx, |p, _| p.ghost.is_none()));
        assert!(call.load(Ordering::Relaxed));

        // A failure removes the ghost and says why; a superseded call's
        // failure doesn't.
        let call = Arc::new(AtomicBool::new(false));
        cx.update(|_, cx| {
            editor.update(cx, |editor, cx| editor.set_ghost(11, "", cx));
            popover.update(cx, |popover, cx| {
                popover.ghost = Some(GhostCall::new(CancelOnDrop(call.clone()), Task::ready(())));
                popover.ghost_failed(&Arc::new(AtomicBool::new(false)), "old".into(), cx);
                popover.ghost_failed(&call, "pi exited".into(), cx);
            })
        });
        cx.run_until_parked();
        assert_eq!(*failures.borrow(), ["pi exited"]);
        assert_eq!(editor.read_with(cx, |e, _| e.ghost().cloned()), None);
    }

    #[gpui::test]
    fn continue_options_collapse_the_selections_and_insert_at_the_caret(
        cx: &mut gpui::TestAppContext,
    ) {
        let config = tempfile::tempdir().unwrap();
        let (editor, popover, cx) = continue_setup("One two three", config.path(), cx);
        cx.update(|_, cx| {
            editor.update(cx, |editor, cx| {
                editor.select_range(0..3, cx);
                editor.add_selection(cx);
                editor.select_range(4..7, cx);
            });
            popover.update(cx, |popover, cx| {
                popover.set_draft(Some(claude_draft()), cx)
            });
        });
        cx.run_until_parked();
        // A ghost streaming elsewhere ends.
        let ghost_call = Arc::new(AtomicBool::new(false));
        cx.update(|_, cx| {
            editor.update(cx, |editor, cx| editor.set_ghost(7, " later", cx));
            popover.update(cx, |popover, cx| {
                popover.ghost = Some(GhostCall::new(
                    CancelOnDrop(ghost_call.clone()),
                    Task::ready(()),
                ));
                popover.continue_options(cx);
            });
        });
        cx.run_until_parked();
        assert!(ghost_call.load(Ordering::Relaxed));
        editor.read_with(cx, |editor, _| {
            assert_eq!(editor.ghost(), None);
            assert_eq!(editor.selected_range(), 7..7);
            assert!(editor.extra_selections().is_empty());
        });
        // The popover opened at the caret; the call can't start before the
        // shell environment is read, which shows in the popover.
        popover.read_with(cx, |popover, _| {
            let open = popover.open.as_ref().unwrap();
            assert_eq!(open.range, 7..7);
            assert_eq!(popover.action_name, "Continue");
            assert!(matches!(&open.mode, Mode::Error(reason) if reason.contains("shell")));
        });
        cx.update(|_, cx| {
            popover.update(cx, |popover, cx| {
                popover.set_mode(Mode::Results(vec![" and a half".into()]), Some(0), cx);
                popover.confirm(cx);
            });
            editor.update(cx, |editor, cx| {
                assert_eq!(editor.text(), "One two and a half three");
                assert!(editor.flashing().is_some());
                assert!(editor.undo(cx));
                assert_eq!(editor.text(), "One two three");
            });
        });
    }

    struct Wrapped {
        editor: Entity<Editor>,
        popover: Option<Entity<SelectionPopover>>,
    }

    impl Render for Wrapped {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let editor = self.editor.clone();
            match &self.popover {
                Some(popover) => SelectionPopover::wrap(popover, editor, cx),
                None => gpui::div().child(editor).into_any_element(),
            }
        }
    }

    #[gpui::test]
    fn the_continue_keys_go_to_the_draft_editor_over_shown_options(cx: &mut gpui::TestAppContext) {
        let config = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            crate::app_settings::init(config.path().to_owned(), cx);
            crate::theme::init(cx);
            editor::init(cx);
            init(cx);
        });
        let (root, cx) = cx.add_window_view(|_, cx| Wrapped {
            editor: cx.new(|cx| Editor::new("One two", cx)),
            popover: None,
        });
        let editor = root.read_with(cx, |root, _| root.editor.clone());
        let popover = cx.update(|window, cx| {
            window.focus(&editor.focus_handle(cx), cx);
            let preview = cx.new(TextSelection::new);
            let store = Store::new(config.path().join("drafts"));
            let popover =
                cx.new(|cx| SelectionPopover::new(editor.clone(), preview, store, window, cx));
            root.update(cx, |root, _| root.popover = Some(popover.clone()));
            popover
        });
        cx.update(|_, cx| {
            editor.update(cx, |editor, cx| editor.select_range(7..7, cx));
            popover.update(cx, |popover, cx| {
                popover.open = Some(Open {
                    text: "One two".into(),
                    range: 7..7,
                    highlighted: Some(0),
                    mode: Mode::Results(vec![" three".into()]),
                });
                cx.notify();
            });
        });
        cx.run_until_parked();
        cx.simulate_keystrokes(editor::secondary!("enter"));
        cx.run_until_parked();
        assert_eq!(editor.read_with(cx, |e, _| e.text().to_owned()), "One two");
        assert!(popover.read_with(cx, |p, _| p.open.is_none()));
    }

    #[gpui::test]
    fn the_ghost_outlasts_leaving_the_app_but_not_focus_leaving_the_editor(
        cx: &mut gpui::TestAppContext,
    ) {
        let config = tempfile::tempdir().unwrap();
        let (editor, popover, cx) = continue_setup("The fox", config.path(), cx);
        let call = Arc::new(AtomicBool::new(false));
        cx.update(|window, cx| {
            window.activate_window();
            editor.update(cx, |editor, cx| {
                editor.select_range(7..7, cx);
                editor.set_ghost(7, " jumps", cx);
            });
            popover.update(cx, |popover, _| {
                popover.ghost = Some(GhostCall::new(CancelOnDrop(call.clone()), Task::ready(())));
            });
        });
        cx.run_until_parked();
        cx.deactivate_window();
        cx.run_until_parked();
        assert!(popover.read_with(cx, |p, _| p.ghost.is_some()));
        assert!(!call.load(Ordering::Relaxed));

        cx.update(|window, _| {
            window.activate_window();
        });
        cx.run_until_parked();
        assert!(popover.read_with(cx, |p, _| p.ghost.is_some()));
        cx.update(|window, cx| window.blur(cx));
        cx.run_until_parked();
        assert!(popover.read_with(cx, |p, _| p.ghost.is_none()));
        assert_eq!(editor.read_with(cx, |e, _| e.ghost().cloned()), None);
        assert!(call.load(Ordering::Relaxed));
    }
}
