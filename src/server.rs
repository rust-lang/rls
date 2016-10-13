// Copyright 2016 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use actions_http::*;
use build::*;
use ide::{ChangeInput, FmtOutput, Input, SaveInput, parse_string};
use vfs::Vfs;

use hyper;
use hyper::header::ContentType;
use hyper::net::Fresh;
use hyper::server::Handler;
use hyper::server::Request;
use hyper::server::Response;

use analysis::AnalysisHost;
use serde_json;

use std::io::Read;
use std::path::Path;
use std::sync::Arc;

pub fn run_server(analysis: Arc<AnalysisHost>, vfs: Arc<Vfs>, build_queue: Arc<BuildQueue>) {
    let handler = MyService {
        analysis: analysis.clone(),
        vfs: vfs.clone(),
        build_queue: build_queue.clone(),
    };

    println!("Listening on 127.0.0.1:9000");
    hyper::Server::http("127.0.0.1:9000").unwrap().handle(handler).unwrap();
}

#[derive(Clone)]
pub struct MyService {
    pub analysis: Arc<AnalysisHost>,
    pub vfs: Arc<Vfs>,
    pub build_queue: Arc<BuildQueue>,
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

macro_rules! dispatch_action_with_vfs {
    ($name: ident, $input_type: ty) => {
        fn $name(&self, input: $input_type, analysis: Arc<AnalysisHost>) -> Vec<u8> {
            let result = $name(input, analysis, self.vfs.clone());
            let reply = serde_json::to_string(&result).unwrap();
            reply.as_bytes().to_vec()
        }
    }
}

impl MyService {
    dispatch_action_with_vfs!(complete, Position);
    dispatch_action_with_vfs!(goto_def, Input);
    dispatch_action!(symbols, String);
    dispatch_action!(find_refs, Input);
    dispatch_action!(title, Input);

    fn fmt(&self, file_name: &str) -> Vec<u8> {
        let result = fmt(file_name, self.vfs.clone());
        if let FmtOutput::Change(ref s) = result {
            self.vfs.set_file(&Path::new(file_name), s);
        }
        let reply = serde_json::to_string(&result).unwrap();
        reply.as_bytes().to_vec()
    }

    fn build(&self, project_path: &str, priority: BuildPriority) -> Vec<u8> {
        let result = self.build_queue.request_build(project_path, priority);
        match result {
            BuildResult::Squashed => {
                println!("Skipped build");
                b"{}\n".to_vec()
            }
            BuildResult::Success(_) | BuildResult::Failure(_) => {
                let reply = serde_json::to_string(&result).unwrap();
                // println!("build result: {:?}", result);

                println!("Refreshing rustw cache: {}", project_path);
                self.analysis.reload(project_path).unwrap();

                reply.as_bytes().to_vec()
            }
            BuildResult::Err => b"{}\n".to_vec(),
        }
    }

    pub fn handle_action(&self, action: &str, body: &[u8]) -> Vec<u8> {
        if action == "/complete" {
            if let Ok(input) = Input::from_bytes(body) {
                // FIXME(#23) how do we get the changed files in memory to Racer?
                println!("Completion for: {:?}", input.pos);
                self.complete(input.pos, self.analysis.clone())
            } else {
                println!("complete failed to parse");
                b"{}\n".to_vec()
            }
        } else if action == "/goto_def" {
            if let Ok(input) = Input::from_bytes(body) {
                println!("Goto def for: {:?}", input);
                self.goto_def(input, self.analysis.clone())
            } else {
                b"{}\n".to_vec()
            }
        } else if action == "/symbols" {
            if let Ok(file_name) = parse_string(body) {
                self.symbols(file_name, self.analysis.clone())
            } else {
                b"{}\n".to_vec()
            }
        } else if action == "/find_refs" {
            if let Ok(input) = Input::from_bytes(body) {
                println!("find refs for: {:?}", input);
                self.find_refs(input, self.analysis.clone())
            } else {
                b"{}\n".to_vec()
            }
        } else if action == "/title" {
            if let Ok(input) = Input::from_bytes(body) {
                self.title(input, self.analysis.clone())
            } else {
                b"{}\n".to_vec()
            }
        } else if action == "/on_change" {
            if let Ok(change) = ChangeInput::from_bytes(body) {
                // println!("on change: {:?}", change);
                self.vfs.on_change(&change.changes);

                self.build(&change.project_path, BuildPriority::Normal)
            } else {
                b"{}\n".to_vec()
            }
        } else if action == "/on_save" {
            if let Ok(save) = SaveInput::from_bytes(body) {
                println!("on save: {}", &save.saved_file);
                self.vfs.on_save(&save.saved_file);

                self.build(&save.project_path, BuildPriority::Immediate)
            } else {
                b"{}\n".to_vec()
            }
        } else if action == "/on_build" {
            if let Ok(file_name) = parse_string(body) {
                println!("Refreshing rustw cache");
                self.analysis.reload(Path::new(&file_name).file_name().unwrap()
                    .to_str().unwrap()).unwrap();
            }
            b"{}\n".to_vec()
        } else if action == "/fmt" {
            if let Ok(file_name) = parse_string(body) {
                self.fmt(&file_name)
            } else {
                b"{}\n".to_vec()
            }
        } else {
            b"{}\n".to_vec()
        }        
    }
}

impl Handler for MyService {
    fn handle<'a, 'k>(&'a self, mut req: Request<'a, 'k>, mut res: Response<'a, Fresh>) {
        let mut body = vec![];
        req.read_to_end(&mut body).unwrap();

        let msg = match req.uri {
            hyper::uri::RequestUri::AbsolutePath(ref x) => self.handle_action(x, &body),
            _ => b"{}\n".to_vec(),
        };

        res.headers_mut().set(ContentType::json());
        res.send(&msg).unwrap();
    }
}
