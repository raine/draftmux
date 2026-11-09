//! The Settings window (history/docs/prototype/settings.html): scopes in a sidebar.
//!
//! General and Selection actions hold app-wide settings. Under Agent, All
//! directories holds the defaults and each directory rule its overrides. A
//! rule's unset fields show the value they inherit and where it comes from;
//! overridden fields get a dot and a reset button. Every change is saved at
//! once and applies to drafts created afterwards.

mod widgets;

use std::path::{Path, PathBuf};

use draftmux_pi_profiles::{Availability, ModelRef, availability};
use draftmux_pi_rpc::{ModelOverride, ThinkingLevel};
use draftmux_settings::{
    ALTERNATIVES, DraftingModel, PiReads, Resolved, Rule, Scope, SelectionModel, Settings, Source,
    Trust, default_profile, pi_reads, tilde_path,
};
use draftmux_storage::Target;
use editor::{Editor, EditorEvent, EditorStyle};
use gpui::{
    Anchor, AnyElement, App, Context, Entity, FocusHandle, Focusable, FontWeight, Global,
    KeyBinding, MouseButton, PathPromptOptions, SharedString, Subscription, TitlebarOptions,
    Window, WindowBackgroundAppearance, WindowBounds, WindowControlArea, WindowHandle,
    WindowOptions, actions, anchored, deferred, div, font, point, prelude::*, px, size, svg,
};

use crate::app_settings::AppSettings;
use crate::assets::{FOLDER_ICON, GEAR_ICON, GLOBE_ICON, PLUS_ICON, SPARK_ICON};
use crate::model_picker::{ModelPicker, PickerEvent};
use crate::profiles::{ModelList, ProfileCatalog};
use crate::theme::{self, Palette};
use widgets::*;

actions!(settings, [OpenSettings, CloseSettings]);

const SIDEBAR_WIDTH: f32 = 210.;
const HEADER_HEIGHT: f32 = 52.;

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-w", CloseSettings, Some("Settings")),
    ]);
    cx.on_action(|_: &OpenSettings, cx| open(cx));
}

struct OpenWindow(WindowHandle<SettingsWindow>);

impl Global for OpenWindow {}

