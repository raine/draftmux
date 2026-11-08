//! The cascade: each field from the nearest matching rule that sets it, else
//! All directories, else the built-in default. Drafting instructions add up
//! instead: All directories first, then each rule outward in, unless a rule
//! replaces what it inherits.

use std::path::{Path, PathBuf};

use draftmux_pi_rpc::ModelOverride;
use draftmux_storage::{DraftSettings, ModelChoice, Target};

use crate::{DraftingModel, Scope, SelectionModel, Settings, expand_home};

/// Where a resolved value comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Builtin,
    AllDirectories,
    /// Index into [`Settings::directories`].
    Rule(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sourced<T> {
    pub value: T,
    pub source: Source,
}

/// Settings resolved for one directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// `None` leaves the profile to `draftmux open`'s fallback,
    /// `~/.pi/agent`. Expanded.
    pub profile: Sourced<Option<PathBuf>>,
    pub drafting_model: Sourced<DraftingModel>,
    pub selection_model: Sourced<SelectionModel>,
    pub target: Sourced<Target>,
    /// The innermost scope that contributed is the source.
    pub instructions: Sourced<Option<String>>,
    pub context_files: Sourced<bool>,
}

impl Settings {
    /// The settings for a draft whose working directory is `cwd`.
    pub fn resolve(&self, cwd: &Path, home: &Path) -> Resolved {
        self.resolve_rules(&self.matching_rules(cwd, home), home)
    }

    /// The All directories scope over the built-in defaults.
    pub fn resolve_defaults(&self, home: &Path) -> Resolved {
        self.resolve_rules(&[], home)
    }

    /// What rule `ix` inherits: the settings at its own directory without
    /// the rule itself.
    pub fn inherited(&self, ix: usize, home: &Path) -> Resolved {
        let mut rules = self.matching_rules(&self.directories[ix].path, home);
        rules.retain(|&r| r != ix);
        self.resolve_rules(&rules, home)
    }

    /// `rules` are indices into `directories`, most specific first.
    fn resolve_rules(&self, rules: &[usize], home: &Path) -> Resolved {
        let scopes: Vec<(Source, &Scope)> = rules
            .iter()
            .map(|&ix| (Source::Rule(ix), &self.directories[ix].scope))
            .chain([(Source::AllDirectories, &self.defaults)])
            .collect();
        fn pick<T: Clone>(
            scopes: &[(Source, &Scope)],
            field: impl Fn(&Scope) -> Option<&T>,
            builtin: T,
        ) -> Sourced<T> {
            scopes
                .iter()
                .find_map(|(source, scope)| {
                    field(scope).map(|value| Sourced {
                        value: value.clone(),
                        source: *source,
                    })
                })
                .unwrap_or(Sourced {
                    value: builtin,
                    source: Source::Builtin,
                })
        }
        let profile = pick(&scopes, |s| s.profile.as_ref(), PathBuf::new());
        Resolved {
            profile: Sourced {
                value: (profile.source != Source::Builtin)
                    .then(|| expand_home(&profile.value, home)),
                source: profile.source,
            },
            drafting_model: pick(
                &scopes,
                |s| s.drafting_model.as_ref(),
                DraftingModel::Session,
            ),
            selection_model: pick(
                &scopes,
                |s| s.selection_model.as_ref(),
                SelectionModel::Drafting,
            ),
            target: pick(&scopes, |s| s.target.as_ref(), Target::Github),
            instructions: self.instructions(rules),
            context_files: pick(&scopes, |s| s.context_files.as_ref(), true),
        }
    }

    fn instructions(&self, rules: &[usize]) -> Sourced<Option<String>> {
        let mut text: Option<String> = None;
        let mut source = Source::Builtin;
        if let Some(own) = &self.defaults.instructions {
            text = Some(own.clone());
            source = Source::AllDirectories;
        }
        for &ix in rules.iter().rev() {
            let rule = &self.directories[ix];
            let own = rule.scope.instructions.as_deref();
            if rule.replace_instructions {
                text = own.map(str::to_owned);
                source = Source::Rule(ix);
            } else if let Some(own) = own {
                text = Some(match text {
                    Some(before) => format!("{before}\n\n{own}"),
                    None => own.to_owned(),
                });
                source = Source::Rule(ix);
            }
        }
        Sourced {
            value: text,
            source,
        }
    }
}

impl Resolved {
    /// The model a new draft switches to; none continues on the session's.
    pub fn model_override(&self) -> Option<ModelOverride> {
        self.drafting_model.value.model().cloned()
    }

