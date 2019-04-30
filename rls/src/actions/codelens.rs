
use lazy_static::lazy_static;
use crate::lsp_data::*;
use crate::actions::InitActionContext;
use rls_vfs::FileContents;
use regex::Regex;
use log::error;
use serde_json::json;
use std::sync::atomic::Ordering;
use std::collections::HashMap;
use std::vec::Vec;

fn offset_to_position(text: &str, offset: usize) -> Option<Position> {
    if offset > text.len() {
        return None;
    }
    let mut line = 0u64;
    let mut character = 0u64;
    let mut count = 0;
    for c in text.chars() {
        if count >= offset {
            return Some(Position::new(line, character));
        }
        if c == '\n' {
            line += 1;
            character = 0;
        } else {
            character += 1;
        }
        count += 1;
    }
    None
}

struct Namespace {
    members: HashMap<String, String>,
    subspaces: HashMap<String, Namespace>,
    glob: bool
}

impl Namespace {
    fn new() -> Self {
        Namespace {members: HashMap::<String, String>::new(), subspaces: HashMap::<String, Namespace>::new(), glob: false }
    }

    fn print(&self, prefix: String) {
        if self.glob {
            println!("{}::*", &prefix);
        }
        for (key, value) in self.members.iter() {
            println!("{}::{} as {}", &prefix, key, value);
        }
        for (key, value) in self.subspaces.iter() {
            value.print(prefix.clone()+ if prefix.len() == 0 {""} else {"::"} + key);
        }
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
}

fn parse_suffix(prefix: String, suffix: String, map: &mut HashMap<String, String>)  {
    lazy_static! {
        static ref SUFFIX_REGEX: Regex = Regex::new(r"[^,]+").unwrap();
    }
    lazy_static! {
        static ref AS_REGEX: Regex = Regex::new(r"([A-Za-z0-9_]+)\s+as\s+([A-Za-z0-9_]+)").unwrap();
    }
    let mut suffix_vec = Vec::<String>::new();
    if suffix.starts_with("{") {
        let suffix = &(suffix[1..(suffix.len()-1)]).trim();
        for capture in SUFFIX_REGEX.captures_iter(suffix) {
            match capture.get(0) {
                Some(matched) => {
                    suffix_vec.push(matched.as_str().trim().to_string());
                }
                _ => {}
            }
        }
    } else {
        suffix_vec.push(suffix)
    };
    for suffix in suffix_vec.iter() {
        match AS_REGEX.captures(suffix) {
            Some(capture) => {
                let suffix = &capture[1];
                let alias = &capture[2];
                map.insert((prefix.clone() + suffix).to_string(), alias.to_string());
            }
            None => {
                map.insert((prefix.clone() + suffix).to_string(), suffix.to_string());
            }
        }
    }
}

fn parse_uses(text: &str) -> HashMap<String, String> {
    lazy_static! {
        static ref USE_REGEX: Regex = Regex::new(r"use (.*)::([^:;]+);").unwrap();
    }
    let mut uses: HashMap<String, String> = HashMap::<String, String>::new();
    for capture in USE_REGEX.captures_iter(text) {
        let mut prefix = capture[1].trim().to_string();
        if prefix.starts_with("crate::") {
            prefix = prefix[7..prefix.len()].to_string();
        }
        let suffix = capture[2].trim().to_string();
        parse_suffix((prefix+"::").to_string(), suffix, &mut uses);
    }
    uses
}

fn simplify_typename(typename: String, uses: &HashMap<String, String>) -> String {
    lazy_static! {
        static ref SUB_TYPE: Regex = Regex::new(r"(['a-zA-Z:_]+)").unwrap();
    }
    let mut simplified = "".to_string();
    let mut captures = SUB_TYPE.captures_iter(&typename);
    if let Some(mut latest) = captures.nth(0).unwrap().get(0) {
        let subtype = latest;
        simplified += &typename[0..subtype.start()].to_string();
        simplified += match uses.get(subtype.as_str()) {
            Some(value) => {
                value
            }
            _ => {
                subtype.as_str()
            }
        };
        for subtype in captures {
            match subtype.get(0) {
                Some(subtype) => {
                    let start = latest.end();
                    simplified += &typename[start..subtype.start()].to_string();
                    simplified += match uses.get(subtype.as_str()) {
                        Some(value) => {
                            value
                        }
                        _ => {
                            subtype.as_str()
                        }
                    };
                    latest = subtype;
                }
                _ => {}
            }
        }
        simplified += &typename[latest.end()..typename.len()];
    }
    simplified
}

#[test]
fn test_use_parsing() {
    let test_str = r"pub use lsp_types::notification::{Exit as ExitNotification, ShowMessage};
pub use lsp_types::request::Initialize as InitializeRequest;
pub use lsp_types::request::Shutdown as ShutdownRequest;
use lsp_types::{
    CodeActionProviderCapability, CodeLensOptions, CompletionOptions, ExecuteCommandOptions,
};";
    let mut uses = parse_uses(test_str);
    for (key, value) in uses.iter() {
        println!("{}: {}", key, value);
    }
    println!("\n{} => {}\n", "lsp_types::request::Shutdown", simplify_typename("lsp_types::request::Shutdown".to_string(), &uses));
    println!("\n{} => {}\n", "std::sync::Mutex<lsp_types::request::Shutdown>", simplify_typename("std::sync::Mutex<lsp_types::request::Shutdown>".to_string(), &uses));
    uses.insert("std::sync::Mutex".to_string(), "Mutex".to_string());
    println!("\n{} => {}\n", "std::sync::Mutex<lsp_types::request::Shutdown>", simplify_typename("std::sync::Mutex<lsp_types::request::Shutdown>".to_string(), &uses));
    assert!(false);
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

    let aliases = parse_uses(&text);

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
                                title: {if capture.as_str().contains("mut") {":mut "} else {": "}}.to_string() + &simplify_typename(typename, &aliases),
                                command: "".to_string(),
                                arguments: None})
                        }
                        _ => {
                            None
                            // Some(Command {
                            //     title: {if capture.as_str().contains("mut") {":mut ???"} else {": ???"}}.to_string(),
                            //     command: "".to_string(),
                            //     arguments: None})
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