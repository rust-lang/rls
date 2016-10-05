use ide::VscodeKind;
use analysis::Span;

#[derive(Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct Position {
    pub filepath: String,
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Serialize, Eq, PartialEq, Deserialize)]
pub enum Provider {
    Compiler,
    Racer,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Completion {
    pub name: String,
    pub context: String,
}

#[derive(Debug, Serialize)]
pub struct Title {
    pub ty: String,
    pub docs: String,
    pub doc_url: String,
}

#[derive(Debug, Serialize)]
pub struct Symbol {
    pub name: String,
    pub kind: VscodeKind,
    pub span: Span,
}
