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

use std::{env, process};

#[cfg(feature = "ipc")]
mod ipc;

pub fn run() {
    // env_logger::init();

    #[cfg(feature = "ipc")]
    let (file_loader, runtime) = {
        let mut rt = tokio::runtime::Runtime::new().unwrap();
        (
            env::var("RLS_IPC_ENDPOINT")
                .ok()
                .and_then(|endpoint| {
                    ipc::FileLoader::spawn(endpoint.into(), &mut rt)
                        .map_err(|e| log::warn!("Couldn't connect to IPC endpoint: {:?}", e))
                        .ok()
                })
                .map(ipc::FileLoader::into_boxed)
                .unwrap_or(None),
            rt,
        )
    };
    #[cfg(not(feature = "ipc"))]
    let file_loader = None;

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

        run_compiler(&args, &mut ShimCalls, file_loader, None)
    })
    .and_then(|result| result);

    #[cfg(feature = "ipc")]
    futures::future::Future::wait(runtime.shutdown_now()).unwrap();

    process::exit(result.is_err() as i32);
}

struct ShimCalls;

impl Callbacks for ShimCalls {
    fn config(&mut self, config: &mut interface::Config) {
        config.opts.debugging_opts.continue_parse_after_error = true;
        config.opts.debugging_opts.save_analysis = true;
    }
}
