// Copyright 2016 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.


use analysis::{AnalysisHost, Span};
use racer::core::complete_from_file;
use racer::core::find_definition;
use racer::core;
use rustfmt::{Input as FmtInput, format_input};
use rustfmt::config::{self, WriteMode};

use std::default::Default;
use std::panic;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use ide::{Input, Output, FmtOutput, VscodeKind};
use vfs::Vfs;

#[derive(Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct Position {
    pub filepath: PathBuf,
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Serialize, Eq, PartialEq, Deserialize)]
pub enum Provider {
    Compiler,
    Racer,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Completion {
    pub name: String,
    pub context: String,
}

#[derive(Debug, Serialize)]
pub struct Title {
    pub ty: String,
    pub docs: String,
    pub doc_url: String,
}

#[derive(Debug, Serialize)]
pub struct Symbol {
    pub name: String,
    pub kind: VscodeKind,
    pub span: Span,
}

pub fn complete(pos: Position, _analysis: Arc<AnalysisHost>, vfs: Arc<Vfs>) -> Vec<Completion> {
    let vfs: &Vfs = &vfs;
    panic::catch_unwind(|| {
        let pos = adjust_vscode_pos_for_racer(pos);
        let file_path = &Path::new(&pos.filepath);

        let cache = core::FileCache::new();
        let session = core::Session::from_path(&cache, file_path, file_path);
        for (path, txt) in vfs.get_cached_files() {
            session.cache_file_contents(&path, txt);
        }

        let src = session.load_file(file_path);

        let pos = session.load_file(file_path).coords_to_point(pos.line, pos.col).unwrap();
        let results = complete_from_file(&src.code, file_path, pos, &session);

        results.map(|comp| Completion {
            name: comp.matchstr.clone(),
            context: comp.contextstr.clone(),
        }).collect()
    }).unwrap_or(vec![])
}

pub fn find_refs(source: Input, analysis: Arc<AnalysisHost>) -> Vec<Span> {
    let t = thread::current();
    let span = source.span;
    println!("title for: {:?}", span);
    let rustw_handle = thread::spawn(move || {
        let result = analysis.find_all_refs(&span, true);
        t.unpark();

        println!("rustw find_all_refs: {:?}", result);
        result
    });

    thread::park_timeout(Duration::from_millis(::COMPILER_TIMEOUT));

    rustw_handle.join().ok().and_then(|t| t.ok()).unwrap_or(vec![])
}

pub fn fmt(file_name: &Path, vfs: Arc<Vfs>) -> FmtOutput {
    let path = PathBuf::from(file_name);
    let input = match vfs.load_file(&path) {
        Ok(s) => FmtInput::Text(s),
        Err(_) => return FmtOutput::Err,
    };

    let mut config = config::Config::default();
    config.skip_children = true;
    config.write_mode = WriteMode::Plain;

    let mut buf = Vec::<u8>::new();
    // TODO save change back to VFS
    match format_input(input, &config, Some(&mut buf)) {
        Ok(_) => FmtOutput::Change(String::from_utf8(buf).unwrap()),
        Err(_) => FmtOutput::Err,
    }
}

pub fn goto_def(source: Input, analysis: Arc<AnalysisHost>, vfs: Arc<Vfs>) -> Output {
    // Save-analysis thread.
    let t = thread::current();
    let span = source.span;
    let compiler_handle = thread::spawn(move || {
        let result = if let Ok(s) = analysis.goto_def(&span) {
            println!("compiler success!");
            Some(Position {
                filepath: s.file_name,
                line: s.line_start,
                col: s.column_start,
            })
        } else {
            println!("compiler failed");
            None
        };

        t.unpark();

        result
    });

    // Racer thread.
    let pos = adjust_vscode_pos_for_racer(source.pos);
    let racer_handle = thread::spawn(move || {
        let file_path = &Path::new(&pos.filepath);

        let cache = core::FileCache::new();
        let session = core::Session::from_path(&cache, file_path, file_path);
        for (path, txt) in vfs.get_cached_files() {
            session.cache_file_contents(&path, txt);
        }

        let src = session.load_file(file_path);

        find_definition(&src.code,
                        file_path,
                        src.coords_to_point(pos.line, pos.col).unwrap(),
                        &session)
            .and_then(|mtch| {
                let source_path = &mtch.filepath;
                if mtch.point != 0 {
                    let (line, col) = session.load_file(source_path)
                                             .point_to_coords(mtch.point)
                                             .unwrap();
                    let fpath = source_path.to_owned();
                    Some(Position {
                        filepath: fpath,
                        line: line,
                        col: col,
                    })
                } else {
                    None
                }
            })
    });

    thread::park_timeout(Duration::from_millis(::COMPILER_TIMEOUT));

    let compiler_result = compiler_handle.join().unwrap_or(None);
    match compiler_result {
        Some(r) => Output::Ok(r, Provider::Compiler),
        None => {
            println!("Using racer");
            match racer_handle.join() {
                Ok(Some(r)) => {
                    Output::Ok(adjust_racer_pos_for_vscode(r), Provider::Racer)
                }
                _ => Output::Err,
            }
        }
    }
}

pub fn title(source: Input, analysis: Arc<AnalysisHost>) -> Option<Title> {
    let t = thread::current();
    let span = source.span;
    println!("title for: {:?}", span);
    let rustw_handle = thread::spawn(move || {
        let ty = analysis.show_type(&span).unwrap_or(String::new());
        let docs = analysis.docs(&span).unwrap_or(String::new());
        let doc_url = analysis.doc_url(&span).unwrap_or(String::new());
        t.unpark();

        println!("rustw show_type: {:?}", ty);
        println!("rustw docs: {:?}", docs);
        println!("rustw doc url: {:?}", doc_url);
        Title {
            ty: ty,
            docs: docs,
            doc_url: doc_url,
        }
    });

    thread::park_timeout(Duration::from_millis(::COMPILER_TIMEOUT));

    rustw_handle.join().ok()
}

pub fn symbols(file_name: PathBuf, analysis: Arc<AnalysisHost>) -> Vec<Symbol> {
    let t = thread::current();
    let rustw_handle = thread::spawn(move || {
        let symbols = analysis.symbols(&file_name).unwrap_or(vec![]);
        t.unpark();

        symbols.into_iter().map(|s| {
            Symbol {
                name: s.name,
                kind: VscodeKind::from(s.kind),
                span: s.span,
            }
        }).collect()
    });

    thread::park_timeout(Duration::from_millis(::COMPILER_TIMEOUT));

    rustw_handle.join().unwrap_or(vec![])
}


fn adjust_vscode_pos_for_racer(mut source: Position) -> Position {
    source.line += 1;
    source
}

fn adjust_racer_pos_for_vscode(mut source: Position) -> Position {
    if source.line > 0 {
        source.line -= 1;
    }
    source
}
