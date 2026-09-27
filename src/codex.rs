use std::{
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, Ordering},
    },
};

use anyhow::{Context, Result};
use codex_app_server_protocol::{
    ClientInfo, InitializeParams, InitializeResponse, JSONRPCError, JSONRPCErrorError,
    JSONRPCMessage, JSONRPCNotification, JSONRPCRequest, JSONRPCResponse, RequestId,
    ThreadListCwdFilter, ThreadListParams, ThreadListResponse, ThreadResumeParams,
    ThreadResumeResponse, ThreadStartParams, ThreadStartResponse, TurnInterruptParams, UserInput,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{broadcast, oneshot},
};

mod turn;

type Pending =
    Arc<Mutex<std::collections::HashMap<RequestId, oneshot::Sender<Result<Value, String>>>>>;

#[derive(Clone)]
enum AppServerEvent {
    Message(JSONRPCMessage),
    Closed(String),
}

pub struct ApprovalRequest {
    pub session_id: String,
    pub method: String,
    pub params: Value,
    pub response: oneshot::Sender<Result<Value, String>>,
}

pub struct TurnRequest {
    pub thread_id: String,
    pub session_id: String,
    pub input: Vec<UserInput>,
}

pub struct AppServer {
    _child: Child,
    stdin: Arc<tokio::sync::Mutex<ChildStdin>>,
    pending: Pending,
    next_id: AtomicI64,
    events: broadcast::Sender<AppServerEvent>,
}

