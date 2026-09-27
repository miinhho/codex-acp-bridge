use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::fs::PermissionsExt,
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::Duration,
};

use proptest::test_runner::{Config, TestRunner};
use serde_json::{Value, json};

struct BridgeProcess {
    _temp: tempfile::TempDir,
    child: Child,
    stdout: Receiver<String>,
}

impl BridgeProcess {
    fn start(fixture: &Value) -> Self {
        let temp = tempfile::tempdir().expect("temporary fixture directory");
        let fixture_path = temp.path().join("fixture.json");
        fs::write(&fixture_path, serde_json::to_vec(fixture).unwrap()).unwrap();
        let codex = temp.path().join("codex");
        fs::write(&codex, include_str!("fixtures/fake-codex.py")).unwrap();
        fs::set_permissions(&codex, fs::Permissions::from_mode(0o755)).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_codex-acp-bridge"))
            .env("PATH", format!("{}:/usr/bin:/bin", temp.path().display()))
            .env("CODEX_FIXTURE", fixture_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start bridge against fake Codex app-server");
        let mut stdout_pipe = BufReader::new(child.stdout.take().unwrap());
        let (sender, stdout) = mpsc::channel();
        thread::spawn(move || {
            let mut line = String::new();
            loop {
                line.clear();
                match stdout_pipe.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) if sender.send(line.clone()).is_err() => break,
                    Ok(_) => {}
                }
            }
        });
        Self {
            _temp: temp,
            child,
            stdout,
        }
    }

    fn send(&mut self, message: Value) {
        writeln!(self.child.stdin.as_mut().unwrap(), "{message}").unwrap();
    }

    fn read_until(&mut self, predicate: impl Fn(&str) -> bool) -> String {
        let mut output = String::new();
        loop {
            let line = self
                .stdout
                .recv_timeout(Duration::from_secs(10))
                .unwrap_or_else(|error| panic!("timed out or lost ACP output ({error}): {output}"));
            output.push_str(&line);
            if predicate(&output) {
                return output;
            }
        }
    }

    fn finish(mut self) {
        drop(self.child.stdin.take());
        let status = self
            .child
            .wait()
            .expect("bridge exits after ACP input closes");
        let mut stderr = String::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        assert!(status.success(), "bridge subprocess failed: {stderr}");
        assert!(stderr.is_empty(), "fake Codex fixture failed: {stderr}");
    }

    fn initialize_and_new_session(&mut self) -> String {
        self.send(
            json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{"protocolVersion":1}}),
        );
        self.send(json!({"jsonrpc":"2.0", "id":2, "method":"session/new", "params":{"cwd":"/tmp", "mcpServers":[]}}));
        self.read_until(|output| output.contains("\"id\":2"))
    }
}

impl Drop for BridgeProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn session_fixture(id: &str, title: &str) -> Value {
    let thread = json!({
        "id":id, "sessionId":id, "forkedFromId":null, "parentThreadId":null,
        "preview":title, "ephemeral":false, "modelProvider":"openai",
        "createdAt":1, "updatedAt":2, "status":{"type":"idle"}, "path":null,
        "cwd":"/tmp", "cliVersion":"fixture", "source":"appServer",
        "threadSource":null, "agentNickname":null, "agentRole":null, "gitInfo":null,
        "name":title, "turns":[]
    });
    let mut resumed_thread = thread.clone();
    resumed_thread["turns"] = json!([{
        "id":"turn-fixture", "itemsView":"full", "status":"completed", "error":null,
        "startedAt":null, "completedAt":null, "durationMs":null,
        "items":[
            {"type":"userMessage", "id":"user-item", "clientId":null,
             "content":[
                {"type":"text", "text":title, "textElements":[]},
                {"type":"image", "detail":null, "url":"data:image/png;base64,aGVsbG8="}
             ]},
            {"type":"agentMessage", "id":"agent-item", "text":title}
        ]
    }]);
    json!({
        "expectedCwd":"/tmp",
        "threadList":{"data":[thread], "nextCursor":null, "backwardsCursor":null},
        "threadResume":{
            "thread":resumed_thread, "model":"gpt-6-luna", "modelProvider":"openai",
            "serviceTier":null, "cwd":"/tmp", "runtimeWorkspaceRoots":[],
            "instructionSources":[], "approvalPolicy":"never", "approvalsReviewer":"user",
            "sandbox":{"type":"workspaceWrite", "writableRoots":[], "networkAccess":false,
                "excludeTmpdirEnvVar":false, "excludeSlashTmp":false},
            "reasoningEffort":null, "initialTurnsPage":null
        }
    })
}

