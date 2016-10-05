use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use std::io::{BufRead, BufReader};

use actions::Position;
use analysis::Span;
use ide::{Input, SaveInput};
use serde_json;

#[derive(Clone, Copy, Debug)]
pub struct Src<'a, 'b> {
    pub file_name: &'a str,
    // 1 indexed
    pub line: usize,
    pub name: &'b str,
}

pub fn src<'a, 'b>(file_name: &'a str, line: usize, name: &'b str) -> Src<'a, 'b> {
    Src {
        file_name: file_name,
        line: line,
        name: name,
    }
}

pub struct Cache {
    base_path: String,
    files: HashMap<String, Vec<String>>,
}

impl Cache {
    pub fn new(base_path: &str) -> Cache {
        Cache {
            base_path: base_path.to_owned(),
            files: HashMap::new(),
        }
    }

    pub fn mk_span(&mut self, src: Src) -> Span {
        let line = self.get_line(src);
        let col = line.find(src.name).expect(&format!("Line does not contain name {}", src.name));
        Span {
            file_name: self.abs_path(src.file_name),
            line_start: src.line - 1,
            line_end: src.line - 1,
            column_start: char_of_byte_index(&line, col),
            column_end: char_of_byte_index(&line, col + src.name.len()),
        }
    }

    pub fn mk_position(&mut self, src: Src) -> Position {
        let line = self.get_line(src);
        let col = line.find(src.name).expect(&format!("Line does not contain name {}", src.name));
        Position {
            filepath: self.abs_path(src.file_name),
            line: src.line - 1,
            col: char_of_byte_index(&line, col),
        }
    }

    fn abs_path(&self, file_name: &str) -> String {
        Path::new(&format!("{}/{}", self.base_path, file_name)).canonicalize()
                                                               .expect("Couldn't canonocalise path")
                                                               .to_str()
                                                               .unwrap()
                                                               .to_owned()
    }

    pub fn mk_input(&mut self, src: Src) -> Vec<u8> {
        let span = self.mk_span(src);
        let pos = self.mk_position(src);
        let input = Input { pos: pos, span: span };

        let s = serde_json::to_string(&input).unwrap();
        let s = format!("{{{}}}", s.replace("\"", "\\\""));
        s.as_bytes().to_vec()
    }

    pub fn mk_save_input(&self, file_name: &str) -> Vec<u8> {
        let input = SaveInput {
            project_path: self.abs_path("."),
            saved_file: file_name.to_owned(),
        };
        let s = serde_json::to_string(&input).unwrap();
        let s = format!("{{{}}}", s.replace("\"", "\\\""));
        s.as_bytes().to_vec()
    }

    fn get_line(&mut self, src: Src) -> String {
        let base_path = &self.base_path;
        let lines = self.files.entry(src.file_name.to_owned()).or_insert_with(|| {
            let file_name = &format!("{}/{}", base_path, src.file_name);
            let file = File::open(file_name).expect(&format!("Couldn't find file: {}", file_name));
            let lines = BufReader::new(file).lines();
            lines.collect::<Result<Vec<_>, _>>().unwrap()
        });

        if src.line - 1 >= lines.len() {
            panic!("Line {} not in file, found {} lines", src.line, lines.len());
        }

        lines[src.line - 1].to_owned()
    }
}

fn char_of_byte_index(s: &str, byte: usize) -> usize {
    for (c, (b, _)) in s.char_indices().enumerate() {
        if b == byte {
            return c;
        }
    }

    panic!("Couldn't find byte {} in {:?}", byte, s);
}
