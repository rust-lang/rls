// Copyright 2016 The RLS Project Developers.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use data::config::Config;
use data::Analysis;
pub use data::{
    CratePreludeData, Def, DefKind, GlobalCrateId as CrateId, Import, Ref, Relation, RelationKind,
    SigElement, Signature, SpanData,
};
use listings::{DirectoryListing, ListingKind};
use {AnalysisLoader, Blacklist};

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

#[derive(Debug)]
pub struct Crate {
    pub id: CrateId,
    pub analysis: Analysis,
    pub timestamp: SystemTime,
    pub path: Option<PathBuf>,
    pub path_rewrite: Option<PathBuf>,
}

impl Crate {
    pub fn new(
        analysis: Analysis,
        timestamp: SystemTime,
        path: Option<PathBuf>,
        path_rewrite: Option<PathBuf>,
    ) -> Crate {
        Crate {
            id: analysis.prelude.as_ref().unwrap().crate_id.clone(),
            analysis,
            timestamp,
            path,
            path_rewrite,
        }
    }
}

/// Reads raw analysis data for non-blacklisted crates from files in directories
/// pointed by `loader`.
pub fn read_analysis_from_files<L: AnalysisLoader>(
    loader: &L,
    crate_timestamps: HashMap<PathBuf, SystemTime>,
    crate_blacklist: Blacklist,
) -> Vec<Crate> {
    let mut result = vec![];

    loader
        .search_directories()
        .iter()
        .inspect(|dir| trace!("Considering analysis files at {}", dir.path.display()))
        .filter_map(|dir| DirectoryListing::from_path(&dir.path).ok().map(|list| (dir, list)))
        .for_each(|(dir, listing)| {
            let t = Instant::now();

            for l in listing.files {
                info!("Considering {:?}", l);
                if let ListingKind::File(ref time) = l.kind {
                    if ignore_data(&l.name, crate_blacklist) {
                        continue;
                    }

                    let path = dir.path.join(&l.name);
                    let is_fresh = crate_timestamps.get(&path).map_or(true, |t| time > t);
                    if is_fresh {
                        if let Some(analysis) = read_crate_data(&path) {
                            result.push(Crate::new(
                                analysis,
                                *time,
                                Some(path),
                                dir.prefix_rewrite.clone(),
                            ));
                        };
                    }
                }
            }

            let d = t.elapsed();
            info!(
                "reading {} crates from {} in {}.{:09}s",
                result.len(),
                dir.path.display(),
                d.as_secs(),
                d.subsec_nanos()
            );
        });

    result
}

fn ignore_data(file_name: &str, crate_blacklist: Blacklist) -> bool {
    crate_blacklist.iter().any(|name| file_name.starts_with(&format!("lib{}-", name)))
}

fn read_file_contents(path: &Path) -> io::Result<String> {
    let mut file = File::open(&path)?;
    let mut buf = String::new();
    file.read_to_string(&mut buf)?;
    Ok(buf)
}

/// Attempts to read and deserialize `Analysis` data from a JSON file at `path`,
/// returns `Some(data)` on success.
fn read_crate_data(path: &Path) -> Option<Analysis> {
    trace!("read_crate_data {:?}", path);
    let t = Instant::now();

    let buf = read_file_contents(path)
        .or_else(|err| {
            warn!("couldn't read file: {}", err);
            Err(err)
        })
        .ok()?;
    let s = ::serde_json::from_str(&buf)
        .or_else(|err| {
            warn!("deserialisation error: {:?}", err);
            json::parse(&buf)
                .map(|parsed| {
                    if let json::JsonValue::Object(obj) = parsed {
                        let expected =
                            Some(json::JsonValue::from(Analysis::new(Config::default()).version));
                        let actual = obj.get("version").cloned();
                        if expected != actual {
                            warn!(
                                "Data file version mismatch; expected {:?} but got {:?}",
                                expected, actual
                            );
                        }
                    } else {
                        warn!("Data file didn't have a JSON object at the root");
                    }
                })
                .map_err(|err| {
                    warn!("Data file was not valid JSON: {:?}", err);
                })
                .ok();

            Err(err)
        })
        .ok()?;

    let d = t.elapsed();
    info!("reading {:?} {}.{:09}s", path, d.as_secs(), d.subsec_nanos());

    s
}

pub fn name_space_for_def_kind(dk: DefKind) -> char {
    match dk {
        DefKind::Enum
        | DefKind::Struct
        | DefKind::Union
        | DefKind::Type
        | DefKind::ExternType
        | DefKind::Trait => 't',
        DefKind::ForeignFunction
        | DefKind::ForeignStatic
        | DefKind::Function
        | DefKind::Method
        | DefKind::Mod
        | DefKind::Local
        | DefKind::Static
        | DefKind::Const
        | DefKind::Tuple
        | DefKind::TupleVariant
        | DefKind::StructVariant
        | DefKind::Field => 'v',
        DefKind::Macro => 'm',
    }
}
