#![feature(custom_derive, plugin)]
#![plugin(serde_macros)]

extern crate tokio_service;
extern crate futures;
extern crate serde;
extern crate serde_json;

extern crate racer;
extern crate rls_analysis as analysis;

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

#[derive(Debug, Serialize)]
enum BuildResult {
    Success(Vec<String>),
    Failure(Vec<String>),
    Err
}

#[derive(Debug, Serialize)]
struct Title {
    ty: String,
    docs: String,
    doc_url: String,
}

#[derive(Debug, Serialize)]
struct Symbol {
    name: String,
    kind: VscodeKind,
    span: analysis::Span,
}

#[allow(dead_code)]
#[derive(Debug, Serialize)]
enum VscodeKind {
    File,
    Module,
    Namespace,
    Package,
    Class,
    Method,
    Property,
    Field,
    Constructor,
    Enum,
    Interface,
    Function,
    Variable,
    Constant,
    String,
    Number,
    Boolean,
    Array,
    Object,
    Key,
    Null
}

impl From<analysis::raw::DefKind> for VscodeKind {
    fn from(k: analysis::raw::DefKind) -> VscodeKind {
        match k {
            analysis::raw::DefKind::Enum => VscodeKind::Enum,
            analysis::raw::DefKind::Tuple => VscodeKind::Array,
            analysis::raw::DefKind::Struct => VscodeKind::Class,
            analysis::raw::DefKind::Trait => VscodeKind::Interface,
            analysis::raw::DefKind::Function => VscodeKind::Function,
            analysis::raw::DefKind::Method => VscodeKind::Function,
            analysis::raw::DefKind::Macro => VscodeKind::Function,
            analysis::raw::DefKind::Mod => VscodeKind::Module,
            analysis::raw::DefKind::Type => VscodeKind::Interface,
            analysis::raw::DefKind::Local => VscodeKind::Variable,
            analysis::raw::DefKind::Static => VscodeKind::Variable,
            analysis::raw::DefKind::Const => VscodeKind::Variable,
            analysis::raw::DefKind::Field => VscodeKind::Variable,
            analysis::raw::DefKind::Import => VscodeKind::Module,
        }
    }
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

fn title(source: Input, analysis: Arc<analysis::AnalysisHost>) -> Option<Title> {
    let t = thread::current();
    let span = rustw_span(source.span);
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

fn symbols(file_name: String, analysis: Arc<analysis::AnalysisHost>) -> Vec<Symbol> {
    let t = thread::current();
    let rustw_handle = thread::spawn(move || {
        let symbols = analysis.symbols(&file_name).unwrap_or(vec![]);
        t.unpark();

        symbols.into_iter().map(|s| {
            Symbol {
                name: s.name,
                kind: VscodeKind::from(s.kind),
                span: adjust_span_for_vscode(s.span),
            }
        }).collect()
    });

    thread::park_timeout(Duration::from_millis(RUSTW_TIMEOUT));

    rustw_handle.join().unwrap_or(vec![])
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

    fn symbols(&self, file_name: String, analysis: Arc<analysis::AnalysisHost>) -> Vec<u8> {
        let result = symbols(file_name, analysis);
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

fn parse_string(input: &[u8]) -> Result<String, serde_json::Error> {
    let s = String::from_utf8(input.to_vec()).unwrap();
    let s = s.replace("\\\"", "\"");
    //println!("decoding: '{}'", s);
    serde_json::from_str(&s)
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

fn build(build_dir: &str) -> BuildResult {
    use std::env;
    use std::process::Command;

    let mut cmd = Command::new("cargo");
    cmd.arg("build");
    cmd.env("RUSTFLAGS", "-Zunstable-options -Zsave-analysis --error-format=json \
                          -Zno-trans -Zcontinue-parse-after-error");
    cmd.current_dir(build_dir);
    println!("building...");
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

impl Service for MyService {
    type Request = http::Message<http::Request>;
    type Response = http::Message<http::Response>;
    type Error = http::Error;
    type Future = BoxFuture<Self::Response, http::Error>;

    fn call(&self, req: Self::Request) -> Self::Future {
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
                } else if x == "/symbols" {
                    if let Ok(file_name) = parse_string(req.body()) {
                        self.symbols(file_name, self.analysis.clone())
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
                    if let Ok(file_name) = parse_string(req.body()) {
                        let res = build(&file_name);
                        let reply = serde_json::to_string(&res).unwrap();
                        println!("build result: {:?}", res);
                        println!("Refreshing rustw cache");
                        self.analysis.reload(Path::new(&file_name).file_name().unwrap()
                            .to_str().unwrap()).unwrap();
                        //b"{}\n".to_vec()
                        let output = reply.as_bytes().to_vec();
                        output
                    } else {
                        b"{}\n".to_vec()
                    }
                } else if x == "/on_build" {
                    if let Ok(file_name) = parse_string(req.body()) {
                        println!("Refreshing rustw cache");
                        self.analysis.reload(Path::new(&file_name).file_name().unwrap()
                            .to_str().unwrap()).unwrap();
                    }
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

    fn poll_ready(&self) -> futures::Async<()> {
        futures::Async::Ready(())
    }
}

pub fn main() {
    let analysis = Arc::new(analysis::AnalysisHost::new(analysis::Target::Debug));
    //analysis.reload(PROJECT).unwrap();

    http::Server::new()
        .bind("127.0.0.1:9000".parse().unwrap())
        .serve(move || MyService { analysis: analysis.clone() })
        .unwrap();

    println!("Listening on 127.0.0.1:9000");

    thread::sleep(Duration::from_secs(1_000_000));
}
