// Copyright 2017 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use toml;

use std::fs::File;
use std::io::Read;
use std::path::Path;

// This trait and the following impl blocks are there so that we an use
// UCFS inside the get_docs() function on types for configs.
pub trait ConfigType {
    fn get_variant_names() -> String;
}

impl ConfigType for bool {
    fn get_variant_names() -> String {
        String::from("<boolean>")
    }
}

impl ConfigType for usize {
    fn get_variant_names() -> String {
        String::from("<unsigned integer>")
    }
}

impl ConfigType for String {
    fn get_variant_names() -> String {
        String::from("<string>")
    }
}

macro_rules! create_config {
    ($($i:ident: $ty:ty, $def:expr, $unstable:expr, $( $dstring:expr ),+ );+ $(;)*) => (
        #[derive(Clone)]
        pub struct Config {
            $(pub $i: $ty),+
        }

        // Just like the Config struct but with each property wrapped
        // as Option<T>. This is used to parse a rustfmt.toml that doesn't
        // specity all properties of `Config`.
        // We first parse into `ParsedConfig`, then create a default `Config`
        // and overwrite the properties with corresponding values from `ParsedConfig`
        #[derive(Clone, Deserialize)]
        pub struct ParsedConfig {
            $(pub $i: Option<$ty>),+
        }

        impl Config {

            fn fill_from_parsed_config(mut self, parsed: ParsedConfig) -> Config {
            $(
                if let Some(val) = parsed.$i {
                    self.$i = val;
                    // TODO error out if unstable
                }
            )+
                self
            }

            pub fn from_toml(toml: &str) -> Config {
                let parsed_config: ParsedConfig = match toml::from_str(toml) {
                    Ok(decoded) => decoded,
                    Err(e) => {
                        debug!("Decoding config file failed.");
                        debug!("Error: {}", e);
                        debug!("Config:\n{}", toml);
                        let parsed: toml::Value = toml.parse().expect("Could not parse TOML");
                        debug!("\n\nParsed:\n{:?}", parsed);
                        panic!();
                    }
                };
                Config::default().fill_from_parsed_config(parsed_config)
            }

            #[allow(dead_code)]
            pub fn print_docs() {
                use std::cmp;

                let max = 0;
                $( let max = cmp::max(max, stringify!($i).len()+1); )+
                let mut space_str = String::with_capacity(max);
                for _ in 0..max {
                    space_str.push(' ');
                }
                println!("Configuration Options:");
                $(
                    if !$unstable {
                        let name_raw = stringify!($i);
                        let mut name_out = String::with_capacity(max);
                        for _ in name_raw.len()..max-1 {
                            name_out.push(' ')
                        }
                        name_out.push_str(name_raw);
                        name_out.push(' ');
                        println!("{}{} Default: {:?}",
                                 name_out,
                                 <$ty>::get_variant_names(),
                                 $def);
                        $(
                            println!("{}{}", space_str, $dstring);
                        )+
                        println!("");
                    }
                )+
            }

            /// Attempt to read a config from .rls.toml, then rls.toml in path, failing that use
            /// defaults.
            pub fn from_path(path: &Path) -> Config {
                const CONFIG_FILE_NAMES: [&str; 2] = [".rls.toml", "rls.toml"];

                for config_file_name in &CONFIG_FILE_NAMES {
                    let config_path = path.to_owned().join(config_file_name);
                    let config_file = File::open(config_path);

                    if let Ok(mut f) = config_file {
                        let mut toml = String::new();
                        f.read_to_string(&mut toml).unwrap();
                        return Config::from_toml(&toml);
                    }
                }

                Config::default()
            }
        }

        // Template for the default configuration
        impl Default for Config {
            fn default() -> Config {
                Config {
                    $(
                        $i: $def,
                    )+
                }
            }
        }
    )
}

create_config! {
    sysroot: String, String::new(), false, "--sysroot";
    target: String, String::new(), false, "--target";
    rustflags: String, String::new(), false, "flags added to RUSTFLAGS";
    build_lib: bool, false, false, "cargo check --lib";
    cfg_test: bool, true, false, "build cfg(test) code";
    unstable_features: bool, false, false, "enable unstable features";
}
