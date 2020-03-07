//! Configuration for the workspace that RLS is operating within and options for
//! tweaking the RLS's behavior itself.

use std::collections::HashMap;
use std::env;
use std::fmt;
use std::fmt::Debug;
use std::io::sink;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use cargo::core::{Shell, Workspace};
use cargo::util::{homedir, important_paths, Config as CargoConfig};
use cargo::CargoResult;

use serde::de::{Deserialize, Deserializer, Visitor};
use serde_derive::{Deserialize, Serialize};

use log::trace;

use rustfmt_nightly::Config as RustfmtConfig;
use rustfmt_nightly::{load_config, CliOptions, EmitMode, Verbosity};

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

impl<T> Inferrable<T> {
    pub fn is_none(&self) -> bool {
        match *self {
            Inferrable::None => true,
            _ => false,
        }
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

/// Returns whether unstable features are allowed.
///
/// It is very similar to what rustfmt uses [[1]] - it relies on
/// CFG_RELEASE_CHANNEL being set by Rust bootstrap.
/// In case the env var is missing, we assume that we're built by Cargo and are
/// using nightly since that's the only channel supported right now.
///
/// [1]: https://github.com/rust-lang/rustfmt/blob/dfa94d150555da40780413d7f1a1378565208c99/src/config/config_type.rs#L53-L67
pub fn unstable_features_allowed() -> bool {
    option_env!("CFG_RELEASE_CHANNEL").map_or(true, |c| c == "nightly" || c == "dev")
}

/// RLS configuration options.
#[derive(Clone, Debug, Deserialize)]
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
    pub wait_to_build: Option<u64>,
    pub show_warnings: bool,
    /// `true` to clear the `RUST_LOG` env variable before calling rustc/cargo.
    /// Default: `true`.
    pub clear_env_rust_log: bool,
    /// `true` to build the project only when a file got saved and not on file change.
    /// Default: `false`.
    pub build_on_save: bool,
    /// Blacklist of crates for RLS to skip. By default omits `winapi`, Unicode
    /// table crates, `serde`, `libc`, `glium` and other.
    pub crate_blacklist: Inferrable<CrateBlacklist>,
    /// The Cargo target directory. If set, overrides the default one.
    pub target_dir: Inferrable<Option<PathBuf>>,
    pub features: Vec<String>,
    pub all_features: bool,
    pub no_default_features: bool,
    pub jobs: Option<u32>,
    pub all_targets: bool,
    /// Enables use of Racer for `textDocument/completion` requests.
    ///
    /// Enabled also enables racer fallbacks for hover and go-to-definition functionality
    /// if rustc analysis should fail.
    pub racer_completion: bool,
    #[serde(deserialize_with = "deserialize_clippy_preference")]
    pub clippy_preference: ClippyPreference,
    /// Instructs cargo to enable full documentation extraction during save-analysis
    /// while building the crate. This has no effect on the pre-built standard library,
    /// which is built without full_docs enabled. Hover tooltips currently extract
    /// documentation from source due this limitation. The docs provided by the save-analysis
    /// are used in the event that source extraction fails. This may prove to be more useful
    /// in the future.
    pub full_docs: Inferrable<bool>,
    /// Show additional context in hover tooltips when available. This is often the type
    /// local variable declaration. When set to false, the content is only available when
    /// holding the `Ctrl` key in some editors.
    pub show_hover_context: bool,
    /// Use provided rustfmt binary instead of the statically linked one.
    /// (requires unstable features).
    pub rustfmt_path: Option<String>,
    /// EXPERIMENTAL (needs unstable features)
    /// If set, executes a given program responsible for rebuilding save-analysis
    /// to be loaded by the RLS. The program given should output a list of
    /// resulting JSON files on stdout.
    pub build_command: Option<String>,
    /// DEPRECATED: Use `crate_blacklist` instead.
    pub use_crate_blacklist: Option<bool>,
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
            wait_to_build: None,
            show_warnings: true,
            clear_env_rust_log: true,
            build_on_save: false,
            crate_blacklist: Inferrable::Inferred(CrateBlacklist::default()),
            target_dir: Inferrable::Inferred(None),
            features: vec![],
            all_features: false,
            no_default_features: false,
            jobs: None,
            all_targets: true,
            racer_completion: true,
            clippy_preference: ClippyPreference::default(),
            full_docs: Inferrable::Inferred(false),
            show_hover_context: true,
            rustfmt_path: None,
            build_command: None,
            use_crate_blacklist: None,
        };
        result.normalise();
        result
    }
}

lazy_static::lazy_static! {
    #[derive(Debug)]
    pub static ref DEPRECATED_OPTIONS: HashMap<&'static str, Option<&'static str>> = {
        [("use_crate_blacklist", Some("use `crate_blacklist` instead"))]
            .iter()
            .map(ToOwned::to_owned)
            .collect()
    };
}

