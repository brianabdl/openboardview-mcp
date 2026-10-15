//! End-to-end test: runs the server binary over stdio, as an MCP client
//! would, against a small BRD fixture. Exercises the C++ parsers, the parse
//! worker process and the tools together.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Client {
    fn start(home: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_openboardview-mcp"))
            // Isolate from the developer's real OpenBoardView config.
            .env("XDG_CONFIG_HOME", home)
            .env("HOME", home)
            .env("OBV_MCP_LOG", "warn")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start server");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut client = Self { child, stdin, stdout, next_id: 0 };

        let init = client.request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
        );
        assert_eq!(init["result"]["serverInfo"]["name"], "openboardview-mcp");
        client.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        client
    }

    fn send(&mut self, message: Value) {
        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let mut line = String::new();
            assert!(self.stdout.read_line(&mut line).unwrap() > 0, "server closed stdout");
            let message: Value = serde_json::from_str(&line).expect("valid JSON-RPC line");
            if message["id"] == id {
                return message;
            }
        }
    }

    /// Calls a tool and returns (is_error, content array).
    fn call(&mut self, tool: &str, args: Value) -> (bool, Vec<Value>) {
        let response = self.request("tools/call", json!({"name": tool, "arguments": args}));
        let result = &response["result"];
        assert!(result.is_object(), "protocol error calling {tool}: {response}");
        (result["isError"] == true, result["content"].as_array().cloned().unwrap_or_default())
    }

    /// Calls a tool that must succeed and returns its JSON payload.
    fn call_ok(&mut self, tool: &str, args: Value) -> Value {
        let (is_error, content) = self.call(tool, args.clone());
        let text = content.iter().find(|c| c["type"] == "text").and_then(|c| c["text"].as_str()).unwrap_or("");
        assert!(!is_error, "{tool}({args}) failed: {text}");
        serde_json::from_str(text).unwrap_or_else(|_| panic!("{tool} returned non-JSON: {text}"))
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A fresh directory holding a copy of the fixture board. `name` keeps
/// parallel tests apart.
fn workspace(name: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("obv-mcp-it-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let board = dir.join("sample.brd");
    std::fs::copy(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample.brd"), &board).unwrap();
    (dir, board)
}

fn names(list: &Value, key: &str) -> Vec<String> {
    let mut v: Vec<String> = list.as_array().unwrap().iter().map(|x| x[key].as_str().unwrap().to_string()).collect();
    v.sort();
    v
}

#[test]
fn tools_over_stdio() {
    let (dir, board) = workspace("tools");
    let mut c = Client::start(&dir);

    let tools = c.request("tools/list", json!({}));
    let tool_names = names(&tools["result"]["tools"], "name");
    for expected in ["open_board", "get_part", "get_net", "parts_between_nets", "render_board", "add_annotation"] {
        assert!(tool_names.iter().any(|n| n == expected), "missing tool {expected}");
    }

    // Board must be opened (or passed) first.
    let (is_error, _) = c.call("list_parts", json!({}));
    assert!(is_error);

    let summary = c.call_ok("open_board", json!({"path": board}));
    assert_eq!(summary["format"], "brd");
    assert_eq!(summary["parts"], 5);
    assert_eq!(summary["pins"], 11);
    assert_eq!(summary["nails"], 2);
    assert_eq!(summary["ground_nets"], json!(["GND"]));
    assert_eq!(summary["parts_by_side"]["bottom"], 1);

    let part = c.call_ok("get_part", json!({"name": "u1"}));
    assert_eq!(part["part"]["kind"], "ic");
    assert_eq!(part["pins"].as_array().unwrap().len(), 4);
    assert_eq!(part["pins"][1]["net"], "EN_3V3");
    assert_eq!(part["pins"][1]["net_kind"], "signal");

    let net = c.call_ok("get_net", json!({"name": "PP3V3"}));
    assert_eq!(net["net"]["kind"], "power");
    assert_eq!(net["net"]["probe"], 1);
    assert_eq!(names(&net["parts"], "part"), ["C1", "C2", "U1"]);
    assert_eq!(net["test_points"].as_array().unwrap().len(), 1);

    let caps = c.call_ok("parts_between_nets", json!({"net_a": "PP3V3", "kind": "capacitor"}));
    assert_eq!(caps["count"], 2);
    assert_eq!(caps["nets_b"], json!(["GND"]));

    let trace = c.call_ok("trace_net", json!({"net": "PP5V"}));
    assert_eq!(trace["reached"][0]["net"], "PP5V_IN");
    assert_eq!(trace["reached"][0]["via_part"], "R1");

    let rails = c.call_ok("power_rails", json!({}));
    let pp3v3 = rails["rails"].as_array().unwrap().iter().find(|r| r["net"] == "PP3V3").unwrap();
    assert_eq!(pp3v3["caps_to_ground"], 2);

    let near = c.call_ok("nearby_parts", json!({"part": "C1", "radius_mm": 3}));
    assert_eq!(near["parts"][0]["part"]["name"], "C2");

    let hits = c.call_ok("search", json!({"query": "pp5"}));
    assert_eq!(hits["hits"][0]["kind"], "net");

    let (is_error, text) = c.call("get_part", json!({"name": "C3"}));
    assert!(is_error);
    assert!(text[0]["text"].as_str().unwrap().contains("did you mean"));

    // Rendering returns a PNG plus metadata.
    let (is_error, content) =
        c.call("render_board", json!({"highlight_parts": ["U1"], "highlight_nets": ["PP3V3"], "width": 400}));
    assert!(!is_error);
    assert_eq!(content[0]["type"], "image");
    assert_eq!(content[0]["mimeType"], "image/png");

    // Annotations are written next to the board, in OpenBoardView's format.
    let added = c.call_ok("add_annotation", json!({"part": "C1", "pin": "2", "note": "checked"}));
    assert_eq!(added["annotation"]["net"], "GND");
    assert!(dir.join("sample_brd.sqlite3").is_file());
    let id = added["id"].as_i64().unwrap();
    c.call_ok("update_annotation", json!({"id": id, "note": "checked twice"}));
    let list = c.call_ok("list_annotations", json!({}));
    assert_eq!(list["annotations"][0]["note"], "checked twice");
    c.call_ok("delete_annotation", json!({"id": id}));
    let list = c.call_ok("list_annotations", json!({}));
    assert!(list["annotations"].as_array().unwrap().is_empty());

    let diff = c.call_ok("compare_boards", json!({"path_a": board, "path_b": board}));
    assert_eq!(diff["totals"]["pin_net_changes"], 0);

    drop(c);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn bad_files_are_reported_not_fatal() {
    let (dir, _) = workspace("bad-files");

    // Pins referencing a part index far out of range: OpenBoardView's BRD
    // parser crashes on this. The worker process absorbs the crash.
    let evil = dir.join("evil.brd");
    std::fs::write(
        &evil,
        "str_length:\nvar_data:\n0 1 2 0\nParts:\nR1 5 2\nPins:\n10 10 -99 0 A\n20 10 -99 900000 B\n",
    )
    .unwrap();
    let unknown = dir.join("notes.txt");
    std::fs::write(&unknown, "hello").unwrap();

    let mut c = Client::start(&dir);
    let (is_error, content) = c.call("open_board", json!({"path": evil}));
    assert!(is_error);
    assert!(content[0]["text"].as_str().unwrap().contains("parser"), "{content:?}");

    let (is_error, content) = c.call("open_board", json!({"path": unknown}));
    assert!(is_error);
    assert!(content[0]["text"].as_str().unwrap().contains("Unrecognized"), "{content:?}");

    // The server is still alive and serving.
    let (is_error, _) = c.call("open_board", json!({"path": dir.join("sample.brd")}));
    assert!(!is_error);

    drop(c);
    std::fs::remove_dir_all(&dir).unwrap();
}
