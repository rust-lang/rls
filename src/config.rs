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

macro_rules! impl_enum_decodable {
    ( $e:ident, $( $x:ident ),* ) => {
        impl ::serde::Deserialize for $e {
            fn decode<D: ::serde::Deserializer>(d: &mut D) -> Result<Self, D::Error> {
                use std::ascii::AsciiExt;
                let s = try!(d.read_str());
                $(
                    if stringify!($x).eq_ignore_ascii_case(&s) {
                      return Ok($e::$x);
                    }
                )*
                Err(d.error("Bad variant"))
            }
        }

        impl ::std::str::FromStr for $e {
            type Err = &'static str;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                use std::ascii::AsciiExt;
                $(
                    if stringify!($x).eq_ignore_ascii_case(s) {
                        return Ok($e::$x);
                    }
                )*
                Err("Bad variant")
            }
        }

        impl ::config::ConfigType for $e {
            fn get_variant_names() -> String {
                let mut variants = Vec::new();
                $(
                    variants.push(stringify!($x));
                )*
                format!("[{}]", variants.join("|"))
            }
        }
    };
}

macro_rules! configuration_option_enum {
    ($e:ident: $( $x:ident ),+ $(,)*) => {
        #[derive(Copy, Clone, Eq, PartialEq, Debug)]
        pub enum $e {
            $( $x ),+
        }

        impl_enum_decodable!($e, $( $x ),+);
    }
}

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
        #[derive(RustcDecodable, Clone)]
        pub struct Config {
            $(pub $i: $ty),+
        }

        // Just like the Config struct but with each property wrapped
        // as Option<T>. This is used to parse a rustfmt.toml that doesn't
        // specity all properties of `Config`.
        // We first parse into `ParsedConfig`, then create a default `Config`
        // and overwrite the properties with corresponding values from `ParsedConfig`
        #[derive(RustcDecodable, Clone, Deserialize)]
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

            /// Attempt to read a confid from rls.toml in path, failing that use defaults.
            pub fn from_path(path: &Path) -> Config {
                let config_path = path.to_owned().join("rls.toml");
                let config_file = File::open(config_path);
                let mut toml = String::new();
                if let Ok(mut f) = config_file {
                    f.read_to_string(&mut toml).unwrap();
                }
                Config::from_toml(&toml)
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
    build_lib: bool, false, false, "cargo check --lib";
    cfg_test: bool, true, false, "build cfg(test) code";
    unstable_features: bool, false, false, "enable unstable features";
}