impl Config {
    /// try to deserialize a Config from a json value, val is expected to be a
    /// Value::Object, all first level keys of val are converted to snake_case,
    /// duplicated and unknown keys are reported
    pub fn try_deserialize(
        val: &serde_json::value::Value,
        dups: &mut std::collections::HashMap<String, Vec<String>>,
        unknowns: &mut Vec<String>,
        deprecated: &mut Vec<String>,
    ) -> Result<Config, ()> {
        #[derive(Clone)]
        struct JsonValue(serde_json::value::Value);

        impl<'de> serde::de::IntoDeserializer<'de, serde_json::Error> for JsonValue {
            type Deserializer = serde_json::value::Value;
            fn into_deserializer(self) -> Self::Deserializer {
                self.0
            }
        }

        if let serde_json::Value::Object(map) = val {
            let seq = serde::de::value::MapDeserializer::new(map.iter().filter_map(|(k, v)| {
                use heck::SnakeCase;
                let snake_case = k.to_snake_case();
                let vec = dups.entry(snake_case.clone()).or_default();
                vec.push(k.to_string());

                if vec.len() == 1 {
                    if DEPRECATED_OPTIONS.contains_key(snake_case.as_str()) {
                        deprecated.push(snake_case.clone());
                    }

                    Some((snake_case, JsonValue(v.to_owned())))
                } else {
                    None
                }
            }));
            match serde_ignored::deserialize(seq, |path| unknowns.push(path.to_string())) {
                Ok(conf) => {
                    dups.retain(|_, v| v.len() > 1);
                    return Ok(conf);
                }
                _ => {
                    dups.retain(|_, v| v.len() > 1);
                }
            }
        }
        Err(())
    }

    /// Join this configuration with the new config.
    pub fn update(&mut self, mut new: Config) {
        macro_rules! combine_option_with_default {
            ($ident: ident, $val: expr) => {
                new.$ident = self.$ident.combine_with_default(&new.$ident, $val);
            };
        }

        new.normalise();
        combine_option_with_default!(target_dir, None);
        combine_option_with_default!(build_lib, false);
        combine_option_with_default!(build_bin, None);
        combine_option_with_default!(full_docs, false);
        combine_option_with_default!(crate_blacklist, CrateBlacklist::default());
        *self = new;
    }

    /// Ensures that unstable options are only allowed if `unstable_features` is
    /// true and that is not allowed on stable release channels.
    pub fn normalise(&mut self) {
        if !unstable_features_allowed() {
            if self.unstable_features {
                eprintln!("`unstable_features` setting can only be used on nightly channel");
            }
            self.unstable_features = false;
        }

        if !self.unstable_features {
            // Force-set any unstable features here.
            self.build_bin = Inferrable::Inferred(None);
            self.build_lib = Inferrable::Inferred(false);
            self.cfg_test = false;
            self.rustfmt_path = None;
            self.build_command = None;
        }
    }

    /// Checks if this config is incomplete, and needs additional values to be inferred.
    pub fn needs_inference(&self) -> bool {
        self.build_bin.is_none() || self.build_lib.is_none() || self.target_dir.is_none()
    }

