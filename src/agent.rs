use std::sync::Mutex;

use acp::{Client as _, ContentBlock, SessionUpdate};
use agent_client_protocol as acp;
use codex_app_server_protocol::{ItemCompletedNotification, ItemStartedNotification};
use serde_json::Value;
use tokio::{sync::mpsc, task::LocalSet};
use tokio_util::compat::{TokioAsyncReadCompatExt as _, TokioAsyncWriteCompatExt as _};

use crate::codex::{AppServer, ApprovalRequest, TurnRequest};

struct Bridge {
    app_server: AppServer,
    active_turns: Mutex<std::collections::HashMap<acp::SessionId, String>>,
    updates: mpsc::UnboundedSender<acp::SessionNotification>,
    approvals: mpsc::UnboundedSender<ApprovalRequest>,
}

impl Bridge {
    fn new(
        app_server: AppServer,
        updates: mpsc::UnboundedSender<acp::SessionNotification>,
        approvals: mpsc::UnboundedSender<ApprovalRequest>,
    ) -> Self {
        Self {
            app_server,
            active_turns: Mutex::default(),
            updates,
            approvals,
        }
    }
}

#[async_trait::async_trait(?Send)]
impl acp::Agent for Bridge {
    async fn initialize(
        &self,
        request: acp::InitializeRequest,
    ) -> acp::Result<acp::InitializeResponse> {
        Ok(acp::InitializeResponse::new(request.protocol_version)
            .agent_info(
                acp::Implementation::new("codex-acp-bridge", env!("CARGO_PKG_VERSION"))
                    .title("Codex ACP Bridge"),
            )
            .agent_capabilities(
                acp::AgentCapabilities::new()
                    .load_session(true)
                    .prompt_capabilities(
                        acp::PromptCapabilities::new()
                            .image(true)
                            .embedded_context(true),
                    )
                    .session_capabilities(
                        acp::SessionCapabilities::new()
                            .list(Some(acp::SessionListCapabilities::new())),
                    ),
            ))
    }

    async fn authenticate(
        &self,
        _request: acp::AuthenticateRequest,
    ) -> acp::Result<acp::AuthenticateResponse> {
        Ok(acp::AuthenticateResponse::default())
    }

    async fn new_session(
        &self,
        request: acp::NewSessionRequest,
    ) -> acp::Result<acp::NewSessionResponse> {
        let cwd = request.cwd.to_string_lossy().into_owned();
        let thread_id = self
            .app_server
            .start_thread(cwd)
            .await
            .map_err(internal_error)?;
        Ok(acp::NewSessionResponse::new(acp::SessionId::new(thread_id)))
    }

    async fn load_session(
        &self,
        request: acp::LoadSessionRequest,
    ) -> acp::Result<acp::LoadSessionResponse> {
        let session_id = request.session_id.clone();
        let resumed = self
            .app_server
            .resume_thread(
                session_id.to_string(),
                request.cwd.to_string_lossy().into_owned(),
            )
            .await
            .map_err(|error| internal_error(format!("{error:#}")))?;
        for update in crate::history::session_updates(resumed.thread.turns) {
            self.updates
                .send(acp::SessionNotification::new(session_id.clone(), update))
                .map_err(internal_error)?;
        }
        Ok(acp::LoadSessionResponse::default())
    }

    async fn list_sessions(
        &self,
        request: acp::ListSessionsRequest,
    ) -> acp::Result<acp::ListSessionsResponse> {
        let cwd = request.cwd.map(|path| path.to_string_lossy().into_owned());
        let response = self
            .app_server
            .list_threads(request.cursor, cwd)
            .await
            .map_err(internal_error)?;
        let sessions = response
            .data
            .into_iter()
            .map(|thread| {
                acp::SessionInfo::new(thread.id, thread.cwd.into_path_buf())
                    .title(thread.name.unwrap_or(thread.preview))
            })
            .collect();
        Ok(acp::ListSessionsResponse::new(sessions).next_cursor(response.next_cursor))
    }

