extern crate tokio_service;
extern crate tokio_hyper;

use actions::*;
use ide::{Input, parse_string};

use analysis::AnalysisHost;
use futures::{self, Future, finished, BoxFuture};
use hyper;
use serde_json;
use self::tokio_service::Service;

use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

pub fn run_server(analysis: Arc<AnalysisHost>) {
    tokio_hyper::Server::new()
        .bind("127.0.0.1:9000".parse().unwrap())
        .serve(move || MyService { analysis: analysis.clone() })
        .unwrap();

    println!("Listening on 127.0.0.1:9000");

    // TODO Why 100000 secs here?
    thread::sleep(Duration::from_secs(1_000_000));
}

#[derive(Clone)]
struct MyService {
    analysis: Arc<AnalysisHost>
}

macro_rules! dispatch_action {
    ($name: ident, $input_type: ty) => {
        fn $name(&self, input: $input_type, analysis: Arc<AnalysisHost>) -> Vec<u8> {
            let result = $name(input, analysis);
            let reply = serde_json::to_string(&result).unwrap();
            reply.as_bytes().to_vec()
        }
    }
}

impl MyService {
    dispatch_action!(complete, Position);
    dispatch_action!(goto_def, Input);
    dispatch_action!(symbols, String);
    dispatch_action!(find_refs, Input);
    dispatch_action!(title, Input);
}

impl Service for MyService {
    type Request = tokio_hyper::Message<tokio_hyper::Request>;
    type Response = tokio_hyper::Message<tokio_hyper::Response>;
    type Error = tokio_hyper::Error;
    type Future = BoxFuture<Self::Response, tokio_hyper::Error>;

    fn call(&self, req: Self::Request) -> Self::Future {
        let msg = match req.head().uri() {
            &hyper::uri::RequestUri::AbsolutePath { path: ref x, .. } => {
                if x == "/complete" {
                    if let Ok(input) = Input::from_bytes(req.body()) {
                        println!("Completion for: {:?}", input.pos);
                        self.complete(input.pos, self.analysis.clone())
                    } else {
                        println!("complete failed to parse");
                        b"{}\n".to_vec()
                    }
                } else if x == "/goto_def" {
                    if let Ok(input) = Input::from_bytes(req.body()) {
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
                    if let Ok(input) = Input::from_bytes(req.body()) {
                        println!("find refs for: {:?}", input);
                        self.find_refs(input, self.analysis.clone())
                    } else {
                        b"{}\n".to_vec()
                    }
                } else if x == "/title" {
                    if let Ok(input) = Input::from_bytes(req.body()) {
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
        let resp = tokio_hyper::Message::new(tokio_hyper::Response::ok()).with_body(msg);

        // Return the response as an immediate future
        finished(resp).boxed()
    }

    fn poll_ready(&self) -> futures::Async<()> {
        futures::Async::Ready(())
    }
}