impl AppServer {
    pub async fn spawn() -> Result<Self> {
        let mut child = Command::new("codex")
            .args(["app-server", "--stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .context("failed to start `codex app-server --stdio`")?;

        let stdin = child
            .stdin
            .take()
            .context("app-server stdin was not piped")?;
        let stdout = child
            .stdout
            .take()
            .context("app-server stdout was not piped")?;
        let (events, _) = broadcast::channel(256);
        let pending = Arc::new(Mutex::new(std::collections::HashMap::new()));
        tokio::spawn(read_messages(stdout, pending.clone(), events.clone()));

        let app_server = Self {
            _child: child,
            stdin: Arc::new(tokio::sync::Mutex::new(stdin)),
            pending,
            next_id: AtomicI64::new(1),
            events,
        };

        let initialize = InitializeParams {
            client_info: ClientInfo {
                name: "codex_acp_bridge".into(),
                title: Some("Codex ACP Bridge".into()),
                version: env!("CARGO_PKG_VERSION").into(),
            },
            capabilities: None,
        };
        let _: InitializeResponse = app_server.request("initialize", &initialize).await?;
        app_server
            .notify("initialized", &serde_json::json!({}))
            .await?;
        Ok(app_server)
    }

    pub async fn start_thread(&self, cwd: String) -> Result<String> {
        let params = ThreadStartParams {
            model: std::env::var("CODEX_ACP_MODEL").ok(),
            cwd: Some(cwd),
            ..ThreadStartParams::default()
        };
        let result: ThreadStartResponse = self.request("thread/start", &params).await?;
        Ok(result.thread.id.to_string())
    }

    pub async fn resume_thread(
        &self,
        thread_id: String,
        cwd: String,
    ) -> Result<ThreadResumeResponse> {
        self.request(
            "thread/resume",
            &ThreadResumeParams {
                thread_id,
                cwd: Some(cwd),
                ..Default::default()
            },
        )
        .await
    }

    pub async fn list_threads(
        &self,
        cursor: Option<String>,
        cwd: Option<String>,
    ) -> Result<ThreadListResponse> {
        let params = ThreadListParams {
            cursor,
            limit: None,
            sort_key: None,
            sort_direction: None,
            model_providers: None,
            source_kinds: None,
            archived: Some(false),
            cwd: cwd.map(ThreadListCwdFilter::One),
            use_state_db_only: false,
            search_term: None,
        };
        self.request("thread/list", &params).await
    }

    pub async fn interrupt(&self, thread_id: String, turn_id: String) -> Result<()> {
        let params = TurnInterruptParams { thread_id, turn_id };
        let _: Value = self.request("turn/interrupt", &params).await?;
        Ok(())
    }

    async fn respond_to_server(
        &self,
        id: RequestId,
        response: Result<Value, String>,
    ) -> Result<()> {
        let message = match response {
            Ok(result) => JSONRPCMessage::Response(JSONRPCResponse { id, result }),
            Err(message) => JSONRPCMessage::Error(JSONRPCError {
                id,
                error: JSONRPCErrorError {
                    code: -32601,
                    message,
                    data: None,
                },
            }),
        };
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(&serde_json::to_vec(&message)?).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(())
    }

    async fn request<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<R> {
        let id = RequestId::Integer(self.next_id.fetch_add(1, Ordering::Relaxed));
        let (sender, receiver) = oneshot::channel();
        self.pending
            .lock()
            .map_err(|_| anyhow::anyhow!("app-server pending request lock poisoned"))?
            .insert(id.clone(), sender);

        let message = JSONRPCMessage::Request(JSONRPCRequest {
            id: id.clone(),
            method: method.to_owned(),
            params: Some(serde_json::to_value(params)?),
            trace: None,
        });
        let write_result = async {
            let mut stdin = self.stdin.lock().await;
            stdin.write_all(&serde_json::to_vec(&message)?).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await?;
            Result::<()>::Ok(())
        }
        .await;
        if let Err(error) = write_result {
            self.pending
                .lock()
                .map_err(|_| anyhow::anyhow!("app-server pending request lock poisoned"))?
                .remove(&id);
            return Err(error);
        }

        let response = receiver
            .await
            .map_err(|_| anyhow::anyhow!("Codex app-server closed before replying to {method}"))?
            .map_err(anyhow::Error::msg)?;
        serde_json::from_value(response)
            .map_err(|error| anyhow::anyhow!("invalid response to {method}: {error}"))
    }

    async fn notify<P: Serialize>(&self, method: &str, params: &P) -> Result<()> {
        let message = JSONRPCMessage::Notification(JSONRPCNotification {
            method: method.to_owned(),
            params: Some(serde_json::to_value(params)?),
        });
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(&serde_json::to_vec(&message)?).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(())
    }
}

async fn read_messages(
    stdout: ChildStdout,
    pending: Pending,
    events: broadcast::Sender<AppServerEvent>,
) {
    let mut stdout = BufReader::new(stdout);
    let mut line = String::new();
    let reason = loop {
        line.clear();
        let bytes = match stdout.read_line(&mut line).await {
            Ok(bytes) => bytes,
            Err(error) => break format!("failed reading Codex app-server output: {error}"),
        };
        if bytes == 0 {
            break "Codex app-server closed stdout".to_owned();
        }
        let message = match serde_json::from_str::<JSONRPCMessage>(&line) {
            Ok(message) => message,
            Err(error) => break format!("invalid Codex app-server JSON-RPC message: {error}"),
        };

        match &message {
            JSONRPCMessage::Response(response) => {
                send_response(&pending, &response.id, Ok(response.result.clone()));
            }
            JSONRPCMessage::Error(error) => {
                send_response(&pending, &error.id, Err(error_message(error)));
            }
            JSONRPCMessage::Notification(_) | JSONRPCMessage::Request(_) => {
                let _ = events.send(AppServerEvent::Message(message));
            }
        }
    };

    if let Ok(mut pending) = pending.lock() {
        for (_, sender) in pending.drain() {
            let _ = sender.send(Err(reason.clone()));
        }
    }
    let _ = events.send(AppServerEvent::Closed(reason));
}

fn send_response(pending: &Pending, id: &RequestId, result: Result<Value, String>) {
    if let Ok(mut pending) = pending.lock()
        && let Some(sender) = pending.remove(id)
    {
        let _ = sender.send(result);
    }
}

fn error_message(error: &JSONRPCError) -> String {
    format!(
        "Codex app-server error {}: {}",
        error.error.code, error.error.message
    )
}

fn parse_params<T: DeserializeOwned>(params: Option<Value>) -> Result<T> {
    serde_json::from_value(params.unwrap_or(Value::Null)).context("invalid notification params")
}
