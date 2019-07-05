// Copyright 2017 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::path::Path;
use std::fmt::Debug;

use serde::de::{Deserialize, Deserializer};

use rustfmt::config::Config as RustfmtConfig;
use rustfmt::config::WriteMode;

const DEFAULT_WAIT_TO_BUILD: u64 = 500;

/// Some values in the config can be inferred without an explicit value set by
/// the user. There are no guarantees which values will or will not be passed
/// to the server, so we treat deserialized values effectively as `Option<T>`
/// and use `None` to mark the values as unspecified, otherwise we always use
/// `Specified` variant for the deserialized values. For user-provided `None`
/// values, they must be `Inferred` prior to usage (and can be further
/// `Specified` by the user).
#[derive(Clone, Debug, Serialize)]
pub enum Inferrable<T> {
    /// Explicitly specified value by the user. Retrieved by deserializing a
    /// non-`null` value. Can replace every other variant.
    Specified(T),
    /// Value that's inferred by the server. Can't replace a `Specified` variant.
    Inferred(T),
    /// Marker value that's retrieved when deserializing a user-specified `null`
    /// value. Can't be used alone and has to be replaced by server-`Inferred`
    /// or user-`Specified` value.
    None
}

// Deserialize as if it's `Option<T>` and use `None` variant if it's `None`,
// otherwise use `Specified` variant for deserialized value.
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Inferrable<T> {
    fn deserialize<D>(deserializer: D) -> Result<Inferrable<T>, D::Error>
        where D: Deserializer<'de>
    {
        let value = Option::<T>::deserialize(deserializer)?;
        Ok(match value {
            None => Inferrable::None,
            Some(value) => Inferrable::Specified(value),
        })
    }
}

impl<T: Clone + Debug> Inferrable<T> {
    pub fn combine_with_default(&self, new: &Self, default: T) -> Self {
        match (self, new) {
            // Don't allow to update a Specified value with an Inferred one
            (&Inferrable::Specified(_), &Inferrable::Inferred(_)) => self.clone(),
            // When trying to update with a `None`, use Inferred variant with
            // a specified default value, as `None` value can't be used directly
            (_, &Inferrable::None) => Inferrable::Inferred(default),
            _ => new.clone(),
        }
    }

    pub fn infer(&mut self, value: T) {
        if let &mut Inferrable::Specified(_) = self {
            trace!("Trying to infer {:?} on a {:?}", value, self);
            return;
        }

        *self = Inferrable::Inferred(value);
    }
}

impl<T> AsRef<T> for Inferrable<T> {
    fn as_ref(&self) -> &T {
        match *self {
            Inferrable::Inferred(ref value) |
            Inferrable::Specified(ref value) => value,
            // Default values should always be initialized as `Inferred` even
            // before actual inference takes place, `None` variant is only used
            // when deserializing and should not be read directly (via `as_ref`)
            Inferrable::None => unreachable!(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub sysroot: Option<String>,
    pub target: Option<String>,
    pub rustflags: Option<String>,
    pub build_lib: Inferrable<bool>,
    pub build_bin: Inferrable<Option<String>>,
    pub cfg_test: bool,
    pub unstable_features: bool,
    pub wait_to_build: u64,
    pub show_warnings: bool,
    pub goto_def_racer_fallback: bool,
    pub workspace_mode: bool,
    pub analyze_package: Option<String>,
    /// Clear the RUST_LOG env variable before calling rustc/cargo? Default: true
    pub clear_env_rust_log: bool,
    /// Build the project only when a file got saved and not on file change. Default: false
    pub build_on_save: bool,
    pub use_crate_blacklist: bool,
}

impl Default for Config {
    fn default() -> Config {
        let mut result = Config {
            sysroot: None,
            target: None,
            rustflags: None,
            build_lib: Inferrable::Inferred(false),
            build_bin: Inferrable::Inferred(None),
            cfg_test: false,
            unstable_features: false,
            wait_to_build: DEFAULT_WAIT_TO_BUILD,
            show_warnings: true,
            goto_def_racer_fallback: false,
            workspace_mode: false,
            analyze_package: None,
            clear_env_rust_log: true,
            build_on_save: false,
            use_crate_blacklist: true,
        };
        result.normalise();
        result
    }
}

impl Config {
    pub fn update(&mut self, mut new: Config) {
        new.build_lib = self.build_lib.combine_with_default(&new.build_lib, false);
        new.build_bin = self.build_bin.combine_with_default(&new.build_bin, None);

        *self = new;
    }

    /// Ensures that unstable options are only allowed if `unstable_features` is
    /// true and that is not allowed on stable release channels.
    pub fn normalise(&mut self) {
        let allow_unstable = option_env!("CFG_RELEASE_CHANNEL").map(|c| c == "nightly").unwrap_or(true);

        if !allow_unstable {
            if self.unstable_features {
                eprintln!("`unstable_features` setting can only be used on nightly channel");
            }
            self.unstable_features = false;
        }

        if !self.unstable_features {
            if self.workspace_mode {
                eprintln!("`workspace_mode` setting is unstable; ignored");
            }
            self.workspace_mode = false;
            self.analyze_package = None;
        }
    }

    pub fn needs_inference(&self) -> bool {
        match (&self.build_lib, &self.build_bin) {
            (&Inferrable::None, _) |
            (_, &Inferrable::None) => true,
            _ => false,
        }
    }
}

/// A rustfmt config (typically specified via rustfmt.toml)
/// The FmtConfig is not an exact translation of the config
/// rustfmt generates from the user's toml file, since when
/// using rustfmt with rls certain configuration options are
/// always used. See `FmtConfig::set_rls_options`
pub struct FmtConfig(RustfmtConfig);

impl FmtConfig {
    /// Look for `.rustmt.toml` or `rustfmt.toml` in `path`, falling back
    /// to the default config if neither exist
    pub fn from(path: &Path) -> FmtConfig {
        if let Ok((config, _)) = RustfmtConfig::from_resolved_toml_path(path) {
            let mut config = FmtConfig(config);
            config.set_rls_options();
            return config;
        }
        FmtConfig::default()
    }

    /// Return an immutable borrow of the config, will always
    /// have any relevant rls specific options set
    pub fn get_rustfmt_config(&self) -> &RustfmtConfig {
        &self.0
    }

    // options that are always used when formatting with rls
    fn set_rls_options(&mut self) {
        self.0.set().skip_children(true);
        self.0.set().write_mode(WriteMode::Plain);
    }
}

impl Default for FmtConfig {
    fn default() -> FmtConfig {
        let config = RustfmtConfig::default();
        let mut config = FmtConfig(config);
        config.set_rls_options();
        config
    }
}
