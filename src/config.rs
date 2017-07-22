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

use rustfmt::config::Config as RustfmtConfig;
use rustfmt::config::WriteMode;

const DEFAULT_WAIT_TO_BUILD: u64 = 500;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Config {
    pub sysroot: Option<String>,
    pub target: Option<String>,
    pub rustflags: Option<String>,
    pub build_lib: bool,
    pub build_bin: Option<String>,
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
}

impl Config {
    pub fn default() -> Config {
        Config {
            sysroot: None,
            target: None,
            rustflags: None,
            build_lib: false,
            build_bin: None,
            cfg_test: false,
            unstable_features: false,
            wait_to_build: DEFAULT_WAIT_TO_BUILD,
            show_warnings: true,
            goto_def_racer_fallback: false,
            workspace_mode: false,
            analyze_package: None,
            clear_env_rust_log: true,
            build_on_save: false,
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
