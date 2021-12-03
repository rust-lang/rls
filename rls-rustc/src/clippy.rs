//! Copied from rls/src/config.rs

use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ClippyPreference {
    /// Disable clippy.
    Off,
    /// Enable clippy, but `allow` clippy lints (i.e., require `warn` override).
    OptIn,
    /// Enable clippy.
    On,
}

pub fn preference() -> Option<ClippyPreference> {
    std::env::var("RLS_CLIPPY_PREFERENCE").ok().and_then(|pref| FromStr::from_str(&pref).ok())
}

/// Permissive deserialization for `ClippyPreference`
/// "opt-in", "Optin" -> `ClippyPreference::OptIn`
impl FromStr for ClippyPreference {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "off" => Ok(ClippyPreference::Off),
            "optin" | "opt-in" => Ok(ClippyPreference::OptIn),
            "on" => Ok(ClippyPreference::On),
            _ => Err(()),
        }
    }
}

pub fn adjust_args(args: Vec<String>, preference: ClippyPreference) -> Vec<String> {
    if preference != ClippyPreference::Off {
        // Allow feature gating in the same way as `cargo clippy`
        let mut clippy_args = vec!["--cfg".to_owned(), r#"feature="cargo-clippy""#.to_owned()];

        if preference == ClippyPreference::OptIn {
            // `OptIn`: Require explicit `#![warn(clippy::all)]` annotation in each workspace crate
            clippy_args.push("-A".to_owned());
            clippy_args.push("clippy::all".to_owned());
        }

        args.iter().map(ToOwned::to_owned).chain(clippy_args).collect()
    } else {
        args.to_owned()
    }
}

#[cfg(feature = "clippy")]
pub fn config(config: &mut rustc_interface::interface::Config) {
    let previous = config.register_lints.take();
    config.register_lints = Some(Box::new(move |sess, mut lint_store| {
        // technically we're ~guaranteed that this is none but might as well call anything that
        // is there already. Certainly it can't hurt.
        if let Some(previous) = &previous {
            (previous)(sess, lint_store);
        }

        let conf = clippy_lints::read_conf(&sess);
        clippy_lints::register_plugins(&mut lint_store, &sess, &conf);
        clippy_lints::register_pre_expansion_lints(&mut lint_store, &sess, &conf);
        clippy_lints::register_renamed(&mut lint_store);
    }));
}