    async fn prompt(&self, request: acp::PromptRequest) -> acp::Result<acp::PromptResponse> {
        let thread_id = request.session_id.to_string();

        let prompt = crate::prompt::codex_input(&request.prompt)
            .map_err(|_| acp::Error::invalid_params())?;

        let updates = self.updates.clone();
        let session_id = request.session_id;
        let cleanup_session_id = session_id.clone();
        let approvals = self.approvals.clone();
        let codex_session_id = session_id.to_string();
        let turn_result = self
            .app_server
            .run_turn(
                TurnRequest {
                    thread_id: thread_id.clone(),
                    session_id: codex_session_id,
                    input: prompt,
                },
                approvals,
                {
                    let session_id = session_id.clone();
                    move |turn_id| {
                        self.active_turns
                            .lock()
                            .map_err(|_| anyhow::anyhow!("active turn lock poisoned"))?
                            .insert(session_id.clone(), turn_id);
                        Ok(())
                    }
                },
                move |text| {
                    updates
                        .send(acp::SessionNotification::new(
                            session_id.clone(),
                            SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(
                                ContentBlock::from(text),
                            )),
                        ))
                        .map_err(|_| anyhow::anyhow!("ACP connection closed"))
                },
                {
                    let session_id = cleanup_session_id.clone();
                    let updates = self.updates.clone();
                    move |method, params| {
                        let item_update = match method.as_str() {
                            "item/started" => {
                                let event: ItemStartedNotification =
                                    serde_json::from_value(params.unwrap_or(Value::Null))?;
                                crate::projection::project_item(event.item, true)
                            }
                            "item/completed" => {
                                let event: ItemCompletedNotification =
                                    serde_json::from_value(params.unwrap_or(Value::Null))?;
                                crate::projection::project_item(event.item, false)
                            }
                            _ => None,
                        };
                        if let Some(update) = item_update {
                            updates
                                .send(acp::SessionNotification::new(session_id.clone(), update))
                                .map_err(|_| anyhow::anyhow!("ACP connection closed"))?;
                        }
                        Ok(())
                    }
                },
            )
            .await;
        self.active_turns
            .lock()
            .map_err(|_| acp::Error::internal_error())?
            .remove(&cleanup_session_id);
        let status = turn_result.map_err(internal_error)?;

        match status {
            codex_app_server_protocol::TurnStatus::Completed => {
                Ok(acp::PromptResponse::new(acp::StopReason::EndTurn))
            }
            codex_app_server_protocol::TurnStatus::Interrupted => {
                Ok(acp::PromptResponse::new(acp::StopReason::Cancelled))
            }
            codex_app_server_protocol::TurnStatus::Failed => {
                Err(internal_error("Codex turn failed"))
            }
            codex_app_server_protocol::TurnStatus::InProgress => Err(internal_error(
                "Codex reported a turn as completed while it is still running",
            )),
        }
    }

    async fn cancel(&self, request: acp::CancelNotification) -> acp::Result<()> {
        let active_turn = self
            .active_turns
            .lock()
            .map_err(|_| acp::Error::internal_error())?
            .get(&request.session_id)
            .cloned();
        if let Some(active_turn) = active_turn {
            self.app_server
                .interrupt(request.session_id.to_string(), active_turn)
                .await
                .map_err(internal_error)?;
        }
        Ok(())
    }
}

fn internal_error(error: impl std::fmt::Display) -> acp::Error {
    acp::Error::new(-32603, error.to_string())
}

pub async fn serve(app_server: AppServer) -> acp::Result<()> {
    let outgoing = tokio::io::stdout().compat_write();
    let incoming = tokio::io::stdin().compat();
    let local_set = LocalSet::new();
    local_set
        .run_until(async move {
            let (update_tx, mut update_rx) = mpsc::unbounded_channel();
            let (approval_tx, mut approval_rx) = mpsc::unbounded_channel();
            let bridge = Bridge::new(app_server, update_tx, approval_tx);
            let (connection, handle_io) =
                acp::AgentSideConnection::new(bridge, outgoing, incoming, |future| {
                    tokio::task::spawn_local(future);
                });
            let connection = std::sync::Arc::new(connection);

            tokio::task::spawn_local(async move {
                loop {
                    tokio::select! {
                        Some(update) = update_rx.recv() => {
                            if let Err(error) = connection.session_notification(update).await {
                                eprintln!("failed to send ACP session update: {error}");
                                break;
                            }
                        }
                        Some(request) = approval_rx.recv() => {
                            let ApprovalRequest { session_id, method, params, response } = request;
                            let result = crate::approval::handle_approval(&connection, method.as_str(),
                                session_id, params).await;
                            let _ = response.send(result);
                        }
                        else => break,
                    }
                }
            });

            handle_io.await.map_err(internal_error)
        })
        .await
}
