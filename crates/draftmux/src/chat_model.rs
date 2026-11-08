//! The chat's model control: shows the model and thinking level pi resolved
//! for the draft's session and switches the model with a [`ModelPicker`].
//!
//! What it shows always comes from `get_state`: the owner passes the state of
//! each new pi process ([`AgentEvent::Ready`](draftmux_pi_rpc::AgentEvent)),
//! and a switch shows the state pi reports after it. A switch is a
//! `set_model`, which pi records in the session, so the draft keeps the
//! model across restarts.

use std::sync::Arc;

use draftmux_pi_profiles::ModelRef;
use draftmux_pi_rpc::{Agent, Error, State, ThinkingLevel};
use gpui::{
    Anchor, Context, Entity, FocusHandle, Focusable, MouseButton, SharedString, Subscription,
    Window, anchored, deferred, div, point, prelude::*, px,
};

use crate::model_picker::{ModelPicker, PickerEvent};
use crate::profiles::ModelList;
use crate::theme;

pub struct ChatModel {
    agent: Arc<Agent>,
    profile_name: SharedString,
    state: Option<State>,
    picker: Option<(Entity<ModelPicker>, Subscription)>,
    /// Focus to restore when the picker closes.
    previous_focus: Option<FocusHandle>,
    switching: bool,
    error: Option<SharedString>,
}

impl ChatModel {
    /// `profile_name` is the draft's pi profile, for the picker's messages.
    pub fn new(agent: Arc<Agent>, profile_name: impl Into<SharedString>) -> Self {
        Self {
            agent,
            profile_name: profile_name.into(),
            state: None,
            picker: None,
            previous_focus: None,
            switching: false,
            error: None,
        }
    }

    /// Shows a state pi reported, such as the one in `AgentEvent::Ready`.
    pub fn set_state(&mut self, state: State, cx: &mut Context<Self>) {
        self.state = Some(state);
        cx.notify();
    }

    fn show(&mut self, state: Result<State, String>, cx: &mut Context<Self>) {
        match state {
            Ok(state) => {
                self.state = Some(state);
                self.error = None;
            }
            Err(err) => self.error = Some(err.into()),
        }
        cx.notify();
    }

    fn current(&self) -> Option<ModelRef> {
        let model = self.state.as_ref()?.model.as_ref()?;
        Some(ModelRef::new(&model.provider, &model.id))
    }

    fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picker.is_some() || self.switching {
            return;
        }
        let current = self.current();
        let profile_name = self.profile_name.clone();
        let picker = cx.new(|cx| {
            let mut picker = ModelPicker::new(profile_name, ModelList::Loading, cx);
            picker.set_current(current, cx);
            picker
        });
        let subscription =
            cx.subscribe_in(&picker, window, |this, _, event, window, cx| match event {
                PickerEvent::Picked(model) => {
                    this.close(window, cx);
                    this.switch(model.clone(), cx);
                }
                PickerEvent::Dismissed => this.close(window, cx),
                PickerEvent::Retry => this.load_models(cx),
            });
        self.previous_focus = window.focused(cx);
        window.focus(&picker.focus_handle(cx), cx);
        self.picker = Some((picker, subscription));
        self.load_models(cx);
        cx.notify();
    }

    /// The draft's own pi process lists the models: that is the list
    /// `set_model` accepts, including providers from project config.
    fn load_models(&mut self, cx: &mut Context<Self>) {
        let Some((picker, _)) = &self.picker else {
            return;
        };
        let picker = picker.downgrade();
        picker
            .update(cx, |picker, cx| picker.set_models(ModelList::Loading, cx))
            .ok();
        let models = self.agent.get_available_models();
        cx.spawn(async move |_, cx| {
            let list = match models.await {
                Ok(models) => ModelList::Loaded(
                    models
                        .iter()
                        .map(|model| ModelRef::new(&model.provider, &model.id))
                        .collect(),
                ),
                Err(err) => ModelList::Failed(err.to_string().into()),
            };
            picker
                .update(cx, |picker, cx| picker.set_models(list, cx))
                .ok();
        })
        .detach();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        if let Some(focus) = self.previous_focus.take() {
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    fn switch(&mut self, model: ModelRef, cx: &mut Context<Self>) {
        if self.current().as_ref() == Some(&model) {
            return;
        }
        self.switching = true;
        self.error = None;
        let switch = self.agent.switch_model(model.provider, model.id);
        cx.spawn(async move |this, cx| {
            let state = switch.await;
            this.update(cx, |this, cx| {
                this.switching = false;
                match state {
                    // pi stopped after switching; the next process's
                    // `Ready` state shows the model.
                    Err(Error::Stopped) => cx.notify(),
                    state => {
                        this.show(state.map_err(|e| format!("Couldn’t switch model: {e}")), cx)
                    }
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn label(&self) -> SharedString {
        match &self.state {
            None => "π …".into(),
            Some(State { model: None, .. }) => "π no model".into(),
            Some(State {
                model: Some(model),
                thinking_level,
                ..
            }) => format!(
                "π {}/{} · thinking {}",
                model.provider,
                model.id,
                thinking_label(*thinking_level)
            )
            .into(),
        }
    }
}

fn thinking_label(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Off => "off",
        ThinkingLevel::Minimal => "minimal",
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High => "high",
        ThinkingLevel::Xhigh => "xhigh",
        ThinkingLevel::Max => "max",
    }
}

impl Render for ChatModel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = *theme::palette(cx);
        div()
            .relative()
            .flex()
            .gap_2()
            .min_w_0()
            .text_size(px(11.))
            .text_color(palette.muted)
            .child(
                div()
                    .id("chat-model")
                    .min_w_0()
                    .truncate()
                    .cursor_pointer()
                    .when(self.switching, |label| label.opacity(0.5))
                    .child(self.label())
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.open(window, cx)
                        }),
                    ),
            )
            .when_some(self.error.clone(), |row, error| {
                row.child(div().truncate().text_color(palette.danger).child(error))
            })
            .when_some(self.picker.as_ref(), |row, (picker, _)| {
                row.child(deferred(
                    anchored()
                        .anchor(Anchor::BottomLeft)
                        .offset(point(px(-8.), px(-6.)))
                        .child(picker.clone()),
                ))
            })
    }
}