/// Opens the Settings window, or brings it to the front.
pub fn open(cx: &mut App) {
    if let Some(handle) = cx.try_global::<OpenWindow>().map(|open| open.0)
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return;
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(760.), px(600.)), cx)),
        window_min_size: Some(size(px(640.), px(420.))),
        titlebar: Some(TitlebarOptions {
            title: Some("Settings".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(16.), px(18.))),
        }),
        window_background: WindowBackgroundAppearance::Blurred,
        app_owns_titlebar_drag: true,
        ..Default::default()
    };
    match cx.open_window(options, |window, cx| {
        cx.new(|cx| SettingsWindow::new(window, cx))
    }) {
        Ok(handle) => cx.set_global(OpenWindow(handle)),
        Err(err) => eprintln!("draftmux: couldn't open the Settings window: {err:#}"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    General,
    Actions,
    Defaults,
    /// Index into `Settings::directories`.
    Rule(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelField {
    Drafting,
    Selection,
}

/// A field with a pop-up menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Profile,
    Model(ModelField),
    Thinking(ModelField),
    Target,
    ContextFiles,
    Alternatives,
}

impl ModelField {
    /// Whether `scope` sets the field.
    fn is_set(self, scope: &Scope) -> bool {
        match self {
            ModelField::Drafting => scope.drafting_model.is_some(),
            ModelField::Selection => scope.selection_model.is_some(),
        }
    }

    /// The model `scope` names for the field.
    fn own_model(self, scope: &Scope) -> Option<&ModelOverride> {
        match self {
            ModelField::Drafting => scope.drafting_model.as_ref()?.model(),
            ModelField::Selection => scope.selection_model.as_ref()?.model(),
        }
    }

    /// The model the field resolves to; `None` follows the session.
    fn model(self, resolved: &Resolved) -> Option<&ModelOverride> {
        match self {
            ModelField::Drafting => resolved.drafting_model.value.model(),
            ModelField::Selection => resolved.selection_model.value.model(),
        }
    }

    fn source(self, resolved: &Resolved) -> Source {
        match self {
            ModelField::Drafting => resolved.drafting_model.source,
            ModelField::Selection => resolved.selection_model.source,
        }
    }

    /// What the field shows when it names no model.
    fn follows(self) -> &'static str {
        match self {
            ModelField::Drafting => "Same as session",
            ModelField::Selection => "Same as drafting",
        }
    }

    fn label(self, resolved: &Resolved) -> SharedString {
        match self.model(resolved) {
            Some(model) => model_ref(model).to_string().into(),
            None => self.follows().into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Choice {
    /// Unset the field.
    Inherit,
    Profile(PathBuf),
    SameAsSession,
    SameAsDrafting,
    ChooseModel(ModelField),
    Thinking(ModelField, ThinkingLevel),
    Target(Target),
    Alternatives(usize),
}

pub struct SettingsWindow {
    page: Page,
    home: PathBuf,
    focus_handle: FocusHandle,
    menu: Option<(Field, Vec<MenuItem<Choice>>)>,
    picker: Option<(ModelField, Entity<ModelPicker>, Subscription)>,
    instructions: Entity<Editor>,
    /// What pi reads in the current rule's directory.
    pi_reads: Option<PiReads>,
    error: Option<SharedString>,
    should_move: bool,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for SettingsWindow {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl SettingsWindow {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let instructions = theme::editor(instructions_style, cx);
        let settings = AppSettings::global(cx);
        let catalog = ProfileCatalog::global(cx);
        let subscriptions = vec![
            cx.subscribe(&instructions, |this, editor, event, cx| {
                if matches!(event, EditorEvent::Edited) {
                    let text = editor.read(cx).text().to_owned();
                    this.set_instructions(text, cx);
                }
            }),
            cx.observe(&settings, |this, _, cx| {
                this.refresh_pi_reads(cx);
                cx.notify();
            }),
            cx.observe(&catalog, |this, _, cx| {
                this.refresh_picker_models(cx);
                cx.notify();
            }),
            cx.on_release(|_, cx| {
                cx.remove_global::<OpenWindow>();
            }),
        ];
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        let mut this = Self {
            page: Page::Defaults,
            home: std::env::home_dir().unwrap_or_default(),
            focus_handle,
            menu: None,
            picker: None,
            instructions,
            pi_reads: None,
            error: None,
            should_move: false,
            _subscriptions: subscriptions,
        };
        this.show(Page::Defaults, window, cx);
        this
    }

    fn settings<'a>(&self, cx: &'a App) -> &'a Settings {
        &AppSettings::global(cx).read(cx).settings
    }

    fn show(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        self.page = page;
        self.menu = None;
        self.picker = None;
        self.error = None;
        let text = scope(self.settings(cx), page)
            .and_then(|scope| scope.instructions.clone())
            .unwrap_or_default();
        self.instructions
            .update(cx, |editor, cx| editor.set_text(&text, cx));
        self.refresh_pi_reads(cx);
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn refresh_pi_reads(&mut self, cx: &App) {
        self.pi_reads = match self.page {
            Page::Rule(ix) => self.settings(cx).directories.get(ix).map(|rule| {
                let dir = draftmux_settings::expand_home(&rule.path, &self.home);
                pi_reads(&dir, &self.profile_dir(&self.resolved(cx)), &self.home)
            }),
            _ => None,
        };
    }

    /// Applies `change` to the settings and saves them.
    fn update(&mut self, change: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        AppSettings::global(cx).update(cx, |settings, cx| settings.update(change, cx));
    }

    fn update_scope(&mut self, change: impl FnOnce(&mut Scope), cx: &mut Context<Self>) {
        let page = self.page;
        self.update(
            |settings| {
                if let Some(scope) = scope_mut(settings, page) {
                    change(scope)
                }
            },
            cx,
        );
    }

    fn set_instructions(&mut self, text: String, cx: &mut Context<Self>) {
        let own = (!text.trim().is_empty()).then_some(text);
        if scope(self.settings(cx), self.page).is_none_or(|s| s.instructions == own) {
            return;
        }
        self.update_scope(|scope| scope.instructions = own, cx);
    }

    /// The current scope's values: the defaults, or a rule's at its own
    /// directory.
    fn resolved(&self, cx: &App) -> Resolved {
        let settings = self.settings(cx);
        match self.page {
            Page::Rule(ix) if ix < settings.directories.len() => {
                settings.resolve(&settings.directories[ix].path, &self.home)
            }
            _ => settings.resolve_defaults(&self.home),
        }
    }

    /// What the current rule inherits; `None` for other pages.
    fn inherited(&self, cx: &App) -> Option<Resolved> {
        match self.page {
            Page::Rule(ix) if ix < self.settings(cx).directories.len() => {
                Some(self.settings(cx).inherited(ix, &self.home))
            }
            _ => None,
        }
    }

    /// The profile the scope runs pi with.
    fn profile_dir(&self, resolved: &Resolved) -> PathBuf {
        resolved
            .profile
            .value
            .clone()
            .unwrap_or_else(|| default_profile(&self.home))
    }

    fn profile_models(&self, dir: &Path, cx: &App) -> (SharedString, ModelList) {
        let catalog = ProfileCatalog::global(cx).read(cx);
        match catalog.entry(dir) {
            Some(entry) => (entry.profile.name.clone().into(), entry.models.clone()),
            None => (
                self.tilde(dir).into(),
                ModelList::Failed("This profile folder isn't one Draftmux knows".into()),
            ),
        }
    }

    fn tilde(&self, path: &Path) -> String {
        tilde_path(path, &self.home).display().to_string()
    }

    fn source_name(&self, source: Source, cx: &App) -> SharedString {
        match source {
            Source::Builtin => "Default".into(),
            Source::AllDirectories => "All directories".into(),
            Source::Rule(ix) => self
                .settings(cx)
                .directories
                .get(ix)
                .map(|rule| rule.path.display().to_string())
                .unwrap_or_default()
                .into(),
        }
    }

    fn open_menu(&mut self, field: Field, cx: &mut Context<Self>) {
        if self.menu.as_ref().is_some_and(|(open, _)| *open == field) {
            self.menu = None;
            return cx.notify();
        }
        self.picker = None;
        let items = self.menu_items(field, cx);
        self.menu = Some((field, items));
        cx.notify();
    }

    fn menu_items(&self, field: Field, cx: &App) -> Vec<MenuItem<Choice>> {
        let item = |label: SharedString, checked: bool, choice: Choice| MenuItem {
            label,
            checked,
            action: Some(choice),
        };
        let separator = || MenuItem {
            label: SharedString::default(),
            checked: false,
            action: None,
        };
        let own = scope(self.settings(cx), self.page)
            .cloned()
            .unwrap_or_default();
        let resolved = self.resolved(cx);
        let inherited = self.inherited(cx);
        let is_rule = inherited.is_some();
        let mut items = Vec::new();
        let inherit = inherited.as_ref().and_then(|inherited| {
            let (value, unset) = match field {
                Field::Profile => (
                    self.profile_label(inherited.profile.value.as_deref(), cx),
                    own.profile.is_none(),
                ),
                Field::Model(model_field) => {
                    (model_field.label(inherited), !model_field.is_set(&own))
                }
                Field::Target => (inherited.target.value.label().into(), own.target.is_none()),
                Field::Thinking(_) | Field::ContextFiles | Field::Alternatives => return None,
            };
            Some(item(
                format!("Inherit ({value})").into(),
                unset,
                Choice::Inherit,
            ))
        });
        if let Some(inherit) = inherit {
            items.push(inherit);
            items.push(separator());
        }
        // A rule checks only what it sets itself; All directories checks
        // the value in effect, built-in defaults included.
        match field {
            Field::Profile => {
                if !is_rule {
                    items.push(item(
                        format!("Default · {}", self.tilde(&default_profile(&self.home))).into(),
                        own.profile.is_none(),
                        Choice::Inherit,
                    ));
                    items.push(separator());
                }
                let own_dir = own
                    .profile
                    .as_ref()
                    .map(|dir| draftmux_settings::expand_home(dir, &self.home));
                for entry in ProfileCatalog::global(cx).read(cx).entries() {
                    let dir = &entry.profile.dir;
                    items.push(item(
                        format!("{} · {}", entry.profile.name, self.tilde(dir)).into(),
                        own_dir.as_ref() == Some(dir),
                        Choice::Profile(dir.clone()),
                    ));
                }
            }
            Field::Model(model_field) => {
                let follows = match is_rule {
                    true => model_field.is_set(&own) && model_field.own_model(&own).is_none(),
                    false => model_field.model(&resolved).is_none(),
                };
                let choice = match model_field {
                    ModelField::Drafting => Choice::SameAsSession,
                    ModelField::Selection => Choice::SameAsDrafting,
                };
                items.push(item(model_field.follows().into(), follows, choice));
                if let Some(model) = model_field.own_model(&own) {
                    items.push(item(
                        model_ref(model).to_string().into(),
                        true,
                        Choice::ChooseModel(model_field),
                    ));
                }
                items.push(item(
                    "Choose model…".into(),
                    false,
                    Choice::ChooseModel(model_field),
                ));
            }
            Field::Thinking(model_field) => {
                let current = model_field.model(&resolved).map(|m| m.thinking);
                for level in ThinkingLevel::ALL {
                    items.push(item(
                        level.as_str().into(),
                        current == Some(level),
                        Choice::Thinking(model_field, level),
                    ));
                }
            }
            Field::Target => {
                for &target in Target::ALL {
                    let checked = match is_rule {
                        true => own.target == Some(target),
                        false => resolved.target.value == target,
                    };
                    items.push(item(target.label().into(), checked, Choice::Target(target)));
                }
            }
            Field::Alternatives => {
                let current = self.settings(cx).alternatives();
                for count in ALTERNATIVES {
                    items.push(item(
                        count.to_string().into(),
                        count == current,
                        Choice::Alternatives(count),
                    ));
                }
            }
            Field::ContextFiles => {}
        }
        items
    }

    fn choose(&mut self, choice: Choice, window: &mut Window, cx: &mut Context<Self>) {
        let Some((field, _)) = self.menu.take() else {
            return;
        };
        cx.notify();
        match choice {
            Choice::ChooseModel(model_field) => self.open_picker(model_field, window, cx),
            Choice::Inherit => self.reset(field, cx),
            Choice::Profile(dir) => {
                let dir = tilde_path(&dir, &self.home);
                self.update_scope(|scope| scope.profile = Some(dir), cx)
            }
            Choice::SameAsSession => self.update_scope(
                |scope| scope.drafting_model = Some(DraftingModel::Session),
                cx,
            ),
            Choice::SameAsDrafting => self.update_scope(
                |scope| scope.selection_model = Some(SelectionModel::Drafting),
                cx,
            ),
            Choice::Thinking(model_field, level) => {
                if let Some(model) = model_field.model(&self.resolved(cx)).cloned() {
                    self.set_model(
                        model_field,
                        ModelOverride {
                            thinking: level,
                            ..model
                        },
                        cx,
                    );
                }
            }
            Choice::Target(target) => self.update_scope(|scope| scope.target = Some(target), cx),
            Choice::Alternatives(count) => self.update(
                |settings| {
                    settings.alternatives =
                        (count != draftmux_settings::DEFAULT_ALTERNATIVES).then_some(count)
                },
                cx,
            ),
        }
    }

    fn set_model(&mut self, field: ModelField, model: ModelOverride, cx: &mut Context<Self>) {
        self.update_scope(
            |scope| match field {
                ModelField::Drafting => scope.drafting_model = Some(DraftingModel::Model(model)),
                ModelField::Selection => scope.selection_model = Some(SelectionModel::Model(model)),
            },
            cx,
        );
    }

    /// Removes the current scope's own value of `field`.
    fn reset(&mut self, field: Field, cx: &mut Context<Self>) {
        self.update_scope(
            |scope| match field {
                Field::Profile => scope.profile = None,
                Field::Model(ModelField::Drafting) | Field::Thinking(ModelField::Drafting) => {
                    scope.drafting_model = None
                }
                Field::Model(ModelField::Selection) | Field::Thinking(ModelField::Selection) => {
                    scope.selection_model = None
                }
                Field::Target => scope.target = None,
                Field::ContextFiles => scope.context_files = None,
                Field::Alternatives => {}
            },
            cx,
        );
    }

    fn open_picker(&mut self, field: ModelField, window: &mut Window, cx: &mut Context<Self>) {
        let resolved = self.resolved(cx);
        let (profile_name, models) = self.profile_models(&self.profile_dir(&resolved), cx);
        let current = field.model(&resolved).map(model_ref);
        let picker = cx.new(|cx| {
            let mut picker = ModelPicker::new(profile_name, models, cx);
            picker.set_current(current.clone(), cx);
            picker.set_configured(current, cx);
            picker
        });
        let subscription =
            cx.subscribe_in(
                &picker,
                window,
                move |this, _, event, window, cx| match event {
                    PickerEvent::Picked(model) => {
                        let thinking = this.default_thinking(field, cx);
                        let model = ModelOverride {
                            provider: model.provider.clone(),
                            model_id: model.id.clone(),
                            thinking,
                        };
                        this.set_model(field, model, cx);
                        this.close_picker(window, cx);
                    }
                    PickerEvent::Dismissed => this.close_picker(window, cx),
                    PickerEvent::Retry => {
                        let dir = this.profile_dir(&this.resolved(cx));
                        ProfileCatalog::global(cx)
                            .update(cx, |catalog, cx| catalog.reload(&dir, cx));
                    }
                },
            );
        window.focus(&picker.focus_handle(cx), cx);
        self.picker = Some((field, picker, subscription));
        cx.notify();
    }

    /// The thinking level a newly picked model starts on: the field's
    /// current one, else the profile's default, else medium.
    fn default_thinking(&self, field: ModelField, cx: &App) -> ThinkingLevel {
        let resolved = self.resolved(cx);
        if let Some(model) = field.model(&resolved) {
            return model.thinking;
        }
        ProfileCatalog::global(cx)
            .read(cx)
            .entry(&self.profile_dir(&resolved))
            .and_then(|entry| entry.defaults.as_ref().ok()?.thinking_level.as_deref())
            .and_then(ThinkingLevel::parse)
            .unwrap_or(ThinkingLevel::Medium)
    }

    fn close_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn refresh_picker_models(&mut self, cx: &mut Context<Self>) {
        if let Some((_, picker, _)) = &self.picker {
            let (_, models) = self.profile_models(&self.profile_dir(&self.resolved(cx)), cx);
            picker.update(cx, |picker, cx| picker.set_models(models, cx));
        }
    }

    fn add_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(dir) = paths.into_iter().next() else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                // A folder that already has a rule opens it instead.
                let ix = match this.settings(cx).rule_for(&dir, &this.home) {
                    Some(ix) => ix,
                    None => {
                        let path = tilde_path(&dir, &this.home);
                        this.update(|settings| settings.directories.push(Rule::new(path)), cx);
                        this.settings(cx).directories.len() - 1
                    }
                };
                this.show(Page::Rule(ix), window, cx);
            })
            .ok();
        })
        .detach();
    }

    fn add_profile(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add profile".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(dir) = paths.into_iter().next() else {
                return;
            };
            this.update(cx, |this, cx| {
                let added =
                    ProfileCatalog::global(cx).update(cx, |catalog, cx| catalog.add(&dir, cx));
                this.error = added.err().map(|err| format!("{err:#}").into());
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn remove_rule(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.update(
            |settings| {
                if ix < settings.directories.len() {
                    settings.directories.remove(ix);
                }
            },
            cx,
        );
        self.show(Page::Defaults, window, cx);
    }

    fn profile_label(&self, dir: Option<&Path>, cx: &App) -> SharedString {
        let dir = dir.map_or_else(|| default_profile(&self.home), Path::to_owned);
        let catalog = ProfileCatalog::global(cx).read(cx);
        match catalog.entry(&dir) {
            Some(entry) => format!("{} · {}", entry.profile.name, self.tilde(&dir)).into(),
            None => self.tilde(&dir).into(),
        }
    }
}

/// Gives each page its own scroll position.
fn page_key(page: Page) -> usize {
    match page {
        Page::General => 0,
        Page::Actions => 1,
        Page::Defaults => 2,
        Page::Rule(ix) => 3 + ix,
    }
}

fn scope(settings: &Settings, page: Page) -> Option<&Scope> {
    match page {
        Page::Defaults => Some(&settings.defaults),
        Page::Rule(ix) => settings.directories.get(ix).map(|rule| &rule.scope),
        _ => None,
    }
}

fn scope_mut(settings: &mut Settings, page: Page) -> Option<&mut Scope> {
    match page {
        Page::Defaults => Some(&mut settings.defaults),
        Page::Rule(ix) => settings.directories.get_mut(ix).map(|rule| &mut rule.scope),
        _ => None,
    }
}

fn model_ref(model: &ModelOverride) -> ModelRef {
    ModelRef::new(&model.provider, &model.model_id)
}

fn instructions_style(palette: &Palette) -> EditorStyle {
    EditorStyle {
        font: font(".SystemUIFont"),
        font_size: px(13.),
        line_height: px(13. * 1.45),
        padding_x: px(8.),
        padding_top: px(6.),
        padding_bottom: px(6.),
        text: palette.text,
        heading: palette.text,
        ..palette.editor_style()
    }
}

// Rendering.
impl SettingsWindow {
    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = &Colors::get(cx);
        let settings = self.settings(cx);
        let nav = |id: SharedString,
                   page: Page,
                   icon: &'static str,
                   text: SharedString,
                   count: Option<usize>,
                   mono_text: bool,
                   cx: &mut Context<Self>| {
            div()
                .id(gpui::ElementId::Name(id))
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(8.))
                .py(px(5.))
                .rounded(px(6.))
                .when(self.page == page, |nav| nav.bg(c.selected_row))
                .child(
                    svg()
                        .path(icon)
                        .size(px(15.))
                        .flex_none()
                        .text_color(c.accent),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .when(mono_text, |text| text.font_family(MONO).text_size(px(12.)))
                        .child(text),
                )
                .when_some(count.filter(|n| *n > 0), |nav, n| {
                    nav.child(
                        div()
                            .text_size(px(11.))
                            .text_color(c.faint)
                            .child(n.to_string()),
                    )
                })
                .on_click(cx.listener(move |this, _, window, cx| this.show(page, window, cx)))
        };
        let rules: Vec<(usize, SharedString, usize)> = settings
            .directories
            .iter()
            .enumerate()
            .map(|(ix, rule)| {
                (
                    ix,
                    rule.path.display().to_string().into(),
                    rule.override_count(),
                )
            })
            .collect();
        let mut side = div()
            .w(px(SIDEBAR_WIDTH))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(c.sidebar)
            .border_r(px(0.5))
            .border_color(c.divider)
            .px(px(10.))
            .pt(px(HEADER_HEIGHT))
            .child(nav(
                "nav-general".into(),
                Page::General,
                GEAR_ICON,
                "General".into(),
                None,
                false,
                cx,
            ))
            .child(nav(
                "nav-actions".into(),
                Page::Actions,
                SPARK_ICON,
                "Selection actions".into(),
                None,
                false,
                cx,
            ))
            .child(
                div()
                    .mt(px(14.))
                    .mb(px(4.))
                    .mx(px(8.))
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(c.faint)
                    .child("Agent"),
            )
            .child(nav(
                "nav-defaults".into(),
                Page::Defaults,
                GLOBE_ICON,
                "All directories".into(),
                None,
                false,
                cx,
            ));
        for (ix, path, count) in rules {
            side = side.child(nav(
                format!("nav-rule-{ix}").into(),
                Page::Rule(ix),
                FOLDER_ICON,
                path,
                Some(count),
                true,
                cx,
            ));
        }
        side.child(
            div()
                .id("nav-add")
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(8.))
                .py(px(5.))
                .rounded(px(6.))
                .text_color(c.muted)
                .child(svg().path(PLUS_ICON).size(px(15.)).text_color(c.muted))
                .child("Add directory…")
                .on_click(cx.listener(|this, _, window, cx| this.add_directory(window, cx))),
        )
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = &Colors::get(cx);
        let settings = self.settings(cx);
        let (title, sub, mono_title): (SharedString, Option<SharedString>, bool) = match self.page {
            Page::General => ("General".into(), None, false),
            Page::Actions => ("Selection actions".into(), None, false),
            Page::Defaults => (
                "All directories".into(),
                Some("Defaults for every draft".into()),
                false,
            ),
            Page::Rule(ix) => (
                settings
                    .directories
                    .get(ix)
                    .map(|r| r.path.display().to_string())
                    .unwrap_or_default()
                    .into(),
                Some("This directory and everything below it".into()),
                true,
            ),
        };
        div()
            .h(px(HEADER_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(22.))
            .border_b(px(0.5))
            .border_color(c.divider)
            .window_control_area(WindowControlArea::Drag)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.should_move = true),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.should_move = false),
            )
            .on_mouse_move(cx.listener(|this, _, window, _| {
                if this.should_move {
                    this.should_move = false;
                    window.start_window_move();
                }
            }))
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .when(mono_title, |title| title.font_family(MONO))
                    .child(title),
            )
            .when_some(sub, |header, sub| {
                header.child(div().text_size(px(12.)).text_color(c.faint).child(sub))
            })
            .when_some(
                match self.page {
                    Page::Rule(ix) => Some(ix),
                    _ => None,
                },
                |header, ix| {
                    header.child(div().ml_auto().child(push_button(
                        c,
                        "remove-rule",
                        "Remove",
                        cx.listener(move |this, _, window, cx| this.remove_rule(ix, window, cx)),
                    )))
                },
            )
    }

    /// A pop-up for `field` with its menu or model picker when open.
    fn popup(
        &self,
        field: Field,
        text: SharedString,
        inherited: bool,
        width: Option<f32>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = &Colors::get(cx);
        let id = SharedString::from(format!("popup-{field:?}"));
        let menu = self
            .menu
            .as_ref()
            .filter(|(open, _)| *open == field)
            .map(|(_, items)| {
                let this = cx.entity().downgrade();
                menu_panel(c, items, move |choice, window, cx| {
                    this.update(cx, |this, cx| this.choose(choice, window, cx))
                        .ok();
                })
            });
        let picker = self
            .picker
            .as_ref()
            .filter(|(open, ..)| Field::Model(*open) == field)
            .map(|(_, picker, _)| picker.clone());
        div()
            .relative()
            .child(popup_button(
                c,
                gpui::ElementId::Name(id),
                text,
                inherited,
                width,
                cx.listener(move |this, _, _, cx| this.open_menu(field, cx)),
            ))
            .when_some(menu, |button, menu| {
                button.child(deferred(
                    anchored()
                        .anchor(Anchor::TopLeft)
                        .offset(point(px(0.), px(24.)))
                        .child(
                            div()
                                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                    this.menu = None;
                                    cx.notify();
                                }))
                                .child(menu),
                        ),
                ))
            })
            .when_some(picker, |button, picker| {
                button.child(deferred(
                    anchored()
                        .anchor(Anchor::TopLeft)
                        .offset(point(px(0.), px(24.)))
                        .child(picker),
                ))
            })
            .into_any_element()
    }

    /// A form row of a field that a rule can override: on a rule page, an
    /// overridden field gets a dot and a reset button, an inherited one the
    /// name of the scope it comes from.
    #[expect(clippy::too_many_arguments, reason = "one call per row kind")]
    fn scoped_row(
        &self,
        field: Field,
        title: &'static str,
        notes: Vec<AnyElement>,
        set: bool,
        inherited_from: Option<Source>,
        control: impl IntoElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = &Colors::get(cx);
        let overridden = set && inherited_from.is_some();
        let marker = if overridden {
            Some(
                reset_button(
                    c,
                    gpui::ElementId::Name(format!("reset-{field:?}").into()),
                    cx.listener(move |this, _, _, cx| this.reset(field, cx)),
                )
                .into_any_element(),
            )
        } else {
            inherited_from
                .map(|source| source_tag(c, self.source_name(source, cx), false).into_any_element())
        };
        row(
            label(c, title, overridden),
            notes,
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .children(marker)
                .child(control),
        )
        .into_any_element()
    }

    fn agent_form(
        &self,
        own: &Scope,
        resolved: &Resolved,
        inherited: Option<&Resolved>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let c = &Colors::get(cx);
        let is_rule = inherited.is_some();
        let profile_row = self.scoped_row(
            Field::Profile,
            "pi profile",
            vec![description(
                c,
                "For sessions not stored in a pi profile, such as Claude Code handoffs",
            )],
            own.profile.is_some(),
            inherited.map(|i| i.profile.source),
            self.popup(
                Field::Profile,
                self.profile_label(resolved.profile.value.as_deref(), cx),
                own.profile.is_none(),
                None,
                cx,
            ),
            cx,
        );
        let drafting = self.model_row(ModelField::Drafting, own, resolved, inherited, cx);
        let selection = self.model_row(ModelField::Selection, own, resolved, inherited, cx);
        let target_row = self.scoped_row(
            Field::Target,
            "Default target",
            vec![description(c, "For drafts handed off without --target")],
            own.target.is_some(),
            inherited.map(|i| i.target.source),
            self.popup(
                Field::Target,
                resolved.target.value.label().into(),
                own.target.is_none(),
                None,
                cx,
            ),
            cx,
        );
        let instructions = self.instructions_row(own, inherited, cx);
        let context_on = resolved.context_files.value;
        let context_row = self.scoped_row(
            Field::ContextFiles,
            "Context files",
            vec![description(
                c,
                "AGENTS.md and CLAUDE.md up the directory tree. Turning them off changes \
                 the system prompt, so the first reply isn't cached",
            )],
            own.context_files.is_some(),
            inherited.map(|i| i.context_files.source),
            switch(
                c,
                "context-files",
                context_on,
                own.context_files.is_none() && is_rule,
                cx.listener(move |this, _, _, cx| {
                    this.update_scope(|scope| scope.context_files = Some(!context_on), cx)
                }),
            ),
            cx,
        );

        let mut form = vec![
            group_title(c, "Models").into_any_element(),
            group(c, vec![profile_row, drafting, selection]).into_any_element(),
        ];
        if !is_rule {
            form.push(
                note(
                    c,
                    "Drafts continue on the model their source session used, and selection \
                     actions reuse it, so the provider's prompt cache covers the forked \
                     conversation. The profile's settings.json default only applies when a \
                     session names no model, as with Claude Code handoffs.",
                )
                .into_any_element(),
            );
        }
        form.push(group_title(c, "Drafts").into_any_element());
        form.push(group(c, vec![target_row, instructions]).into_any_element());
        form.push(group_title(c, "pi setup").into_any_element());
        form.push(group(c, vec![context_row]).into_any_element());
        form
    }

    fn model_row(
        &self,
        field: ModelField,
        own: &Scope,
        resolved: &Resolved,
        inherited: Option<&Resolved>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = &Colors::get(cx);
        let (title, desc) = match field {
            ModelField::Drafting => ("Drafting model", "Chat and draft edits"),
            ModelField::Selection => (
                "Selection model",
                "Alternatives and actions on selected text",
            ),
        };
        let set = field.is_set(own);
        let model = field.model(resolved);
        let mut notes = vec![description(c, desc)];
        match model {
            None => notes.push(
                div()
                    .mt(px(2.))
                    .child(source_tag(c, "Keeps the prompt cache", true))
                    .into_any_element(),
            ),
            Some(model) => {
                let (profile, models) = self.profile_models(&self.profile_dir(resolved), cx);
                if let Some(list) = models.loaded()
                    && availability(Some(list), &model_ref(model)) == Availability::Missing
                {
                    notes.push(warning(
                        c,
                        format!(
                            "{} isn't available in the {profile} profile",
                            model_ref(model)
                        ),
                    ));
                } else if field == ModelField::Drafting {
                    notes.push(description(
                        c,
                        "New drafts switch to this model, so their first reply re-reads the \
                         conversation without the cache",
                    ));
                }
            }
        }
        let thinking = match model {
            Some(model) => self
                .popup(
                    Field::Thinking(field),
                    model.thinking.as_str().into(),
                    !set && inherited.is_some(),
                    Some(96.),
                    cx,
                )
                .into_any_element(),
            None => div()
                .w(px(96.))
                .h(px(22.))
                .flex()
                .items_center()
                .px(px(8.))
                .text_color(c.faint)
                .child("—")
                .into_any_element(),
        };
        self.scoped_row(
            Field::Model(field),
            title,
            notes,
            set,
            inherited.map(|i| field.source(i)),
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(self.popup(
                    Field::Model(field),
                    field.label(resolved),
                    !set,
                    Some(190.),
                    cx,
                ))
                .child(thinking),
            cx,
        )
    }

    fn instructions_row(
        &self,
        own: &Scope,
        inherited: Option<&Resolved>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = &Colors::get(cx);
        let rule = match self.page {
            Page::Rule(ix) => self.settings(cx).directories.get(ix).map(|rule| (ix, rule)),
            _ => None,
        };
        let replace = rule.is_some_and(|(_, rule)| rule.replace_instructions);
        let overridden = rule.is_some() && (own.instructions.is_some() || replace);
        let inherited_text = inherited.and_then(|i| {
            i.instructions
                .value
                .clone()
                .map(|text| (text, self.source_name(i.instructions.source, cx)))
        });
        let desc = match (&inherited_text, replace) {
            (None, _) => "Sent after the conversation, so the cached prefix is untouched",
            (Some(_), false) => "Added after the inherited instructions",
            (Some(_), true) => "Replace the inherited instructions; leave empty to clear them",
        };
        let header = div()
            .flex()
            .items_end()
            .justify_between()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(label(c, "Drafting instructions", overridden))
                    .child(description(c, desc)),
            )
            .when(overridden, |header| {
                header.child(reset_button(
                    c,
                    "reset-instructions",
                    cx.listener(|this, _, window, cx| {
                        let page = this.page;
                        this.update(
                            |settings| {
                                if let Page::Rule(ix) = page
                                    && let Some(rule) = settings.directories.get_mut(ix)
                                {
                                    rule.scope.instructions = None;
                                    rule.replace_instructions = false;
                                }
                            },
                            cx,
                        );
                        this.show(page, window, cx);
                    }),
                ))
            });
        let inherited_box = inherited_text.map(|(text, from)| {
            div()
                .px(px(8.))
                .py(px(6.))
                .rounded(px(6.))
                .bg(c.inset)
                .text_size(px(12.))
                .text_color(c.muted)
                .when(replace, |inherited| inherited.line_through())
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(c.faint)
                        .mb(px(2.))
                        .child(format!("Inherited from {from}")),
                )
                .child(text)
        });
        let replace_row = rule.map(|(ix, _)| {
            div().mx(px(-12.)).child(row(
                label(c, "Replace inherited", false),
                Vec::new(),
                switch(
                    c,
                    "replace-instructions",
                    replace,
                    false,
                    cx.listener(move |this, _, _, cx| {
                        this.update(
                            |settings| {
                                if let Some(rule) = settings.directories.get_mut(ix) {
                                    rule.replace_instructions = !replace;
                                }
                            },
                            cx,
                        )
                    }),
                ),
            ))
        });
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .px(px(12.))
            .py(px(7.))
            .child(header)
            .children(inherited_box)
            .child(
                div()
                    .h(px(84.))
                    .rounded(px(6.))
                    .bg(c.control)
                    .border(px(0.5))
                    .border_color(c.field_ring)
                    .overflow_hidden()
                    .child(self.instructions.clone()),
            )
            .children(replace_row)
            .into_any_element()
    }

    fn pi_panel(&self, resolved: &Resolved, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let c = &Colors::get(cx);
        let Some(reads) = &self.pi_reads else {
            return Vec::new();
        };
        let profile = self.profile_dir(resolved);
        let profile_name = self.profile_label(Some(&profile), cx);
        let mut rows = vec![
            row(
                div()
                    .flex()
                    .gap(px(4.))
                    .child("Runs pi with")
                    .child(mono(format!(
                        "PI_CODING_AGENT_DIR={}",
                        self.tilde(&profile)
                    ))),
                vec![description(
                    c,
                    format!(
                        "{profile_name} · {}",
                        self.source_name(resolved.profile.source, cx)
                    ),
                )],
                badge(c, "Profile", BadgeKind::Pi),
            )
            .into_any_element(),
        ];
        let (trust_text, trust_badge, trust_kind): (String, &str, BadgeKind) = match &reads.trust {
            Trust::NotNeeded => ("Nothing here needs pi's project trust".into(), "No trust needed", BadgeKind::Plain),
            Trust::Trusted { saved_for } => (
                format!("Trusted for {} in the profile's trust.json", self.tilde(saved_for)),
                "Trusted",
                BadgeKind::Pi,
            ),
            Trust::TrustedByDefault => (
                "The profile trusts every project (defaultProjectTrust: always)".into(),
                "Trusted",
                BadgeKind::Pi,
            ),
            Trust::Untrusted { saved_for } => (
                format!("Not trusted for {} in the profile's trust.json", self.tilde(saved_for)),
                "Not trusted",
                BadgeKind::Warning,
            ),
            Trust::NotDecided => (
                "pi hasn't been asked to trust this project; drafts can't ask, so pi skips its .pi/ folder".into(),
                "Not trusted",
                BadgeKind::Warning,
            ),
            Trust::Unreadable(err) => (err.clone(), "Unreadable", BadgeKind::Warning),
        };
        rows.push(
            row(
                "Project trust",
                vec![description(c, trust_text)],
                badge(c, trust_badge, trust_kind),
            )
            .into_any_element(),
        );
        if let Some(project) = &reads.project_settings {
            let detail = match &project.error {
                Some(err) => err.clone(),
                None => [
                    project.model.as_ref().map(|m| format!("model {m}")),
                    project.thinking.as_ref().map(|t| format!("thinking {t}")),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · "),
            };
            let (text, kind) = if reads.trust.loads_project() {
                ("Loaded", BadgeKind::Pi)
            } else {
                ("Skipped", BadgeKind::Plain)
            };
            rows.push(
                row(
                    mono(self.tilde(&project.path)),
                    (!detail.is_empty())
                        .then(|| description(c, detail))
                        .into_iter()
                        .collect(),
                    badge(c, text, kind),
                )
                .into_any_element(),
            );
        }
        let context_on = resolved.context_files.value;
        for file in &reads.context_files {
            rows.push(
                row(
                    mono(self.tilde(&file.path)),
                    vec![description(
                        c,
                        if file.from_profile {
                            "Context file from the profile"
                        } else {
                            "Context file"
                        },
                    )],
                    badge(
                        c,
                        if context_on { "Loaded" } else { "Off" },
                        if context_on {
                            BadgeKind::Pi
                        } else {
                            BadgeKind::Plain
                        },
                    ),
                )
                .into_any_element(),
            );
        }
        let mut panel = vec![
            group_title(c, "What pi reads here")
                .child(
                    div()
                        .ml_auto()
                        .font_weight(FontWeight::NORMAL)
                        .text_color(c.faint)
                        .child("read-only"),
                )
                .into_any_element(),
            group(c, rows).into_any_element(),
        ];
        if matches!(reads.trust, Trust::NotDecided | Trust::Untrusted { .. }) {
            panel.push(
                note(
                    c,
                    format!(
                        "Trust it once by running PI_CODING_AGENT_DIR={} pi in this directory. \
                     A running draft's pi reports the status it actually used.",
                        self.tilde(&profile)
                    ),
                )
                .into_any_element(),
            );
        }
        panel
    }

    fn general_page(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let c = &Colors::get(cx);
        let catalog = ProfileCatalog::global(cx).read(cx);
        let default_profile = self.profile_label(None, cx);
        let mut rows: Vec<AnyElement> = catalog
            .entries()
            .iter()
            .map(|entry| {
                let defaults = match &entry.defaults {
                    Ok(defaults) => match (&defaults.model, &defaults.thinking_level) {
                        (Some(model), Some(level)) => format!("Default {model}, {level} thinking"),
                        (Some(model), None) => format!("Default {model}"),
                        _ => "No default model".to_owned(),
                    },
                    Err(err) => err.to_string(),
                };
                let missing_default = entry.default_availability() == Some(Availability::Missing);
                let models = match &entry.models {
                    ModelList::Loading => "Listing models…".to_owned(),
                    ModelList::Failed(err) => err.to_string(),
                    ModelList::Loaded(models) => {
                        let mut providers: Vec<&str> =
                            models.iter().map(|m| m.provider.as_str()).collect();
                        providers.dedup();
                        format!("{} models from {}", models.len(), providers.join(", "))
                    }
                };
                row(
                    div()
                        .flex()
                        .gap(px(8.))
                        .items_center()
                        .child(entry.profile.name.clone())
                        .child(
                            div()
                                .text_color(c.muted)
                                .child(mono(self.tilde(&entry.profile.dir))),
                        ),
                    vec![
                        if missing_default {
                            warning(c, format!("{defaults}, which pi doesn't list"))
                        } else {
                            description(c, defaults)
                        },
                        description(c, models),
                    ],
                    div(),
                )
                .into_any_element()
            })
            .collect();
        rows.push(
            row(
                div()
                    .text_color(c.muted)
                    .child("Add a profile from another folder…"),
                Vec::new(),
                push_button(
                    c,
                    "add-profile",
                    "Choose…",
                    cx.listener(|this, _, _, cx| this.add_profile(cx)),
                ),
            )
            .into_any_element(),
        );
        let data_dir = draftmux_storage::data_dir().ok();
        let shell = shell_env::ShellEnv::global(cx);
        let tool = |name: &str, path: Option<PathBuf>, use_: &str| -> AnyElement {
            row(
                name.to_owned(),
                vec![match path {
                    Some(path) => div()
                        .mt(px(2.))
                        .flex()
                        .gap(px(4.))
                        .text_size(px(11.5))
                        .text_color(c.muted)
                        .child(div().text_color(c.ok).child("●"))
                        .child(mono(self.tilde(&path)))
                        .child(format!(", {use_}"))
                        .into_any_element(),
                    None => description(
                        c,
                        match &shell {
                            None => "Reading the login shell environment…".to_owned(),
                            Some(_) => "Not found on the login shell's PATH".to_owned(),
                        },
                    ),
                }],
                div(),
            )
            .into_any_element()
        };
        vec![
            group_title(c, "Tools").into_any_element(),
            group(c, vec![
                tool(
                    "pi",
                    shell
                        .as_ref()
                        .and_then(|env| env.pi().ok().map(Path::to_owned)),
                    "found through your login shell",
                ),
                tool(
                    "oxfmt",
                    shell
                        .as_ref()
                        .and_then(|env| env.oxfmt().map(Path::to_owned)),
                    "used by Format (⌘⇧F)",
                ),
            ])
            .into_any_element(),
            group_title(c, "pi profiles")
                .child(
                    div()
                        .ml_auto()
                        .font_weight(FontWeight::NORMAL)
                        .text_color(c.faint)
                        .child("found in ~/.pi*/agent"),
                )
                .into_any_element(),
            group(c, rows).into_any_element(),
            note(c, format!(
                "Defaults come from each profile's settings.json; models are what pi --list-models \
                 reports for it. A handoff keeps the profile its session file is stored in; \
                 others use the directory's pi profile setting, else {default_profile}. \
                 Draftmux never writes pi's files."
            ))
            .into_any_element(),
            group_title(c, "Drafts").into_any_element(),
            group(c, vec![
                row(
                    "Drafts and settings",
                    data_dir
                        .iter()
                        .map(|dir| description(c, self.tilde(dir)))
                        .collect(),
                    push_button(c,
                        "show-data",
                        "Show in Finder",
                        cx.listener(|_, _, _, cx| {
                            if let Ok(dir) = draftmux_storage::data_dir() {
                                cx.reveal_path(&dir);
                            }
                        }),
                    ),
                )
                .into_any_element(),
            ])
            .into_any_element(),
        ]
    }

    fn actions_page(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let c = &Colors::get(cx);
        let count = self.settings(cx).alternatives();
        vec![
            group_title(c, "Suggestions").into_any_element(),
            group(
                c,
                vec![
                    row(
                        label(c, "Alternatives", false),
                        vec![description(
                            c,
                            "How many suggestions each action offers, in every directory",
                        )],
                        self.popup(
                            Field::Alternatives,
                            count.to_string().into(),
                            false,
                            Some(64.),
                            cx,
                        ),
                    )
                    .into_any_element(),
                    row(
                        label(c, "Actions", false),
                        vec![description(
                            c,
                            "Alternatives, Shorter, Tighten, and an instruction of your own",
                        )],
                        div(),
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
            note(
                c,
                "Each action runs as a one-off pi call on the draft's session that leaves no \
                 session behind. Its model is the Selection model setting of the draft's \
                 directory.",
            )
            .into_any_element(),
        ]
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = &Colors::get(cx);
        let error = AppSettings::global(cx)
            .read(cx)
            .error
            .clone()
            .or(self.error.clone());
        let mut body: Vec<AnyElement> = Vec::new();
        if let Page::Rule(ix) = self.page {
            let count = self
                .settings(cx)
                .directories
                .get(ix)
                .map_or(0, Rule::override_count);
            body.push(
                div()
                    .mx(px(4.))
                    .mb(px(-4.))
                    .text_size(px(11.5))
                    .text_color(c.muted)
                    .child(match count {
                        0 => "Nothing overridden yet. Change any setting to override it for this directory.".to_owned(),
                        1 => "1 setting overridden here; everything else inherits.".to_owned(),
                        n => format!("{n} settings overridden here; everything else inherits."),
                    })
                    .into_any_element(),
            );
        }
        body.extend(match self.page {
            Page::General => self.general_page(cx),
            Page::Actions => self.actions_page(cx),
            Page::Defaults | Page::Rule(_) => {
                let own = scope(self.settings(cx), self.page)
                    .cloned()
                    .unwrap_or_default();
                let resolved = self.resolved(cx);
                let inherited = self.inherited(cx);
                let mut form = self.agent_form(&own, &resolved, inherited.as_ref(), cx);
                form.extend(self.pi_panel(&resolved, cx));
                form
            }
        });
        div()
            .size_full()
            .flex()
            .text_size(px(13.))
            .text_color(c.text)
            .key_context("Settings")
            .track_focus(&self.focus_handle)
            .on_action(|_: &CloseSettings, window, _| window.remove_window())
            .child(self.sidebar(cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .bg(c.window)
                    .child(self.header(cx))
                    .when_some(error, |main, error| {
                        main.child(
                            div()
                                .px(px(22.))
                                .py(px(6.))
                                .bg(c.warning_soft)
                                .text_size(px(12.))
                                .text_color(c.warning)
                                .child(error),
                        )
                    })
                    .child(
                        div()
                            .id(("settings-scroll", page_key(self.page)))
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .px(px(22.))
                                    .pt(px(2.))
                                    .pb(px(28.))
                                    .children(body),
                            ),
                    ),
            )
    }
}
