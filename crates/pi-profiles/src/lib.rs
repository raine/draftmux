//! pi profiles: the pi config folders (`PI_CODING_AGENT_DIR`) Draftmux can
//! run pi with, their default model from `settings.json`, and the models pi
//! lists for them.
//!
//! Profiles are the `~/.pi*/agent` folders plus folders the user added,
//! which are kept in Draftmux's `profiles.json`. Draftmux reads only a
//! profile's `settings.json`; logins stay with pi, which reports the models
//! it has credentials for through `pi --list-models`.

mod models;
mod profiles;

pub use models::{Availability, ModelRef, availability, list_models};
pub use profiles::{AddedProfiles, Defaults, Profile, detect};
