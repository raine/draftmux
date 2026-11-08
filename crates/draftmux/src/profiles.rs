//! The pi profiles the app knows, with each profile's defaults and models.
//!
//! Detection and parsing live in `draftmux-pi-profiles`; this holds the
//! results in an entity views can observe. Model lists come from
//! `pi --list-models`, which takes a few seconds per profile, so they load
//! in the background after the login-shell environment is captured.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use draftmux_pi_profiles::{
    AddedProfiles, Availability, Defaults, ModelRef, Profile, availability, detect, list_models,
};
use gpui::{App, AppContext as _, Context, Entity, Global, SharedString, Task};
use shell_env::ShellEnv;

/// A profile's models as far as they are known.
#[derive(Debug, Clone)]
pub enum ModelList {
    Loading,
    Loaded(Arc<[ModelRef]>),
    Failed(SharedString),
}

impl ModelList {
    pub fn loaded(&self) -> Option<&[ModelRef]> {
        match self {
            ModelList::Loaded(models) => Some(models),
            _ => None,
        }
    }
}

pub struct ProfileEntry {
    pub profile: Profile,
    /// From the profile's `settings.json`; an error when it doesn't parse.
    pub defaults: Result<Defaults, SharedString>,
    pub models: ModelList,
    /// Dropping it discards a listing that a reload replaced.
    load: Option<Task<()>>,
}

impl ProfileEntry {
    /// Whether the profile has its own default model. `None` when
    /// `settings.json` names none.
    pub fn default_availability(&self) -> Option<Availability> {
        let model = self.defaults.as_ref().ok()?.model.as_ref()?;
        Some(availability(self.models.loaded(), model))
    }
}

pub struct ProfileCatalog {
    home: PathBuf,
    data_dir: PathBuf,
    added: AddedProfiles,
    entries: Vec<ProfileEntry>,
}

struct GlobalCatalog(Entity<ProfileCatalog>);

impl Global for GlobalCatalog {}

/// Detects the profiles and starts loading their models.
pub fn init(cx: &mut App) {
    let catalog = cx.new(|cx| {
        let home = std::env::home_dir().unwrap_or_default();
        let data_dir = draftmux_storage::data_dir().unwrap_or_else(|_| std::env::temp_dir());
        let added = AddedProfiles::new(&data_dir);
        let mut catalog = ProfileCatalog {
            home,
            data_dir,
            added,
            entries: Vec::new(),
        };
        catalog.detect(cx);
        catalog
    });
    cx.set_global(GlobalCatalog(catalog));
}

impl ProfileCatalog {
    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalCatalog>().0.clone()
    }

    pub fn entries(&self) -> &[ProfileEntry] {
        &self.entries
    }

    pub fn entry(&self, dir: &Path) -> Option<&ProfileEntry> {
        self.entries.iter().find(|entry| entry.profile.dir == dir)
    }

    /// Adds a profile folder (kept in `profiles.json`) and loads it.
    pub fn add(&mut self, dir: &Path, cx: &mut Context<Self>) -> Result<()> {
        self.added.add(dir, &self.home)?;
        self.detect(cx);
        Ok(())
    }

    /// Reads the profile's `settings.json` again and relists its models.
    pub fn reload(&mut self, dir: &Path, cx: &mut Context<Self>) {
        if let Some(ix) = self.entries.iter().position(|e| e.profile.dir == dir) {
            self.entries[ix].defaults = read_defaults(dir);
            self.load(ix, cx);
        }
    }

    /// Detects profiles again, keeping what is known about ones already
    /// listed and loading the new ones.
    fn detect(&mut self, cx: &mut Context<Self>) {
        let added = self.added.load().unwrap_or_else(|err| {
            eprintln!("draftmux: ignoring added pi profiles: {err:#}");
            Vec::new()
        });
        let mut old = std::mem::take(&mut self.entries);
        for profile in detect(&self.home, &added) {
            match old.iter().position(|e| e.profile.dir == profile.dir) {
                Some(ix) => self.entries.push(old.swap_remove(ix)),
                None => {
                    self.entries.push(ProfileEntry {
                        defaults: read_defaults(&profile.dir),
                        profile,
                        models: ModelList::Loading,
                        load: None,
                    });
                    self.load(self.entries.len() - 1, cx);
                }
            }
        }
        cx.notify();
    }

    fn load(&mut self, ix: usize, cx: &mut Context<Self>) {
        let dir = self.entries[ix].profile.dir.clone();
        let cwd = self.data_dir.clone();
        self.entries[ix].models = ModelList::Loading;
        self.entries[ix].load = Some(cx.spawn(async move |this, cx| {
            let env = cx.update(|cx| ShellEnv::ready(cx)).await;
            let listing = {
                let dir = dir.clone();
                cx.background_spawn(async move {
                    let pi = env.pi()?;
                    std::fs::create_dir_all(&cwd)
                        .with_context(|| format!("creating {}", cwd.display()))?;
                    list_models(env.command(pi), &dir, &cwd)
                })
            };
            let models = match listing.await {
                Ok(models) => ModelList::Loaded(models.into()),
                Err(err) => {
                    eprintln!("draftmux: listing models of {}: {err:#}", dir.display());
                    ModelList::Failed(format!("{err:#}").into())
                }
            };
            this.update(cx, |this, cx| {
                if let Some(entry) = this.entries.iter_mut().find(|e| e.profile.dir == dir) {
                    entry.models = models;
                    entry.load = None;
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }
}

fn read_defaults(dir: &Path) -> Result<Defaults, SharedString> {
    Defaults::read(dir).map_err(|err| format!("{err:#}").into())
}