    /// Tries to auto-detect certain option values if they were unspecified.
    /// Specifically, this:
    /// - detects correct `target/` build directory used by Cargo, if not specified.
    pub fn infer_defaults(&mut self, project_dir: &Path) -> CargoResult<()> {
        // Note that this may not be equal `build_dir` when inside a workspace member.
        let manifest_path = important_paths::find_root_manifest_for_wd(project_dir)?;
        trace!("root manifest_path: {:?}", &manifest_path);

        let shell = Shell::from_write(Box::new(sink()));
        let cwd = env::current_dir().expect("failed to get cwd");

        let config = CargoConfig::new(shell, cwd, homedir(project_dir).unwrap());

        let ws = Workspace::new(&manifest_path, &config)?;

        // Constructing a `Workspace` also probes the filesystem and detects where to place the
        // build artifacts. We need to rely on Cargo's behaviour directly not to possibly place our
        // own artifacts somewhere else (e.g., when analyzing only a single crate in a workspace).
        match self.target_dir {
            // We require an absolute path, so adjust a relative one if it's passed.
            Inferrable::Specified(Some(ref mut path)) if path.is_relative() => {
                *path = project_dir.join(&path);
            }
            _ => {}
        }
        if self.target_dir.as_ref().is_none() {
            let target_dir = ws.target_dir().into_path_unlocked();
            let target_dir = target_dir.join("rls");
            self.target_dir.infer(Some(target_dir));
            trace!(
                "For project path {:?} Cargo told us to use this target/ dir: {:?}",
                project_dir,
                self.target_dir.as_ref().as_ref().unwrap(),
            );
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ClippyPreference {
    /// Disable clippy.
    Off,
    /// Enable clippy, but `allow` clippy lints (i.e., require `warn` override).
    OptIn,
    /// Enable clippy.
    On,
}

impl Default for ClippyPreference {
    fn default() -> Self {
        ClippyPreference::OptIn
    }
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

impl ToString for ClippyPreference {
    fn to_string(&self) -> String {
        match self {
            ClippyPreference::Off => "off",
            ClippyPreference::OptIn => "optin",
            ClippyPreference::On => "on",
        }
        .to_string()
    }
}

/// Permissive custom deserialization for `ClippyPreference` using `FromStr`.
fn deserialize_clippy_preference<'de, T, D>(deserializer: D) -> Result<T, D::Error>
where
    T: Deserialize<'de> + FromStr<Err = ()>,
    D: Deserializer<'de>,
{
    struct ClippyPrefDeserializer<T>(PhantomData<fn() -> T>);

    impl<'de, T> Visitor<'de> for ClippyPrefDeserializer<T>
    where
        T: Deserialize<'de> + FromStr<Err = ()>,
    {
        type Value = T;
        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("`on`, `opt-in` or `off`")
        }
        fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<T, E> {
            FromStr::from_str(value)
                .map_err(|_| serde::de::Error::unknown_variant(value, &["on", "opt-in", "off"]))
        }
    }
    deserializer.deserialize_any(ClippyPrefDeserializer(PhantomData))
}

/// A Rustfmt config (typically specified via `rustfmt.toml`).
/// The `FmtConfig` is not an exact translation of the config
/// Rustfmt generates from the user's TOML file, since when
/// using Rustfmt with RLS, certain configuration options are
/// always used. See `FmtConfig::set_rls_options`.
pub struct FmtConfig(RustfmtConfig);

impl FmtConfig {
    /// Look for `.rustmt.toml` or `rustfmt.toml` in `path`, falling back
    /// to the default config if neither exists.
    pub fn from(path: &Path) -> FmtConfig {
        struct NullOptions;

        impl CliOptions for NullOptions {
            fn apply_to(self, _: &mut RustfmtConfig) {
                unreachable!();
            }
            fn config_path(&self) -> Option<&Path> {
                unreachable!();
            }
        }

        if let Ok((config, _)) = load_config::<NullOptions>(Some(path), None) {
            let mut config = FmtConfig(config);
            config.set_rls_options();
            return config;
        }
        FmtConfig::default()
    }

    /// Returns an immutable borrow of the config; will always
    /// have any relevant RLS specific options set.
    pub fn get_rustfmt_config(&self) -> &RustfmtConfig {
        &self.0
    }

    // Options that are always used when formatting with RLS.
    fn set_rls_options(&mut self) {
        self.0.set().skip_children(true);
        self.0.set().emit_mode(EmitMode::Stdout);
        self.0.set().verbose(Verbosity::Quiet);
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

#[derive(Clone, Debug, PartialEq)]
/// List of crates for which IDE analysis should not be generated
pub struct CrateBlacklist(pub Arc<[String]>);

impl<'de> Deserialize<'de> for CrateBlacklist {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let boxed = <Box<[String]> as serde::Deserialize>::deserialize(deserializer)?;
        Ok(CrateBlacklist(boxed.into()))
    }
}

impl Default for CrateBlacklist {
    fn default() -> Self {
        CrateBlacklist(
            [
                "cocoa",
                "gleam",
                "glium",
                "idna",
                "libc",
                "openssl",
                "rustc_serialize",
                "serde",
                "serde_json",
                "typenum",
                "unicode_normalization",
                "unicode_segmentation",
                "winapi",
            ]
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .into(),
        )
    }
}

#[test]
fn clippy_preference_from_str() {
    assert_eq!(ClippyPreference::from_str("Optin"), Ok(ClippyPreference::OptIn));
    assert_eq!(ClippyPreference::from_str("OFF"), Ok(ClippyPreference::Off));
    assert_eq!(ClippyPreference::from_str("opt-in"), Ok(ClippyPreference::OptIn));
    assert_eq!(ClippyPreference::from_str("on"), Ok(ClippyPreference::On));
}

#[test]
fn blacklist_default() {
    let value = serde_json::json!({});
    let config =
        Config::try_deserialize(&value, &mut Default::default(), &mut vec![], &mut vec![]).unwrap();
    assert_eq!(config.crate_blacklist.as_ref(), &CrateBlacklist::default());
    let value = serde_json::json!({"crate_blacklist": []});

    let config =
        Config::try_deserialize(&value, &mut Default::default(), &mut vec![], &mut vec![]).unwrap();
    assert_eq!(&*config.crate_blacklist.as_ref().0, &[] as &[String]);

    let value = serde_json::json!({"crate_blacklist": ["serde"]});
    let config =
        Config::try_deserialize(&value, &mut Default::default(), &mut vec![], &mut vec![]).unwrap();
    assert_eq!(&*config.crate_blacklist.as_ref().0, &["serde".to_string()]);
}
