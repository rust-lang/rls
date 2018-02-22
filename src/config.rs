// Copyright 2017 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Configuration for the workspace that RLS is operating within and options for
//! tweaking the RLS's behavior itself.

use build;

use std::env;
use std::fmt::Debug;
use std::io::sink;
use std::path::{Path, PathBuf};

use cargo::CargoResult;
use cargo::util::important_paths;
use cargo::core::{Shell, Workspace};

use failure;
use serde::de::{Deserialize, Deserializer};

#[cfg(feature = "rustfmt")]
use rustfmt::config::Config as RustfmtConfig;
#[cfg(feature = "rustfmt")]
use rustfmt::config::WriteMode;

const DEFAULT_WAIT_TO_BUILD: u64 = 1500;

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
    None,
}

// Deserialize as if it's `Option<T>` and use `None` variant if it's `None`,
// otherwise use `Specified` variant for deserialized value.
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Inferrable<T> {
    fn deserialize<D>(deserializer: D) -> Result<Inferrable<T>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Option::<T>::deserialize(deserializer)?;
        Ok(match value {
            None => Inferrable::None,
            Some(value) => Inferrable::Specified(value),
        })
    }
}

impl<T: Clone + Debug> Inferrable<T> {
    /// Combine these inferrable values, preferring our own specified values
    /// when possible, and falling back the given default value.
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

    /// Infer the given value if we don't already have an explicitly specified
    /// value.
    pub fn infer(&mut self, value: T) {
        if let Inferrable::Specified(_) = *self {
            trace!("Trying to infer {:?} on a {:?}", value, self);
            return;
        }

        *self = Inferrable::Inferred(value);
    }
}

impl<T> AsRef<T> for Inferrable<T> {
    fn as_ref(&self) -> &T {
        match *self {
            Inferrable::Inferred(ref value) | Inferrable::Specified(ref value) => value,
            // Default values should always be initialized as `Inferred` even
            // before actual inference takes place, `None` variant is only used
            // when deserializing and should not be read directly (via `as_ref`)
            Inferrable::None => unreachable!(),
        }
    }
}

/// RLS configuration options.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[allow(missing_docs)]
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
    /// Clear the RUST_LOG env variable before calling rustc/cargo? Default: true
    pub clear_env_rust_log: bool,
    /// Build the project only when a file got saved and not on file change. Default: false
    pub build_on_save: bool,
    pub use_crate_blacklist: bool,
    /// Cargo target dir. If set overrides the default one.
    #[serde(skip_deserializing, skip_serializing)]
    pub target_dir: Option<PathBuf>,
    pub features: Vec<String>,
    pub all_features: bool,
    pub no_default_features: bool,
    pub jobs: Option<u32>,
    pub all_targets: bool,
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
            workspace_mode: true,
            clear_env_rust_log: true,
            build_on_save: false,
            use_crate_blacklist: true,
            target_dir: None,
            features: vec![],
            all_features: false,
            no_default_features: false,
            jobs: None,
            all_targets: false,
        };
        result.normalise();
        result
    }
}

impl Config {
    /// Join this configuration with the new config.
    pub fn update(&mut self, mut new: Config) {
        new.build_lib = self.build_lib.combine_with_default(&new.build_lib, false);
        new.build_bin = self.build_bin.combine_with_default(&new.build_bin, None);

        // Ignore requests to disable workspace mode.
        self.workspace_mode = true;

        *self = new;
    }

    /// Ensures that unstable options are only allowed if `unstable_features` is
    /// true and that is not allowed on stable release channels.
    pub fn normalise(&mut self) {
        let allow_unstable = option_env!("CFG_RELEASE_CHANNEL")
            .map(|c| c == "nightly")
            .unwrap_or(true);

        if !allow_unstable {
            if self.unstable_features {
                eprintln!("`unstable_features` setting can only be used on nightly channel");
            }
            self.unstable_features = false;
        }

        if !self.unstable_features {
            // Force-set any unstable features here.
        }
    }

    /// Is this config incomplete, and needs additional values to be inferred?
    pub fn needs_inference(&self) -> bool {
        match (&self.build_lib, &self.build_bin) {
            (&Inferrable::None, _) | (_, &Inferrable::None) => true,
            _ => false,
        }
    }

    /// Auto-detect --lib/--bin switch if working under single package mode.
    pub fn infer_defaults(&mut self, project_dir: &Path) -> CargoResult<()> {
        if self.workspace_mode {
            return Ok(());
        }

        // Note that this may not be equal build_dir when inside a workspace member
        let manifest_path = important_paths::find_root_manifest_for_wd(None, project_dir)?;
        trace!("root manifest_path: {:?}", &manifest_path);

        // Cargo constructs relative paths from the manifest dir, so we have to pop "Cargo.toml"
        let manifest_dir = manifest_path.parent().unwrap();
        let shell = Shell::from_write(Box::new(sink()));
        let cwd = env::current_dir().expect("failed to get cwd");
        let cargo_config = build::make_cargo_config(manifest_dir, None, &cwd, shell);

        let ws = Workspace::new(&manifest_path, &cargo_config)?;
        let package = ws.current()?;

        trace!(
            "infer_config_defaults: Auto-detected `{}` package",
            package.name()
        );

        let targets = package.targets();
        let (lib, bin) = if targets.iter().any(|x| x.is_lib()) {
            (true, None)
        } else {
            let mut bins = targets.iter().filter(|x| x.is_bin());
            // No `lib` detected, but also can't find any `bin` target - there's
            // no sensible target here, so just Err out
            let first = bins.nth(0)
                .ok_or_else(|| failure::err_msg("No `bin` or `lib` targets in the package"))?;

            let mut bins = targets.iter().filter(|x| x.is_bin());
            let target = match bins.find(|x| x.src_path().ends_with("main.rs")) {
                Some(main_bin) => main_bin,
                None => first,
            };

            (false, Some(target.name().to_owned()))
        };

        trace!(
            "infer_config_defaults: build_lib: {:?}, build_bin: {:?}",
            lib,
            bin
        );

        // Unless crate target is explicitly specified, mark the values as
        // inferred, so they're not simply ovewritten on config change without
        // any specified value
        let (lib, bin) = match (&self.build_lib, &self.build_bin) {
            (&Inferrable::Specified(true), _) => (lib, None),
            (_, &Inferrable::Specified(Some(_))) => (false, bin),
            _ => (lib, bin),
        };

        self.build_lib.infer(lib);
        self.build_bin.infer(bin);

        Ok(())
    }
}

/// A rustfmt config (typically specified via rustfmt.toml)
/// The `FmtConfig` is not an exact translation of the config
/// rustfmt generates from the user's toml file, since when
/// using rustfmt with rls certain configuration options are
/// always used. See `FmtConfig::set_rls_options`
#[cfg(feature = "rustfmt")]
pub struct FmtConfig(RustfmtConfig);

#[cfg(feature = "rustfmt")]
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

#[cfg(feature = "rustfmt")]
impl Default for FmtConfig {
    fn default() -> FmtConfig {
        let config = RustfmtConfig::default();
        let mut config = FmtConfig(config);
        config.set_rls_options();
        config
    }
}
