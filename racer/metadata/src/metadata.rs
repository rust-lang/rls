//! Data structures for metadata
use racer_interner::InternedString;
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Metadata {
    pub packages: Vec<Package>,
    pub workspace_members: Vec<PackageId>,
    pub resolve: Option<Resolve>,
    #[serde(default)]
    pub workspace_root: PathBuf,
    pub target_directory: PathBuf,
    version: usize,
    #[serde(skip)]
    __guard: (),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Package {
    pub id: PackageId,
    pub targets: Vec<Target>,
    pub manifest_path: PathBuf,
    #[serde(default = "edition_default")]
    pub edition: InternedString,
    #[serde(skip)]
    __guard: (),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Resolve {
    pub nodes: Vec<ResolveNode>,
    #[serde(skip)]
    __guard: (),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolveNode {
    pub id: PackageId,
    pub dependencies: Vec<PackageId>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Target {
    pub name: InternedString,
    pub kind: Vec<InternedString>,
    pub src_path: PathBuf,
    #[serde(default = "edition_default")]
    pub edition: InternedString,
    #[serde(skip)]
    __guard: (),
}

const LIB_KINDS: [&'static str; 4] = ["lib", "rlib", "dylib", "proc-macro"];

impl Target {
    pub fn is_lib(&self) -> bool {
        self.kind.iter().any(|k| LIB_KINDS.contains(&k.as_str()))
    }
    pub fn is_2015(&self) -> bool {
        self.edition.as_str() == "2015"
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct PackageId(InternedString);

impl PackageId {
    pub fn name(&self) -> &str {
        let idx = self.0.find(' ').expect("Whitespace not found");
        &self.0[..idx]
    }
}

#[inline(always)]
fn edition_default() -> InternedString {
    InternedString::new("2015")
}