fn turn_fixture(prompt: &str, reply: &str, image_data: &str) -> Value {
    let thread = json!({
        "id":"thread-turn", "sessionId":"thread-turn", "forkedFromId":null,
        "parentThreadId":null, "preview":prompt, "ephemeral":false,
        "modelProvider":"openai", "createdAt":1, "updatedAt":2,
        "status":{"type":"idle"}, "path":null, "cwd":"/tmp",
        "cliVersion":"fixture", "source":"appServer", "threadSource":null,
        "agentNickname":null, "agentRole":null, "gitInfo":null, "name":prompt, "turns":[]
    });
    let sandbox = json!({"type":"workspaceWrite", "writableRoots":[], "networkAccess":false,
        "excludeTmpdirEnvVar":false, "excludeSlashTmp":false});
    json!({
        "threadId":"thread-turn", "turnId":"turn-turn", "reply":reply,
        "expectedInput":[
            {"type":"text", "text":prompt, "text_elements":[]},
            {"type":"image", "detail":null, "url":format!("data:image/png;base64,{image_data}")},
            {"type":"text", "text":"Resource link: requirements (file:///requirements.md)\nPrioritize stable APIs", "text_elements":[]}
        ],
        "threadStart":{"thread":thread, "model":"gpt-6-luna", "modelProvider":"openai",
            "serviceTier":null, "cwd":"/tmp", "runtimeWorkspaceRoots":[],
            "instructionSources":[], "approvalPolicy":"never", "approvalsReviewer":"user",
            "sandbox":sandbox, "reasoningEffort":null},
        "turnStart":{"turn":{"id":"turn-turn", "items":[], "status":"inProgress"}}
    })
}

#[test]
fn session_list_and_load_roundtrip_generated_fixtures() {
    let mut runner = TestRunner::new(Config {
        cases: 12,
        failure_persistence: None,
        ..Config::default()
    });
    runner.run(&("[a-z0-9-]{4,16}", "[A-Za-z0-9 _-]{1,40}"), |(id, title)| {
        let mut fixture = BridgeProcess::start(&session_fixture(&id, &title));
        fixture.send(json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{"protocolVersion":1}}));
        fixture.send(json!({"jsonrpc":"2.0", "id":2, "method":"session/list", "params":{}}));
        fixture.send(json!({"jsonrpc":"2.0", "id":3, "method":"session/load", "params":{"sessionId":id, "cwd":"/tmp", "mcpServers":[]}}));
        let output = fixture.read_until(|output| output.contains("\"id\":2") && output.contains("\"id\":3") && output.matches(&id).count() >= 3 && output.matches(&title).count() >= 3 && output.contains("\"data\":\"aGVsbG8=\""));
        assert!(output.contains(&format!("\"sessionId\":\"{id}\"")), "session missing: {output}");
        assert!(output.matches(&title).count() >= 3, "history messages were not replayed: {output}");
        assert!(output.contains("\"type\":\"image\""), "image history was not replayed: {output}");
        assert!(output.contains("\"data\":\"aGVsbG8=\""), "image history data was lost: {output}");
        fixture.finish();
        Ok(())
    }).expect("generated fixture sessions should roundtrip through ACP and Codex protocols");
}

#[test]
fn embedded_text_and_image_resources_reach_codex() {
    let mut fixture = turn_fixture("resource test", "resource reply", "aGVsbG8=");
    fixture["expectedInput"] = json!([
        {"type":"text", "text":"Embedded resource (text/markdown): file:///guide.md\n\nUse stable APIs", "text_elements":[]},
        {"type":"image", "detail":null, "url":"data:image/png;base64,aGVsbG8="}
    ]);
    let mut bridge = BridgeProcess::start(&fixture);
    let initialized = bridge.initialize_and_new_session();
    assert!(
        initialized.contains("\"embeddedContext\":true"),
        "embedded resource capability missing: {initialized}"
    );
    bridge.send(json!({"jsonrpc":"2.0", "id":3, "method":"session/prompt", "params":{"sessionId":"thread-turn", "prompt":[
        {"type":"resource", "resource":{"uri":"file:///guide.md", "mimeType":"text/markdown", "text":"Use stable APIs"}},
        {"type":"resource", "resource":{"uri":"file:///diagram.png", "mimeType":"image/png", "blob":"aGVsbG8="}}
    ]}}));
    let output = bridge.read_until(|output| output.contains("\"id\":3"));
    bridge.finish();
    assert!(
        output.contains("\"stopReason\":\"end_turn\""),
        "embedded resource prompt did not complete: {output}"
    );
}

