use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

#[test]
fn mcp_handshake_list_and_call() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.py"), "def alpha(x):\n    return x\n").unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_blast"))
        .arg("--root")
        .arg(tmp.path())
        .arg("serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let mut send = |v: Value| {
        writeln!(stdin, "{}", v).unwrap();
        stdin.flush().unwrap();
    };
    let mut recv = || -> Value {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };

    send(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}));
    let r = recv();
    assert_eq!(r["id"], 1);
    assert_eq!(r["result"]["serverInfo"]["name"], "blastcode");
    assert_eq!(r["result"]["protocolVersion"], "2025-06-18");

    send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));

    send(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
    let r = recv();
    let names: Vec<&str> = r["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names.len(), 14);
    assert!(names.contains(&"get_impact_radius"));
    assert!(names.contains(&"get_affected_tests"));
    assert!(names.contains(&"verify_patch"));
    assert!(names.contains(&"get_co_changed_files"));
    assert!(names.contains(&"find_dead_code"));
    assert!(names.contains(&"grep_workspace"));

    // Background indexing may still be running; poll until the symbol is visible.
    let mut found = false;
    for i in 0..50 {
        send(json!({"jsonrpc":"2.0","id":10+i,"method":"tools/call","params":{"name":"search_symbols","arguments":{"query":"alpha"}}}));
        let r = recv();
        assert_eq!(r["result"]["isError"], false, "{r}");
        let text = r["result"]["content"][0]["text"].as_str().unwrap();
        if text.contains("a.py") {
            found = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(found, "symbol never became searchable");

    send(json!({"jsonrpc":"2.0","id":99,"method":"tools/call","params":{"name":"nope","arguments":{}}}));
    let r = recv();
    assert_eq!(r["error"]["code"], -32602);

    send(json!({"jsonrpc":"2.0","id":100,"method":"does/not/exist"}));
    let r = recv();
    assert_eq!(r["error"]["code"], -32601);

    drop(stdin);
    let _ = child.wait();
}
