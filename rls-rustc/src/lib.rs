#![feature(rustc_private)]

extern crate env_logger;
extern crate rustc;
extern crate rustc_driver;
extern crate rustc_interface;
extern crate syntax;

use rustc::session::config::ErrorOutputType;
use rustc::session::early_error;
use rustc_driver::{run_compiler, Callbacks};
use rustc_interface::interface;
use syntax::source_map::FileLoader;

use std::env;
use std::process;

#[cfg(feature = "ipc")]
mod ipc;

fn file_loader() -> Option<Box<dyn FileLoader + Send + Sync>> {
    #[cfg(feature = "ipc")]
    {
        let endpoint = std::env::var("RLS_IPC_ENDPOINT").ok()?;
        let loader = ipc::IpcFileLoader::new(endpoint).expect("Couldn't connect to IPC endpoint");
        Some(Box::new(loader))
    }
    #[cfg(not(feature = "ipc"))]
    {
        None
    }
}

pub fn run() {
    env_logger::init();
    let result = rustc_driver::report_ices_to_stderr_if_any(|| {
        let args = env::args_os()
            .enumerate()
            .map(|(i, arg)| {
                arg.into_string().unwrap_or_else(|arg| {
                    early_error(
                        ErrorOutputType::default(),
                        &format!("Argument {} is not valid Unicode: {:?}", i, arg),
                    )
                })
            })
            .collect::<Vec<_>>();

        run_compiler(&args, &mut ShimCalls, file_loader(), None)
    })
    .and_then(|result| result);
    process::exit(result.is_err() as i32);
}

struct ShimCalls;

impl Callbacks for ShimCalls {
    fn config(&mut self, config: &mut interface::Config) {
        config.opts.debugging_opts.continue_parse_after_error = true;
        config.opts.debugging_opts.save_analysis = true;
    }
}
