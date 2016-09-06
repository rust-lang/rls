#![feature(custom_derive, plugin)]
#![plugin(serde_macros)]

extern crate tokio_service;
extern crate futures;
extern crate serde;
extern crate serde_json;

extern crate racer;

extern crate rustw;

#[macro_use]
extern crate hyper;
extern crate tokio_hyper as http;

use racer::core::complete_from_file;
use racer::core::find_definition;
use racer::core;
use racer::scopes;

use tokio_service::Service;
use futures::{Future, finished, BoxFuture};
use std::path::*;
use std::fs::File;

use std::thread;
use std::time::Duration;
use std::io::prelude::*;

use rustw::analysis;
use std::sync::Arc;

use std::panic;

#[derive(Debug, Deserialize, Serialize)]
struct Position {
    filepath: String,
    line: usize,
    col: usize,
}

#[derive(Debug, Deserialize, Serialize)]
struct Completion {
    name: String,
    context: String,
}

#[derive(Debug, Deserialize)]
struct Input {
    pos: Position,
    span: analysis::Span,
}

#[derive(Debug, Serialize)]
enum Output {
    Ok(Position, Provider),
    Err,
}

#[derive(Debug, Serialize)]
enum Provider {
    Rustw,
    Racer,
}

#[derive(Clone)]
struct MyService {
    analysis: Arc<analysis::AnalysisHost>
}

