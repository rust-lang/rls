extern crate racer;

use analysis::{AnalysisHost, Span};
use self::racer::core::complete_from_file;
use self::racer::core::find_definition;
use self::racer::core;
use self::racer::scopes;

use std::fs::File;
use std::io::prelude::*;
use std::panic;
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use ide::{Input, Output, VscodeKind};

#[derive(Debug, Deserialize, Serialize)]
pub struct Position {
    pub filepath: String,
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Serialize)]
pub enum Provider {
    Rustw,
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

#[derive(Debug, Serialize)]
pub enum BuildResult {
    Success(Vec<String>),
    Failure(Vec<String>),
    Err
}

// Timeout = 0.5s (totally arbitrary).
const RUSTW_TIMEOUT: u64 = 500;

pub fn complete(source: Position) -> Vec<Completion> {
    use std::io::prelude::*;
    panic::catch_unwind(|| {
        let path = Path::new(&source.filepath);
        let mut f = File::open(&path).unwrap();
        let mut src = String::new();
        f.read_to_string(&mut src).unwrap();
        let pos = scopes::coords_to_point(&src, source.line, source.col);
        let cache = core::FileCache::new();
        let got = complete_from_file(&src,
                                     &path,
                                     pos,
                                     &core::Session::from_path(&cache, &path, &path));

        let mut results = vec![];
        for comp in got {
            results.push(Completion {
                name: comp.matchstr.clone(),
                context: comp.contextstr.clone(),
            });
        }
        results
    }).unwrap_or(vec![])
}

pub fn find_refs(source: Input, analysis: Arc<AnalysisHost>) -> Vec<Span> {
    let t = thread::current();
    let span = ::rustw_span(source.span);
    println!("title for: {:?}", span);
    let rustw_handle = thread::spawn(move || {
        let result = analysis.find_all_refs(&span);
        t.unpark();

        println!("rustw find_all_refs: {:?}", result);
        result
    });

    thread::park_timeout(Duration::from_millis(RUSTW_TIMEOUT));

    rustw_handle.join().ok().and_then(|t| t.ok()).unwrap_or(vec![]).into_iter().map(::adjust_span_for_vscode).collect()
}

pub fn goto_def(source: Input, analysis: Arc<AnalysisHost>) -> Output {
    // Rustw thread.
    let t = thread::current();
    let span = ::rustw_span(source.span);
    let rustw_handle = thread::spawn(move || {
        let result = if let Ok(s) = analysis.goto_def(&span) {
            println!("rustw success!");
            Some(Position {
                filepath: s.file_name,
                line: s.line_start,
                col: s.column_start,
            })
        } else {
            println!("rustw failed");
            None
        };

        t.unpark();

        result
    });

    // Racer thread.
    let pos = source.pos;
    let racer_handle = thread::spawn(move || {
        let path = Path::new(&pos.filepath);
        let mut f = File::open(&path).unwrap();
        let mut src = String::new();
        f.read_to_string(&mut src).unwrap();
        let pos = scopes::coords_to_point(&src, pos.line, pos.col);
        let cache = core::FileCache::new();
        if let Some(mch) = find_definition(&src,
                                        &path,
                                        pos,
                                        &core::Session::from_path(&cache, &path, &path)) {
            let mut f = File::open(&mch.filepath).unwrap();
            let mut source_src = String::new();
            f.read_to_string(&mut source_src).unwrap();
            if mch.point != 0 {
                let (line, col) = scopes::point_to_coords(&source_src, mch.point);
                let fpath = mch.filepath.to_str().unwrap().to_string();
                Some(Position {
                    filepath: fpath,
                    line: line,
                    col: col,
                })
            } else {
                None
            }
        } else {
            None
        }
    });

    thread::park_timeout(Duration::from_millis(RUSTW_TIMEOUT));

    let rustw_result = rustw_handle.join().unwrap_or(None);
    match rustw_result {
        Some(mut r) => {
            // FIXME Racer uses 0-indexed columns, rustw uses 1-indexed columns.
            // We should decide on which we want to use long-term.
            if r.col > 0 {
                r.col -= 1;
            }
            Output::Ok(r, Provider::Rustw)
        }
        None => {
            println!("Using racer");
            match racer_handle.join() {
                Ok(Some(r)) => Output::Ok(r, Provider::Racer),
                _ => Output::Err,
            }
        }
    }
}

pub fn title(source: Input, analysis: Arc<AnalysisHost>) -> Option<Title> {
    let t = thread::current();
    let span = ::rustw_span(source.span);
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

    thread::park_timeout(Duration::from_millis(RUSTW_TIMEOUT));

    rustw_handle.join().ok()
}

pub fn symbols(file_name: String, analysis: Arc<AnalysisHost>) -> Vec<Symbol> {
    let t = thread::current();
    let rustw_handle = thread::spawn(move || {
        let symbols = analysis.symbols(&file_name).unwrap_or(vec![]);
        t.unpark();

        symbols.into_iter().map(|s| {
            Symbol {
                name: s.name,
                kind: VscodeKind::from(s.kind),
                span: ::adjust_span_for_vscode(s.span),
            }
        }).collect()
    });

    thread::park_timeout(Duration::from_millis(RUSTW_TIMEOUT));

    rustw_handle.join().unwrap_or(vec![])
}

pub fn build(build_dir: &str) -> BuildResult {
    use std::env;
    use std::process::Command;

    let mut cmd = Command::new("cargo");
    cmd.arg("rustc");
    cmd.arg("--");
    cmd.arg("-Zno-trans");
    cmd.env("RUSTFLAGS", "-Zunstable-options -Zsave-analysis --error-format=json \
                          -Zcontinue-parse-after-error");
    cmd.env("RUSTC", &env::var("RLS_RUSTC").unwrap_or(String::new()));
    cmd.current_dir(build_dir);
    println!("building {}...", build_dir);
    match cmd.output() {
        Ok(x) => {
            let stderr_json_msg = convert_message_to_json_strings(x.stderr);
            match x.status.code() {
                Some(0) => {
                    BuildResult::Success(stderr_json_msg)
                }
                Some(_) => {
                    BuildResult::Failure(stderr_json_msg)
                }
                None => BuildResult::Err
            }
        }
        Err(_) => {
            BuildResult::Err
        }
    }
}

fn convert_message_to_json_strings(input: Vec<u8>) -> Vec<String> {
    let mut output = vec![];

    //FIXME: this is *so gross*  Trying to work around cargo not supporting json messages
    let it = input.into_iter();

    let mut read_iter = it.skip_while(|&x| x != b'{');

    let mut _msg = String::new();
    loop {
        match read_iter.next() {
            Some(b'\n') => {
                output.push(_msg);
                _msg = String::new();
                while let Some(res) = read_iter.next() {
                    if res == b'{' {
                        _msg.push('{');
                        break;
                    }
                }
            }
            Some(x) => {
                _msg.push(x as char);
            }
            None => {
                break;
            }
        }
    }

    output
}
