//! App-wide selection actions and their editable name and prompt.

use super::*;
use draftmux_settings::SelectionAction;

pub(super) struct ActionEditor {
    index: Option<usize>,
    saved: Vec<SelectionAction>,
    name: Entity<Editor>,
    prompt: Entity<Editor>,
}

impl SettingsWindow {
    fn edit_action(&mut self, index: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let saved = self.settings(cx).selection_actions();
        let name = theme::editor(instructions_style, cx);
        let prompt = theme::editor(instructions_style, cx);
        if let Some(action) = index.and_then(|index| saved.get(index)) {
            name.update(cx, |editor, cx| editor.set_text(&action.name, cx));
            prompt.update(cx, |editor, cx| editor.set_text(&action.prompt, cx));
        }
        window.focus(&name.focus_handle(cx), cx);
        self.action_editor = Some(ActionEditor {
            index,
            saved,
            name,
            prompt,
        });
        self.error = None;
        cx.notify();
    }

    fn save_action(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = &self.action_editor else {
            return;
        };
        let action = SelectionAction {
            name: edit.name.read(cx).text().trim().to_owned(),
            prompt: edit.prompt.read(cx).text().to_owned(),
        };
        if !action.is_valid() || action.name.contains(['\n', '\r']) {
            self.error = Some("Enter a name on one line and a nonempty prompt.".into());
            cx.notify();
            return;
        }
        let saved = edit.saved.clone();
        let index = edit.index;
        let mut conflict = false;
        self.update(
            |settings| {
                conflict = !save_action(settings, &saved, index, action);
            },
            cx,
        );
        if conflict {
            self.error = Some("Actions changed outside this editor. Copy your edits, cancel, and reopen the action.".into());
        } else {
            self.action_editor = None;
            self.error = None;
            window.focus(&self.focus_handle, cx);
        }
        cx.notify();
    }

    pub(super) fn action_rows(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let c = &Colors::get(cx);
        let actions = self.settings(cx).selection_actions();
        let mut rows = Vec::new();
        for (index, action) in actions.iter().enumerate() {
            let controls =
                div()
                    .flex()
                    .gap(px(6.))
                    .when(self.action_editor.is_none(), |controls| {
                        controls
                            .when(index > 0, |controls| {
                                controls.child(push_button(
                                    c,
                                    ("action-up", index),
                                    "↑",
                                    cx.listener(move |this, _, _, cx| {
                                        this.change_action(index, Some(index - 1), cx)
                                    }),
                                ))
                            })
                            .when(index + 1 < actions.len(), |controls| {
                                controls.child(push_button(
                                    c,
                                    ("action-down", index),
                                    "↓",
                                    cx.listener(move |this, _, _, cx| {
                                        this.change_action(index, Some(index + 1), cx)
                                    }),
                                ))
                            })
                            .child(push_button(
                                c,
                                ("action-edit", index),
                                "Edit…",
                                cx.listener(move |this, _, window, cx| {
                                    this.edit_action(Some(index), window, cx)
                                }),
                            ))
                            .child(push_button(
                                c,
                                ("action-delete", index),
                                "Delete",
                                cx.listener(move |this, _, _, cx| {
                                    this.change_action(index, None, cx)
                                }),
                            ))
                    });
            rows.push(
                row(
                    div().truncate().child(action.name.clone()),
                    vec![description(c, action.prompt.clone())],
                    controls,
                )
                .into_any_element(),
            );
        }
        if let Some(edit) = &self.action_editor {
            let field = |editor: &Entity<Editor>, height| {
                div()
                    .h(px(height))
                    .rounded(px(6.))
                    .bg(c.control)
                    .border(px(0.5))
                    .border_color(c.field_ring)
                    .overflow_hidden()
                    .child(editor.clone())
            };
            rows.push(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .px(px(12.))
                    .py(px(10.))
                    .child(label(c, "Name", false))
                    .child(field(&edit.name, 32.))
                    .child(label(c, "Prompt", false))
                    .child(field(&edit.prompt, 112.))
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap(px(6.))
                            .child(push_button(
                                c,
                                "cancel-action",
                                "Cancel",
                                cx.listener(|this, _, window, cx| {
                                    this.action_editor = None;
                                    this.error = None;
                                    window.focus(&this.focus_handle, cx);
                                    cx.notify();
                                }),
                            ))
                            .child(push_button(
                                c,
                                "save-action",
                                "Save",
                                cx.listener(|this, _, window, cx| this.save_action(window, cx)),
                            )),
                    )
                    .into_any_element(),
            );
        } else {
            rows.push(
                row(
                    push_button(
                        c,
                        "add-action",
                        "Add Action…",
                        cx.listener(|this, _, window, cx| this.edit_action(None, window, cx)),
                    ),
                    Vec::new(),
                    push_button(
                        c,
                        "restore-actions",
                        "Restore Defaults",
                        cx.listener(|this, _, _, cx| {
                            this.update(|settings| settings.selection_actions = None, cx);
                        }),
                    ),
                )
                .into_any_element(),
            );
        }
        rows
    }

    fn change_action(&mut self, index: usize, destination: Option<usize>, cx: &mut Context<Self>) {
        self.update(
            |settings| {
                let mut actions = settings.selection_actions();
                if index >= actions.len() {
                    return;
                }
                match destination {
                    Some(destination) if destination < actions.len() => {
                        actions.swap(index, destination)
                    }
                    Some(_) => return,
                    None => {
                        actions.remove(index);
                    }
                }
                settings.selection_actions = Some(actions);
            },
            cx,
        );
    }
}

fn save_action(
    settings: &mut Settings,
    saved: &[SelectionAction],
    index: Option<usize>,
    action: SelectionAction,
) -> bool {
    let mut actions = settings.selection_actions();
    if actions != saved {
        return false;
    }
    match index {
        Some(index) => {
            let Some(current) = actions.get_mut(index) else {
                return false;
            };
            *current = action;
        }
        None => actions.push(action),
    }
    settings.selection_actions = Some(actions);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom() -> SelectionAction {
        SelectionAction {
            name: "Friendly".into(),
            prompt: "Use a warmer tone.".into(),
        }
    }

    #[test]
    fn save_edits_defaults_and_appends_to_an_empty_list() {
        let mut settings = Settings::default();
        let saved = settings.selection_actions();
        assert!(save_action(&mut settings, &saved, Some(0), custom()));
        assert_eq!(
            settings.selection_actions(),
            vec![custom(), saved[1].clone()]
        );
        settings.selection_actions = Some(Vec::new());
        assert!(save_action(&mut settings, &[], None, custom()));
        assert_eq!(settings.selection_actions(), vec![custom()]);
    }

    #[test]
    fn save_rejects_changed_actions_without_overwriting_settings() {
        let mut settings = Settings::default();
        let saved = settings.selection_actions();
        settings.selection_actions = Some(vec![saved[1].clone(), saved[0].clone()]);
        let before = settings.clone();
        assert!(!save_action(&mut settings, &saved, Some(0), custom()));
        assert_eq!(settings, before);
        settings.selection_actions = Some(Vec::new());
        assert!(!save_action(&mut settings, &saved, Some(1), custom()));
        assert!(settings.selection_actions().is_empty());
    }

    #[test]
    fn unrelated_settings_changes_do_not_block_save() {
        let mut settings = Settings::default();
        let saved = settings.selection_actions();
        settings.alternatives = Some(5);
        assert!(save_action(&mut settings, &saved, None, custom()));
        assert_eq!(settings.alternatives(), 5);
        assert_eq!(settings.selection_actions().last(), Some(&custom()));
    }
}
