extern crate tokio_service;
extern crate futures;
extern crate rustc_serialize;

extern crate racer;

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
use std::fs::{self, File};

use std::thread;
use std::time::Duration;

use rustc_serialize::json;

use std::panic;

#[derive(Debug, RustcDecodable, RustcEncodable)]
struct Position {
    filepath: String,
    line: usize,
    col: usize,
}

#[derive(Debug, RustcDecodable, RustcEncodable)]
struct Completion {
    name: String,
    context: String,
}

#[derive(Clone)]
struct MyService;

fn complete(source: Position) -> Vec<Completion> {
    use std::io::prelude::*;
    let result = panic::catch_unwind(|| {
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
    });
    if let Ok(output) = result {
        output
    } else {
        vec![]
    }
}

impl MyService {
    fn complete(&self, pos: Position) -> Vec<u8> {
        let completions = complete(pos);
        let reply = json::encode(&completions).unwrap();
        reply.as_bytes().to_vec()
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
                    let s = String::from_utf8(req.body().to_vec()).unwrap();
                    // FIXME: this is gross.  There should be a better way to unescape
                    let s = unsafe {
                        s.slice_unchecked(1, s.len()-1)
                    };
                    let s = s.replace("\\\"", "\"");
                    let pos: Position =
                        json::decode(&s).unwrap();
                    println!("Completion for: {:?}", pos);
                    self.complete(pos)
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
    http::Server::new()
        .bind("127.0.0.1:9000".parse().unwrap())
        .serve(|| MyService)
        .unwrap();

    println!("Listening on 127.0.0.1:9000");

    thread::sleep(Duration::from_secs(1_000_000));
}
