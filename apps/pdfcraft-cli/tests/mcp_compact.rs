//! `pdfcraft-cli mcp --compact`: the flag reaches the server, `--root` is still honoured, and plain `mcp` still lists every tool.
#![cfg(feature = "mcp")]

use std::io::Write as _;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

/// Run `pdfcraft-cli mcp <flags>` over an initialize request plus `requests` and return the replies to `requests`.
fn session(flags: &[&str], requests: &[Value]) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_pdfcraft-cli"))
        .arg("mcp")
        .args(flags)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = String::from("{\"jsonrpc\":\"2.0\",\"id\":0,\"method\":\"initialize\",\"params\":{}}\n");
    for r in requests {
        input.push_str(&r.to_string());
        input.push('\n');
    }
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap().lines().skip(1).map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn request(id: u64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

/// The `tools/list` tool names of `pdfcraft-cli mcp <flags>`.
fn listed_tools(flags: &[&str]) -> Vec<String> {
    let reply = session(flags, &[request(1, "tools/list", json!({}))]).remove(0);
    reply["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect()
}

#[test]
fn mcp_rejects_unknown_options_before_starting_the_server() {
    for option in ["--totally-bogus-flag", "--rot", "--root=."] {
        let out = Command::new(env!("CARGO_BIN_EXE_pdfcraft-cli")).args(["mcp", option]).output().expect("run CLI");

        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "mcp {option} unexpectedly succeeded");
        assert!(stderr.contains("unknown option") && stderr.contains(option), "mcp {option}: {stderr}");
        assert!(!stderr.contains("MCP server on stdio"), "server started despite invalid option: {stderr}");
    }
}

#[test]
fn compact_flag_shortens_the_tool_list() {
    let full = listed_tools(&[]);
    let compact = listed_tools(&["--compact"]);
    assert_eq!(compact.len(), 17, "{compact:?}");
    assert!(compact.iter().any(|n| n == "tool_search") && compact.iter().any(|n| n == "tool_call"));
    assert!(compact.iter().all(|n| full.contains(n) || n == "tool_search" || n == "tool_call"));
    assert!(full.len() > 100 && !full.iter().any(|n| n == "tool_search"), "{} tools", full.len());
    // The flag can sit on either side of --root.
    assert_eq!(listed_tools(&["--root", ".", "--compact"]), compact);
    assert_eq!(listed_tools(&["--compact", "--root", "."]), compact);
}

#[test]
fn compact_flag_keeps_the_root() {
    let root = std::env::temp_dir().join(format!("pdfcraft-cli-compact-root-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let outside = std::env::temp_dir().join(format!("pdfcraft-cli-compact-outside-{}.pdf", std::process::id()));
    let outside = outside.to_string_lossy().into_owned();
    let root_arg = root.to_string_lossy().into_owned();
    let open_outside =
        request(1, "tools/call", json!({ "name": "tool_call", "arguments": { "name": "doc_open", "arguments": { "path": outside } } }));
    let text = |reply: &Value| reply["result"]["content"][0]["text"].as_str().unwrap_or_default().to_string();

    // With the root, in either flag order, a path outside it is refused with the root's own message.
    for flags in [vec!["--root", root_arg.as_str(), "--compact"], vec!["--compact", "--root", root_arg.as_str()]] {
        let reply = session(&flags, std::slice::from_ref(&open_outside)).remove(0);
        assert_eq!(reply["result"]["isError"], true, "{flags:?}: {reply}");
        assert!(text(&reply).contains("is outside the allowed directory"), "{flags:?}: {}", text(&reply));
    }
    // Without a root the same call gets past that check (the file does not exist, so it fails for that reason instead).
    let reply = session(&["--compact"], std::slice::from_ref(&open_outside)).remove(0);
    assert_eq!(reply["result"]["isError"], true, "{reply}");
    assert!(!text(&reply).contains("is outside the allowed directory"), "{}", text(&reply));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn theme_management_uses_existing_mcp_and_compact_discovery() {
    let root = std::env::temp_dir().join(format!("pdfcraft-mcp-themes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let call = |id, name, arguments| request(id, "tools/call", json!({"name":name,"arguments":arguments}));
    let replies = session(
        &["--root", root.to_str().unwrap(), "--compact"],
        &[
            call(1, "tool_search", json!({"name":"theme_update"})),
            call(
                2,
                "theme_create",
                json!({"store":"themes.json","name":"Agent theme","base_theme_id":"ocean","id":"00000000-0000-4000-8000-000000000001"}),
            ),
            call(
                3,
                "tool_call",
                json!({"name":"theme_activate","arguments":{"store":"themes.json","id":"custom:00000000-0000-4000-8000-000000000001"}}),
            ),
            call(4, "theme_inspect", json!({"store":"themes.json","id":"custom:00000000-0000-4000-8000-000000000001"})),
            call(5, "theme_delete", json!({"store":"themes.json","id":"light"})),
            call(6, "theme_list", json!({"store":"themes.json"})),
            call(7, "theme_inspect", json!({"store":"themes.json","id":"ocean"})),
        ],
    );
    assert!(replies[0]["result"].to_string().contains("theme_update"));
    assert_eq!(replies[1]["result"]["isError"], false, "{}", replies[1]);
    assert_eq!(replies[2]["result"]["isError"], false, "{}", replies[2]);
    assert_eq!(replies[3]["result"]["structuredContent"]["palette"].as_object().unwrap().len(), 31);
    assert_eq!(replies[4]["result"]["isError"], true);
    assert_eq!(replies[4]["result"]["structuredContent"]["error"]["code"], "builtin_protected");
    let themes = replies[5]["result"]["structuredContent"]["themes"].as_array().unwrap();
    for (id, name) in [
        ("catppuccin", "Atelier"),
        ("solarized", "Editorial"),
        ("forest", "Heritage"),
        ("ocean", "Executive"),
        ("sunset", "Sienna"),
        ("paper-ink", "Paper & Ink"),
    ] {
        assert_eq!(themes.iter().find(|theme| theme["id"] == id).unwrap()["name"], name);
    }
    assert_eq!(replies[6]["result"]["structuredContent"]["name"], "Executive");
    let _ = std::fs::remove_dir_all(root);
}
