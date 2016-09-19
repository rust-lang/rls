use actions::*;
use build::*;
use ide::{ChangeInput, Input, SaveInput, parse_string};
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

pub fn run_server(analysis: Arc<AnalysisHost>, vfs: Arc<Vfs>) {
    let handler = MyService {
        analysis: analysis.clone(),
        vfs: vfs.clone(),
    };

    println!("Listening on 127.0.0.1:9000");
    hyper::Server::http("127.0.0.1:9000").unwrap().handle(handler).unwrap();
}

#[derive(Clone)]
struct MyService {
    analysis: Arc<AnalysisHost>,
    vfs: Arc<Vfs>,
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

impl Handler for MyService {
    fn handle<'a, 'k>(&'a self, mut req: Request<'a, 'k>, mut res: Response<'a, Fresh>) {
        let mut body = vec![];
        req.read_to_end(&mut body).unwrap();

        let msg = match req.uri {
            hyper::uri::RequestUri::AbsolutePath(ref x) => {
                if x == "/complete" {
                    if let Ok(input) = Input::from_bytes(&body) {
                        // TODO the client has done a save so we should exectute the on_save logic here too.
                        println!("Completion for: {:?}", input.pos);
                        self.complete(input.pos, self.analysis.clone())
                    } else {
                        println!("complete failed to parse");
                        b"{}\n".to_vec()
                    }
                } else if x == "/goto_def" {
                    if let Ok(input) = Input::from_bytes(&body) {
                        println!("Goto def for: {:?}", input);
                        self.goto_def(input, self.analysis.clone())
                    } else {
                        b"{}\n".to_vec()
                    }
                } else if x == "/symbols" {
                    if let Ok(file_name) = parse_string(&body) {
                        self.symbols(file_name, self.analysis.clone())
                    } else {
                        b"{}\n".to_vec()
                    }
                } else if x == "/find_refs" {
                    if let Ok(input) = Input::from_bytes(&body) {
                        println!("find refs for: {:?}", input);
                        self.find_refs(input, self.analysis.clone())
                    } else {
                        b"{}\n".to_vec()
                    }
                } else if x == "/title" {
                    if let Ok(input) = Input::from_bytes(&body) {
                        self.title(input, self.analysis.clone())
                    } else {
                        b"{}\n".to_vec()
                    }
                } else if x == "/on_change" {
                    if let Ok(change) = ChangeInput::from_bytes(&body) {
                        // println!("on change: {:?}", change);
                        self.vfs.on_change(&change.changes);

                        // TODO need to queue a build with vfs changes
                    }
                    b"{}\n".to_vec()
                } else if x == "/on_save" {
                    if let Ok(save) = SaveInput::from_bytes(&body) {
                        println!("on save: {}", &save.saved_file);
                        self.vfs.on_save(&save.saved_file);

                        // TODO log this on a work queue and coalesce builds
                        let res = build(&save.project_path);
                        let reply = serde_json::to_string(&res).unwrap();
                        println!("build result: {:?}", res);

                        println!("Refreshing rustw cache");
                        self.analysis.reload(Path::new(&save.project_path).file_name()
                                                                          .unwrap()
                                                                          .to_str()
                                                                          .unwrap()).unwrap();
                        reply.as_bytes().to_vec()
                    } else {
                        b"{}\n".to_vec()
                    }
                } else if x == "/on_build" {
                    if let Ok(file_name) = parse_string(&body) {
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

        res.headers_mut().set(ContentType::json());
        res.send(&msg).unwrap();
    }
}