#[test]
fn prompt_streams_text_and_forwards_image_fixtures() {
    let mut runner = TestRunner::new(Config {
        cases: 4,
        failure_persistence: None,
        ..Config::default()
    });
    runner.run(&"[a-z0-9 ]{1,24}", |prompt| {
        let fixture = turn_fixture(&prompt, "fixture reply", "aGVsbG8=");
        let mut bridge = BridgeProcess::start(&fixture);
        let initialized = bridge.initialize_and_new_session();
        assert!(initialized.contains("\"image\":true"), "image capability missing: {initialized}");
        bridge.send(json!({"jsonrpc":"2.0", "id":3, "method":"session/prompt", "params":{"sessionId":"thread-turn", "prompt":[{"type":"text","text":prompt},{"type":"image","data":"aGVsbG8=","mimeType":"image/png"},{"type":"resource_link","name":"requirements","uri":"file:///requirements.md","description":"Prioritize stable APIs"}]}}));
        let output = bridge.read_until(|output| output.contains("\"id\":3") && output.contains("fixture reply"));
        assert!(output.contains("\"stopReason\":\"end_turn\""), "prompt did not complete: {output}");
        bridge.finish();
        Ok(())
    }).expect("generated ACP prompts should roundtrip through fake Codex app-server");
}

#[test]
fn cancel_interrupts_the_active_codex_turn() {
    let mut fixture = turn_fixture("cancel test", "unused", "aGVsbG8=");
    fixture["turnBehavior"] = json!("waitForInterrupt");
    let mut bridge = BridgeProcess::start(&fixture);
    let mut output = bridge.initialize_and_new_session();
    bridge.send(json!({"jsonrpc":"2.0", "id":3, "method":"session/prompt", "params":{"sessionId":"thread-turn", "prompt":[{"type":"text","text":"cancel test"},{"type":"image","data":"aGVsbG8=","mimeType":"image/png"},{"type":"resource_link","name":"requirements","uri":"file:///requirements.md","description":"Prioritize stable APIs"}]}}));
    output.push_str(&bridge.read_until(|output| output.contains("ready-to-cancel")));
    bridge.send(
        json!({"jsonrpc":"2.0", "method":"session/cancel", "params":{"sessionId":"thread-turn"}}),
    );
    output.push_str(&bridge.read_until(|output| output.contains("\"id\":3")));
    assert!(
        output.contains("\"stopReason\":\"cancelled\""),
        "prompt should return cancelled: {output}"
    );
    bridge.finish();
}

#[test]
fn command_approval_roundtrips_between_codex_and_acp() {
    let mut fixture = turn_fixture("approval test", "approved fixture reply", "aGVsbG8=");
    fixture["turnBehavior"] = json!("requestApproval");
    let mut bridge = BridgeProcess::start(&fixture);
    let mut output = bridge.initialize_and_new_session();
    bridge.send(json!({"jsonrpc":"2.0", "id":3, "method":"session/prompt", "params":{"sessionId":"thread-turn", "prompt":[{"type":"text","text":"approval test"},{"type":"image","data":"aGVsbG8=","mimeType":"image/png"},{"type":"resource_link","name":"requirements","uri":"file:///requirements.md","description":"Prioritize stable APIs"}]}}));
    let permission = bridge.read_until(|output| output.contains("session/request_permission"));
    output.push_str(&permission);
    let request = permission
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|message| {
            message.get("method").and_then(Value::as_str) == Some("session/request_permission")
        })
        .expect("bridge sends an ACP permission request");
    assert!(
        request["params"]["options"]
            .as_array()
            .is_some_and(|options| options
                .iter()
                .any(|option| option["optionId"] == "allow-once"))
    );
    bridge.send(json!({"jsonrpc":"2.0", "id":request["id"], "result":{"outcome":{"outcome":"selected","optionId":"allow-once"}}}));
    output.push_str(&bridge.read_until(|output| {
        output.contains("\"id\":3") && output.contains("approved fixture reply")
    }));
    assert!(
        output.contains("\"stopReason\":\"end_turn\""),
        "approved turn should complete: {output}"
    );
    bridge.finish();
}
