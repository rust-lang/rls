use crate::core::{BytePos, Coordinate, Match, MatchType, SearchType, Session, SessionExt};
use crate::matchers;
use crate::nameres::RUST_SRC_PATH;
use crate::project_model::Edition;
use std::path::{Path, PathBuf};

/// get crate file from current path & crate name
pub fn get_crate_file(name: &str, from_path: &Path, session: &Session<'_>) -> Option<PathBuf> {
    debug!("get_crate_file {}, {:?}", name, from_path);
    get_std_file(name, session).or_else(|| get_outer_crates(name, from_path, session))
}

pub fn get_std_file(name: &str, session: &Session<'_>) -> Option<PathBuf> {
    if let Some(ref std_path) = *RUST_SRC_PATH {
        // try lib<name>/lib.rs, like in the rust source dir
        let cratelibname = format!("lib{}", name);
        let filepath = std_path.join(cratelibname).join("lib.rs");
        if filepath.exists() || session.contains_file(&filepath) {
            return Some(filepath);
        }
        // If not found, try using the new standard library directory layout
        let filepath = std_path.join(name).join("src").join("lib.rs");
        if filepath.exists() || session.contains_file(&filepath) {
            return Some(filepath);
        }
    }
    return None;
}

/// 2018 style crate name resolution
pub fn search_crate_names(
    searchstr: &str,
    search_type: SearchType,
    file_path: &Path,
    only_2018: bool,
    session: &Session<'_>,
) -> Vec<Match> {
    let manifest_path = try_vec!(session.project_model.discover_project_manifest(file_path));
    if only_2018 {
        let edition = session
            .project_model
            .edition(&manifest_path)
            .unwrap_or(Edition::Ed2015);
        if edition < Edition::Ed2018 {
            return Vec::new();
        }
    }
    let hyphenated = searchstr.replace('_', "-");
    let searchstr = searchstr.to_owned();
    session
        .project_model
        .search_dependencies(
            &manifest_path,
            Box::new(move |libname| match search_type {
                SearchType::ExactMatch => libname == hyphenated || libname == searchstr,
                SearchType::StartsWith => {
                    libname.starts_with(&hyphenated) || libname.starts_with(&searchstr)
                }
            }),
        )
        .into_iter()
        .map(|(name, path)| {
            let name = name.replace('-', "_");
            let raw_src = session.load_raw_file(&path);
            Match {
                matchstr: name,
                filepath: path,
                point: BytePos::ZERO,
                coords: Some(Coordinate::start()),
                local: false,
                mtype: MatchType::Crate,
                contextstr: String::new(),
                docs: matchers::find_mod_doc(&raw_src, BytePos::ZERO),
            }
        })
        .collect()
}

/// get module file from current path & crate name
pub fn get_module_file(name: &str, parentdir: &Path, session: &Session<'_>) -> Option<PathBuf> {
    // try just <name>.rs
    let filepath = parentdir.join(format!("{}.rs", name));
    if filepath.exists() || session.contains_file(&filepath) {
        return Some(filepath);
    }
    // try <name>/mod.rs
    let filepath = parentdir.join(name).join("mod.rs");
    if filepath.exists() || session.contains_file(&filepath) {
        return Some(filepath);
    }
    None
}

/// try to get outer crates
/// if we have dependencies in cache, use it.
/// else, call cargo-metadata(default) or fall back to rls
fn get_outer_crates(libname: &str, from_path: &Path, session: &Session<'_>) -> Option<PathBuf> {
    debug!(
        "[get_outer_crates] lib name: {:?}, from_path: {:?}",
        libname, from_path
    );

    let manifest = session.project_model.discover_project_manifest(from_path)?;
    let res = session.project_model.resolve_dependency(&manifest, libname);
    res
}
