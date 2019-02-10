use crate::actions::InitActionContext;
use lazy_static::lazy_static;
use log::error;
use ordslice::Ext;
use regex::Regex;
use rls_span::{Column, Position, Range, Row, ZeroIndexed};
use rls_vfs::FileContents;
use serde_derive::Serialize;

use std::{collections::HashMap, iter, path::Path};

pub fn collect_run_actions(ctx: &InitActionContext, file: &Path) -> Vec<RunAction> {
    let text = match ctx.vfs.load_file(file) {
        Ok(FileContents::Text(text)) => text,
        Ok(FileContents::Binary(_)) => return Vec::new(),
        Err(e) => {
            error!("failed to collect run actions: {}", e);
            return Vec::new();
        }
    };
    if !text.contains("#[test]") {
        return Vec::new();
    }

    lazy_static! {
        static ref TEST_FN_RE: Regex = Regex::new(r"#\[test\]\s*\n\s*fn\s*(?P<name>\w+)").unwrap();
    }

    let line_index = LineIndex::new(&text);

    let mut ret = Vec::new();
    for caps in TEST_FN_RE.captures_iter(&text) {
        let group = caps.name("name").unwrap();
        let target_element = Range::from_positions(
            line_index.offset_to_position(group.start()),
            line_index.offset_to_position(group.end()),
        );
        let test_name = group.as_str();
        let run_action = RunAction {
            label: "Run test".to_string(),
            target_element,
            cmd: Cmd {
                binary: "cargo".to_string(),
                args: vec![
                    "test".to_string(),
                    "--".to_string(),
                    "--nocapture".to_string(),
                    test_name.to_string(),
                ],
                env: iter::once(("RUST_BACKTRACE".to_string(), "short".to_string())).collect(),
            },
        };
        ret.push(run_action);
    }
    ret
}

pub struct RunAction {
    pub label: String,
    pub target_element: Range<ZeroIndexed>,
    pub cmd: Cmd,
}

#[derive(Serialize)]
pub struct Cmd {
    pub binary: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
}

pub struct LineIndex {
    newlines: Vec<usize>,
}

impl LineIndex {
    pub fn new(text: &str) -> LineIndex {
        let newlines = text.bytes().enumerate().filter(|&(_i, b)| b == b'\n').map(|(i, _b)| i + 1);
        let newlines = iter::once(0).chain(newlines).collect();
        LineIndex { newlines }
    }

    pub fn offset_to_position(&self, offset: usize) -> Position<ZeroIndexed> {
        let line = self.newlines.upper_bound(&offset) - 1;
        let line_start_offset = self.newlines[line];
        let col = offset - line_start_offset;
        Position::new(Row::new_zero_indexed(line as u32), Column::new_zero_indexed(col as u32))
    }
}