fn complete(source: Position) -> Vec<Completion> {
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

// Timeout = 0.5s (totally arbitrary).
const RUSTW_TIMEOUT: u64 = 500;

fn find_refs(source: Input, analysis: Arc<analysis::AnalysisHost>) -> Vec<analysis::Span> {
    let t = thread::current();
    let span = rustw_span(source.span);
    println!("title for: {:?}", span);
    let rustw_handle = thread::spawn(move || {
        let result = analysis.find_all_refs(&span);
        t.unpark();

        println!("rustw find_all_refs: {:?}", result);
        result
    });

    thread::park_timeout(Duration::from_millis(RUSTW_TIMEOUT));

    rustw_handle.join().ok().and_then(|t| t.ok()).unwrap_or(vec![]).into_iter().map(adjust_span_for_vscode).collect()
}

fn goto_def(source: Input, analysis: Arc<analysis::AnalysisHost>) -> Output {
    // Rustw thread.
    let t = thread::current();
    let span = rustw_span(source.span);
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

fn title(source: Input, analysis: Arc<analysis::AnalysisHost>) -> Option<String> {
    let t = thread::current();
    let span = rustw_span(source.span);
    println!("title for: {:?}", span);
    let rustw_handle = thread::spawn(move || {
        let result = analysis.show_type(&span);
        t.unpark();

        println!("rustw show_type: {:?}", result);
        result
    });

    thread::park_timeout(Duration::from_millis(RUSTW_TIMEOUT));

    rustw_handle.join().ok().and_then(|t| t.ok())
}

// TODO overlap with VSCode plugin
fn rustw_span(mut source: analysis::Span) -> analysis::Span {
    source.column_start += 1;
    source.column_end += 1;
    source
}
fn adjust_span_for_vscode(mut source: analysis::Span) -> analysis::Span {
    source.column_start -= 1;
    source.column_end -= 1;
    source
}

impl MyService {
    fn complete(&self, pos: Position) -> Vec<u8> {
        let completions = complete(pos);
        let reply = serde_json::to_string(&completions).unwrap();
        reply.as_bytes().to_vec()
    }

    fn goto_def(&self, input: Input, analysis: Arc<analysis::AnalysisHost>) -> Vec<u8> {
        let result = goto_def(input, analysis);
        let reply = serde_json::to_string(&result).unwrap();
        reply.as_bytes().to_vec()
    }

    fn find_refs(&self, input: Input, analysis: Arc<analysis::AnalysisHost>) -> Vec<u8> {
        let result = find_refs(input, analysis);
        let reply = serde_json::to_string(&result).unwrap();
        reply.as_bytes().to_vec()
    }

    fn title(&self, input: Input, analysis: Arc<analysis::AnalysisHost>) -> Vec<u8> {
        let result = title(input, analysis);
        let reply = serde_json::to_string(&result).unwrap();
        reply.as_bytes().to_vec()
    }
}

fn parse_input_pos(input: &[u8]) -> Result<Input, serde_json::Error> {
    let s = String::from_utf8(input.to_vec()).unwrap();
    // FIXME: this is gross.  There should be a better way to unescape
    let s = unsafe {
        s.slice_unchecked(1, s.len()-1)
    };
    let s = s.replace("\\\"", "\"");
    //println!("decoding: '{}'", s);
    serde_json::from_str(&s)
}

// TODO so gross, so hard-wired
//const RUST_PATH: &'static str = "/home/ncameron/rust/x86_64-unknown-linux-gnu/stage2/bin";
const RUST_PATH: &'static str = "/Users/jturner/Source/rust/build/x86_64-apple-darwin/stage1/bin";
fn build() {
    use std::env;
    use std::process::Command;

    let mut cmd = Command::new("cargo");
    cmd.arg("build");
    cmd.env("RUSTFLAGS", "-Zunstable-options -Zsave-analysis -Zno-trans -Zcontinue-parse-after-error");
    cmd.env("PATH", &format!("{}:{}", RUST_PATH, env::var("PATH").unwrap()));
    cmd.current_dir("./sample_project_2");
    println!("building...");
    match cmd.output() {
        Ok(x) => println!("success: {:?}", x),
        Err(e) => println!("error: `{}`", e),
    }
}

impl Service for MyService {
    type Req = http::Message<http::Request>;
    type Resp = http::Message<http::Response>;
    type Error = http::Error;
    type Fut = BoxFuture<Self::Resp, http::Error>;

    fn call(&self, req: Self::Req) -> Self::Fut {
        let msg = match req.head().uri() {
            &hyper::uri::RequestUri::AbsolutePath { path: ref x, .. } => {
                if x == "/complete" {
                    if let Ok(input) = parse_input_pos(req.body()) {
                        println!("Completion for: {:?}", input.pos);
                        self.complete(input.pos)
                    } else {
                        println!("complete failed to parse");
                        b"{}\n".to_vec()
                    }
                } else if x == "/goto_def" {
                    if let Ok(input) = parse_input_pos(req.body()) {
                        println!("Goto def for: {:?}", input);
                        self.goto_def(input, self.analysis.clone())
                    } else {
                        b"{}\n".to_vec()
                    }
                } else if x == "/find_refs" {
                    if let Ok(input) = parse_input_pos(req.body()) {
                        println!("find refs for: {:?}", input);
                        self.find_refs(input, self.analysis.clone())
                    } else {
                        b"{}\n".to_vec()
                    }
                } else if x == "/title" {
                    if let Ok(input) = parse_input_pos(req.body()) {
                        self.title(input, self.analysis.clone())
                    } else {
                        b"{}\n".to_vec()
                    }
                } else if x == "/on_change" {
                    // TODO need to log this on a work queue and coalesce builds
                    build();
                    println!("Refreshing rustw cache");
                    self.analysis.reload().unwrap();
                    b"{}\n".to_vec()
                } else if x == "/on_build" {
                    println!("Refreshing rustw cache");
                    self.analysis.reload().unwrap();
                    b"{}\n".to_vec()
                } else {
                    b"{}\n".to_vec()
                }
            }
            _ => b"{}\n".to_vec(),
        };

        // Create the HTTP response with the body
        let resp = http::Message::new(http::Response::ok()).with_body(msg);

        // Return the response as an immediate future
        finished(resp).boxed()
    }
}

pub fn main() {
    let analysis = Arc::new(analysis::AnalysisHost::new("sample_project_2", analysis::Target::Debug));
    analysis.reload().unwrap();

    http::Server::new()
        .bind("127.0.0.1:9000".parse().unwrap())
        .serve(move || MyService { analysis: analysis.clone() })
        .unwrap();

    println!("Listening on 127.0.0.1:9000");

    thread::sleep(Duration::from_secs(1_000_000));
}