    /// What a new draft keeps.
    pub fn draft_settings(&self) -> DraftSettings {
        DraftSettings {
            instructions: self.instructions.value.clone(),
            context_files: self.context_files.value,
            selection_model: self.selection_model.value.model().map(|model| ModelChoice {
                provider: model.provider.clone(),
                model_id: model.model_id.clone(),
                thinking: model.thinking.as_str().to_owned(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use draftmux_pi_rpc::ThinkingLevel;

    use super::*;
    use crate::Rule;

    fn model(id: &str, thinking: ThinkingLevel) -> ModelOverride {
        ModelOverride {
            provider: "anthropic".into(),
            model_id: id.into(),
            thinking,
        }
    }

    /// `~/work` and `~/work/app` exist under a temp home.
    fn home() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().canonicalize().unwrap();
        fs::create_dir_all(home.join("work/app/src")).unwrap();
        (dir, home)
    }

    #[test]
    fn builtins_without_settings() {
        let (_t, home) = home();
        let resolved = Settings::default().resolve(&home.join("work"), &home);
        assert_eq!(resolved.profile.value, None);
        assert_eq!(resolved.drafting_model.value, DraftingModel::Session);
        assert_eq!(resolved.selection_model.value, SelectionModel::Drafting);
        assert_eq!(resolved.target.value, Target::Github);
        assert_eq!(resolved.instructions.value, None);
        assert!(resolved.context_files.value);
        assert_eq!(resolved.target.source, Source::Builtin);
        assert_eq!(resolved.model_override(), None);
        assert_eq!(resolved.draft_settings(), DraftSettings::default());
    }

    #[test]
    fn each_field_comes_from_the_nearest_scope_that_sets_it() {
        let (_t, home) = home();
        let mut settings = Settings::default();
        settings.defaults.target = Some(Target::Slack);
        settings.defaults.profile = Some("~/.pi/agent".into());
        settings.defaults.context_files = Some(false);
        let mut work = Rule::new("~/work".into());
        work.scope.profile = Some("~/.pi-work/agent".into());
        work.scope.drafting_model = Some(DraftingModel::Model(model("opus", ThinkingLevel::High)));
        let mut app = Rule::new("~/work/app".into());
        app.scope.target = Some(Target::Github);
        // An explicit "Same as session" overrides the outer rule's model.
        app.scope.drafting_model = Some(DraftingModel::Session);
        settings.directories = vec![app, work];

        let r = settings.resolve(&home.join("work/app/src"), &home);
        assert_eq!(
            r.target,
            Sourced {
                value: Target::Github,
                source: Source::Rule(0)
            }
        );
        assert_eq!(r.profile.value, Some(home.join(".pi-work/agent")));
        assert_eq!(r.profile.source, Source::Rule(1));
        assert_eq!(r.drafting_model.value, DraftingModel::Session);
        assert_eq!(r.drafting_model.source, Source::Rule(0));
        assert_eq!(
            r.context_files,
            Sourced {
                value: false,
                source: Source::AllDirectories
            }
        );

        let r = settings.resolve(&home.join("work"), &home);
        assert_eq!(r.target.source, Source::AllDirectories);
        assert_eq!(
            r.model_override(),
            Some(ModelOverride {
                provider: "anthropic".into(),
                model_id: "opus".into(),
                thinking: ThinkingLevel::High,
            })
        );

        let r = settings.resolve(&home, &home);
        assert_eq!(r.profile.value, Some(home.join(".pi/agent")));
        assert_eq!(r.target.value, Target::Slack);
    }

    #[test]
    fn inherited_skips_the_rule_itself() {
        let (_t, home) = home();
        let mut settings = Settings::default();
        settings.defaults.target = Some(Target::Slack);
        let mut work = Rule::new("~/work".into());
        work.scope.target = Some(Target::Github);
        let mut app = Rule::new("~/work/app".into());
        app.scope.target = Some(Target::Slack);
        settings.directories = vec![work, app];

        assert_eq!(settings.inherited(1, &home).target.source, Source::Rule(0));
        assert_eq!(
            settings.inherited(0, &home).target.source,
            Source::AllDirectories
        );
    }

    #[test]
    fn instructions_add_up_unless_replaced() {
        let (_t, home) = home();
        let mut settings = Settings::default();
        settings.defaults.instructions = Some("No emoji.".into());
        let mut work = Rule::new("~/work".into());
        work.scope.instructions = Some("Casual.".into());
        let app = Rule::new("~/work/app".into());
        settings.directories = vec![work, app];

        let at = |s: &Settings| s.resolve(&home.join("work/app"), &home).instructions;
        assert_eq!(
            at(&settings),
            Sourced {
                value: Some("No emoji.\n\nCasual.".into()),
                source: Source::Rule(0)
            }
        );

        settings.directories[1].scope.instructions = Some("Short.".into());
        settings.directories[1].replace_instructions = true;
        assert_eq!(at(&settings).value.as_deref(), Some("Short."));

        // Replacing with nothing clears them.
        settings.directories[1].scope.instructions = None;
        assert_eq!(
            at(&settings),
            Sourced {
                value: None,
                source: Source::Rule(1)
            }
        );

        // An outer replace still lets inner rules add.
        settings.directories[1].replace_instructions = false;
        settings.directories[1].scope.instructions = Some("Short.".into());
        settings.directories[0].replace_instructions = true;
        assert_eq!(at(&settings).value.as_deref(), Some("Casual.\n\nShort."));
    }

    #[test]
    fn draft_settings_keep_the_selection_model() {
        let (_t, home) = home();
        let mut settings = Settings::default();
        settings.defaults.selection_model =
            Some(SelectionModel::Model(model("haiku", ThinkingLevel::Xhigh)));
        settings.defaults.instructions = Some("Terse.".into());
        let kept = settings.resolve(&home, &home).draft_settings();
        assert_eq!(kept.instructions.as_deref(), Some("Terse."));
        assert_eq!(
            kept.selection_model,
            Some(ModelChoice {
                provider: "anthropic".into(),
                model_id: "haiku".into(),
                thinking: "xhigh".into(),
            })
        );
    }
}
