//! Available remote procedure call (RPC) interfaces.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use jsonrpc_derive::rpc;
use serde::{Deserialize, Serialize};

pub use jsonrpc_core::{Error, Result};

// Separated because #[rpc] macro generated a `gen_client` mod and so two
// interfaces cannot be derived in the same scope due to a generated name clash
/// RPC interface for an overriden file loader to be used inside `rustc`.
pub mod file_loader {
    use super::*;
    // Expanded via #[rpc]
    pub use gen_client::Client;
    pub use rpc_impl_Rpc::gen_server::Rpc as Server;

    #[rpc]
    /// RPC interface for an overriden file loader to be used inside `rustc`.
    pub trait Rpc {
        /// Query the existence of a file.
        #[rpc(name = "file_exists")]
        fn file_exists(&self, path: PathBuf) -> Result<bool>;

        /// Read the contents of a UTF-8 file into memory.
        #[rpc(name = "read_file")]
        fn read_file(&self, path: PathBuf) -> Result<String>;
    }
}

// Separated because #[rpc] macro generated a `gen_client` mod and so two
// interfaces cannot be derived in the same scope due to a generated name clash
/// RPC interface to feed back data from `rustc` instances.
pub mod callbacks {
    use super::*;
    // Expanded via #[rpc]
    pub use gen_client::Client;
    pub use rpc_impl_Rpc::gen_server::Rpc as Server;

    #[rpc]
    /// RPC interface to feed back data from `rustc` instances.
    pub trait Rpc {
        /// Hands back computed analysis data for the compiled crate
        #[rpc(name = "complete_analysis")]
        fn complete_analysis(&self, analysis: rls_data::Analysis) -> Result<()>;

        /// Hands back computed input files for the compiled crate
        #[rpc(name = "input_files")]
        fn input_files(&self, input_files: HashMap<PathBuf, HashSet<Crate>>) -> Result<()>;
    }
}

/// Build system-agnostic, basic compilation unit
#[derive(PartialEq, Eq, Hash, Debug, Clone, Deserialize, Serialize)]
pub struct Crate {
    /// Crate name
    pub name: String,
    /// Optional path to a crate root
    pub src_path: Option<PathBuf>,
    /// Edition in which a given crate is compiled
    pub edition: Edition,
    /// From rustc; mainly used to group other properties used to disambiguate a
    /// given compilation unit.
    pub disambiguator: (u64, u64),
}

/// Rust edition
#[derive(PartialEq, Eq, Hash, Debug, PartialOrd, Ord, Copy, Clone, Deserialize, Serialize)]
pub enum Edition {
    /// Rust 2015
    Edition2015,
    /// Rust 2018
    Edition2018,
    /// Rust 2021
    Edition2021,
}
