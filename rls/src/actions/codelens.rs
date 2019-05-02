
use crate::lsp_data::*;
use crate::actions::InitActionContext;
use rls_vfs::FileContents;
use log::error;
use serde_json::json;
use std::sync::atomic::Ordering;
use lazy_static::lazy_static;
use regex::Regex;
use std::collections::HashMap;
use std::vec::Vec;

fn offset_to_position(text: &str, offset: usize) -> Option<Position> {
    if offset > text.len() {
        return None;
    }
    let mut line = 0u64;
    let mut character = 0u64;
    let mut count = 0;
    for (count, c) in text.chars().enumerate() {
        if count >= offset {
            return Some(Position::new(line, character));
        }
        if c == '\n' {
            line += 1;
            character = 0;
        } else {
            character += 1;
        }
    }
    None
}

struct Namespace {
    members: HashMap<String, String>,
    subspaces: HashMap<String, Namespace>,
    glob: bool
}

impl std::fmt::Display for Namespace {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{{")?;
        for (origin, alias) in self.members.iter() {
            if origin == alias {
                write!(f, "{}, ", origin)?;
            } else {
                write!(f, "{} as {}, ", origin, alias)?;
            }
        }
        for (name, namespace) in self.subspaces.iter() {
            write!(f, "{}::{}, ", name, namespace)?;
        }
        if self.glob {
            write!(f, "*")?;
        }
        write!(f, "}}")?;
        Ok(())
    }
}

impl Namespace {
    fn new() -> Self {
        Namespace {members: HashMap::<String, String>::new(), subspaces: HashMap::<String, Namespace>::new(), glob: false }
    }

    fn get_subspace(&mut self, key: String) -> &mut Namespace {
        match self.subspaces.get_mut(&key) {
            Some(subspace) => {},
            None => {
                let subspace = Namespace::new();
                self.subspaces.insert(key.clone(), subspace);
            }
        };
        self.subspaces.get_mut(&key).unwrap()
    }

    fn parse(&mut self, value: String) {
        let value = value.trim().to_string();
        if value.starts_with("{") {
            let mut bracket_counter = 0;
            let mut start = 1;
            for (i, character) in value.chars().enumerate() {
                match character {
                    '{' => {bracket_counter+=1;}
                    '}' => {
                        bracket_counter-=1;
                        if bracket_counter == 0 {
                            self.parse(value[start..i].to_string());
                        }
                    }
                    ',' => {
                        if bracket_counter == 1 {
                            self.parse(value[start..i].to_string());
                            start = i + 1;
                        }
                    }
                    _ => {}
                };
            }
        } else {
            if let Some(position) = value.chars().position(|character| character == ':') {
                let prefix = value[0..position].to_string();
                let suffix = value[position+2..value.len()].to_string();
                self.get_subspace(prefix).parse(suffix);
            } else {
                if value == "*" {
                    self.glob = true;
                } else {
                    lazy_static! {
                        static ref AS_REGEX: Regex = Regex::new(r"([A-Za-z0-9_]+)\s+as\s+([A-Za-z0-9_]+)").unwrap();
                    }
                    match AS_REGEX.captures(&value) {
                        Some(capture) => {
                            self.members.insert(capture[1].to_string(), capture[2].to_string());
                        }
                        None => {self.members.insert(value.clone(), value.clone());}
                    };
                }
            }
        }
    }

    fn recurse_simplify(&self, typename: String) -> Option<String> {
        match typename.chars().position(|character| character == ':') {
            Some(position) => {
                let prefix = typename[0..position].to_string();
                let suffix = typename[position+2..typename.len()].to_string();
                if let Some(subspace) = self.subspaces.get(&prefix) {
                    if let Some(simplified) = subspace.recurse_simplify(suffix.clone()) {
                        return Some(simplified);
                    }
                }
                if let Some(simplified_prefix) = self.members.get(&prefix) {
                    return Some((simplified_prefix.to_string() + "::" + &suffix).to_string());
                }
                if self.glob {
                    return Some((prefix + "::" + &suffix).to_string())
                }
                None
            }
            None => {
                if let Some(simplified) = self.members.get(&typename) {
                    return Some(simplified.to_string());
                }
                if self.glob {
                    return Some(typename);
                }
                None
            }
        }
    }

    fn simplify(&self, typename: String) -> String {
        match self.recurse_simplify(typename.clone()) {
            Some(simplified) => simplified,
            None => typename
        }
    }
}

fn parse_uses(text: &str) -> Namespace {
    lazy_static! {
        static ref USE_REGEX: Regex = Regex::new(r"use ([^;]+);").unwrap();
    }
    let mut root = Namespace::new();
    for capture in USE_REGEX.captures_iter(text) {
        root.parse(capture[1].to_string());
    }
    root
}

#[test]
fn name_shortening_test() {
    let uses = "use std::vec::Vec;\nuse std::collections::*; use std::collections::HashSet as HashSetAlias; use std::sync::mpsc; use futures::{Futures,\nStream, mpsc::channel};";
    let root = parse_uses(uses);
    println!("Namespace Description:");
    println!("{}", root);
    println!();
    let test_cases = [("std::vec::Vec", "Vec"), ("std::collections::HashMap", "HashMap"), ("std::sync::Mutex", "std::sync::Mutex"), ("futures::Futures", "Futures"), ("std::sync::mpsc::channel", "mpsc::channel"), ("futures::mpsc::channel", "channel"), ("futures::Stream", "Stream"), ("std::collections::HashSet", "HashSetAlias")];
    for case in test_cases.iter() {
        println!("{} -> {}", case.0, root.simplify(case.0.to_string()));
        assert_eq!(root.simplify(case.0.to_string()), case.1);
    }
}

pub fn collect_declaration_typings(ctx: &InitActionContext, params: &TextDocumentIdentifier) -> Vec<CodeLens> {
    let analysis = &ctx.analysis;

    let file = parse_file_path(&params.uri).unwrap();

    let mut ret = Vec::new();

    let text = match ctx.vfs.load_file(&file) {
        Ok(FileContents::Text(text)) => text,
        Ok(FileContents::Binary(_)) => return ret,
        Err(e) => {
            error!("failed to collect run actions: {}", e);
            return ret;
        }
    };
    if !text.contains("let") {
        return ret;
    }
    lazy_static! {
        static ref LET_REGEX: Regex = Regex::new(r"let(\s+mut)?\s+([^( ]+)").unwrap();
    }

    let root = parse_uses(&text);

    for capture in LET_REGEX.find_iter(&text) {
        let offset = capture.end();
        if let Some(position) = offset_to_position(&text, offset.clone()) {
            let command = if (false && ctx.active_build_count.load(Ordering::Relaxed) > 0) {
                    None
                } else {
                    let span_position = Position::new(position.line, position.character-1);
                    let span = ctx.convert_pos_to_span(file.clone(), span_position);
                    let typename = analysis.show_type(&span);
                    match typename {
                        Ok(typename)=>{

                            Some(Command {
                                title: {if capture.as_str().contains("mut") {":mut "} else {": "}}.to_string() + &root.simplify(typename.to_string()),
                                command: "".to_string(),
                                arguments: None})
                        }
                        _ => {
                            None
                        }
                    }
                };
            let start = position.clone();
            let mut end = position;
            end.character += 1;
            let lens = CodeLens {range: Range::new(start, end), command, data: Some(json!({
                "lens_type": String::from("type_span"),
                "position": position,
                "file": params.clone(),
                "mutable": capture.as_str().contains("mut")
            }))};
            ret.push(lens);
        }
    }
    
    ret
}
