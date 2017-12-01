// Copyright 2016 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::mem;
use std::process::{Child, ChildStdin, ChildStdout};

use serde_json;

#[derive(Clone, Debug)]
pub struct ExpectedMessage {
    id: Option<u64>,
    contains: Vec<String>,
}

impl ExpectedMessage {
    pub fn new(id: Option<u64>) -> ExpectedMessage {
        ExpectedMessage {
            id: id,
            contains: vec![],
        }
    }

    pub fn expect_contains(&mut self, s: &str) -> &mut ExpectedMessage {
        self.contains.push(s.to_owned());
        self
    }
}

pub fn read_message<R: Read>(reader: &mut BufReader<R>) -> io::Result<String> {
    let mut content_length = None;
    // Read the headers
    loop {
        let mut header = String::new();
        reader.read_line(&mut header)?;
        if header.len() == 0 {
            panic!("eof")
        }
        if header == "\r\n" {
            // This is the end of the headers
            break;
        }
        let parts: Vec<&str> = header.splitn(2, ": ").collect();
        if parts[0] == "Content-Length" {
            content_length = Some(parts[1].trim().parse::<usize>().unwrap())
        }
    }

    // Read the actual message
    let content_length = content_length.expect("did not receive Content-Length header");
    let mut msg = vec![0; content_length];
    reader.read_exact(&mut msg)?;
    let result = String::from_utf8_lossy(&msg).into_owned();
    Ok(result)
}

pub fn expect_messages<R: Read>(reader: &mut BufReader<R>, expected: &[&ExpectedMessage]) {
    let mut results: Vec<String> = Vec::new();
    while results.len() < expected.len() {
        results.push(read_message(reader).unwrap());
    }

    println!(
        "expect_messages:\n  results: {:#?},\n  expected: {:#?}",
        results,
        expected
    );
    assert_eq!(results.len(), expected.len());
    for (found, expected) in results.iter().zip(expected.iter()) {
        let values: serde_json::Value = serde_json::from_str(found).unwrap();
        assert!(
            values
                .get("jsonrpc")
                .expect("Missing jsonrpc field")
                .as_str()
                .unwrap() == "2.0",
            "Bad jsonrpc field"
        );
        if let Some(id) = expected.id {
            assert_eq!(
                values
                    .get("id")
                    .expect("Missing id field")
                    .as_u64()
                    .unwrap(),
                id,
                "Unexpected id"
            );
        }
        for c in expected.contains.iter() {
            found
                .find(c)
                .expect(&format!("Could not find `{}` in `{}`", c, found));
        }
    }
}

pub struct RlsHandle {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl RlsHandle {
    pub fn new(mut child: Child) -> RlsHandle {
        let stdin = mem::replace(&mut child.stdin, None).unwrap();
        let stdout = mem::replace(&mut child.stdout, None).unwrap();
        let stdout = BufReader::new(stdout);

        RlsHandle {
            child,
            stdin,
            stdout,
        }
    }

    pub fn send_string(&mut self, s: &str) -> io::Result<usize> {
        let full_msg = format!("Content-Length: {}\r\n\r\n{}", s.len(), s);
        self.stdin.write(full_msg.as_bytes())
    }
    pub fn send(&mut self, j: serde_json::Value) -> io::Result<usize> {
        self.send_string(&j.to_string())
    }
    pub fn notify(&mut self, method: &str, params: serde_json::Value) -> io::Result<usize> {
        self.send(json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
    }
    pub fn request(&mut self, id: u64, method: &str, params: serde_json::Value) -> io::Result<usize> {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
    }
    pub fn shutdown_exit(&mut self) {
        self.request(99999, "shutdown", json!({})).unwrap();

        self.expect_messages(&[
            &ExpectedMessage::new(Some(99999)),
        ]);

        self.notify("exit", json!({})).unwrap();

        let ecode = self.child.wait()
            .expect("failed to wait on child rls process");
        
        assert!(ecode.success());
    }

    pub fn expect_messages(&mut self, expected: &[&ExpectedMessage]) {
        expect_messages(&mut self.stdout, expected);
    }
}