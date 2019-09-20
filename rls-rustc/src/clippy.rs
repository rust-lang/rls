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
pub fn after_parse_callback(compiler: &rustc_interface::interface::Compiler) {
    use rustc_driver::plugin::registry::Registry;

    let sess = compiler.session();
    let mut registry = Registry::new(
        sess,
        compiler
            .parse()
            .expect(
                "at this compilation stage \
                 the crate must be parsed",
            )
            .peek()
            .span,
    );
    registry.args_hidden = Some(Vec::new());

    let conf = clippy_lints::read_conf(&registry);
    clippy_lints::register_plugins(&mut registry, &conf);

    let Registry {
        early_lint_passes, late_lint_passes, lint_groups, llvm_passes, attributes, ..
    } = registry;
    let mut ls = sess.lint_store.borrow_mut();
    for pass in early_lint_passes {
        ls.register_early_pass(Some(sess), true, false, pass);
    }
    for pass in late_lint_passes {
        ls.register_late_pass(Some(sess), true, false, false, pass);
    }

    for (name, (to, deprecated_name)) in lint_groups {
        ls.register_group(Some(sess), true, name, deprecated_name, to);
    }
    clippy_lints::register_pre_expansion_lints(sess, &mut ls, &conf);
    clippy_lints::register_renamed(&mut ls);

    sess.plugin_llvm_passes.borrow_mut().extend(llvm_passes);
    sess.plugin_attributes.borrow_mut().extend(attributes);
}
