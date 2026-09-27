use anyhow::{Result, bail};
use codex_app_server_protocol::{
    AgentMessageDeltaNotification, JSONRPCMessage, TurnCompletedNotification, TurnStartParams,
    TurnStartResponse, TurnStatus,
};
use serde_json::Value;
use tokio::sync::{broadcast, oneshot};

use super::{AppServer, AppServerEvent, ApprovalRequest, TurnRequest, parse_params};

impl AppServer {
    pub async fn run_turn(
        &self,
        request: TurnRequest,
        approvals: tokio::sync::mpsc::UnboundedSender<ApprovalRequest>,
        mut on_started: impl FnMut(String) -> Result<()>,
        mut on_text: impl FnMut(String) -> Result<()>,
        mut on_notification: impl FnMut(String, Option<Value>) -> Result<()>,
    ) -> Result<TurnStatus> {
        let TurnRequest {
            thread_id,
            session_id,
            input,
        } = request;
        let mut events = self.events.subscribe();
        let params = TurnStartParams {
            thread_id: thread_id.clone(),
            input,
            ..TurnStartParams::default()
        };
        let response: TurnStartResponse = self.request("turn/start", &params).await?;
        let turn_id = response.turn.id;
        on_started(turn_id.clone())?;

        loop {
            let message = match events.recv().await {
                Ok(AppServerEvent::Message(message)) => message,
                Ok(AppServerEvent::Closed(reason)) => bail!("{reason}"),
                Err(broadcast::error::RecvError::Lagged(count)) => {
                    bail!("Codex app-server event stream dropped {count} messages")
                }
                Err(broadcast::error::RecvError::Closed) => {
                    bail!("Codex app-server event stream closed")
                }
            };
            match message {
                JSONRPCMessage::Notification(notification)
                    if notification.method == "item/agentMessage/delta" =>
                {
                    let delta: AgentMessageDeltaNotification = parse_params(notification.params)?;
                    if delta.thread_id == thread_id && delta.turn_id == turn_id {
                        on_text(delta.delta)?;
                    }
                }
                JSONRPCMessage::Notification(notification)
                    if notification.method == "turn/completed" =>
                {
                    let completed: TurnCompletedNotification = parse_params(notification.params)?;
                    if completed.thread_id == thread_id && completed.turn.id == turn_id {
                        if completed.turn.status == TurnStatus::Failed {
                            let error = completed
                                .turn
                                .error
                                .map(|error| error.message)
                                .unwrap_or_else(|| "Codex turn failed".into());
                            bail!("{error}");
                        }
                        return Ok(completed.turn.status);
                    }
                }
                JSONRPCMessage::Notification(notification) => {
                    let params = notification.params.clone();
                    let matches_turn = params
                        .as_ref()
                        .and_then(|params| params.get("threadId"))
                        .and_then(Value::as_str)
                        == Some(thread_id.as_str())
                        && params
                            .as_ref()
                            .and_then(|params| params.get("turnId"))
                            .and_then(Value::as_str)
                            == Some(turn_id.as_str());
                    if matches_turn {
                        on_notification(notification.method, params)?;
                    }
                }
                JSONRPCMessage::Request(request) => {
                    let request_thread_id = request
                        .params
                        .as_ref()
                        .and_then(|params| params.get("threadId"))
                        .and_then(Value::as_str);
                    if request_thread_id != Some(thread_id.as_str()) {
                        continue;
                    }

                    let (response_tx, response_rx) = oneshot::channel();
                    approvals
                        .send(ApprovalRequest {
                            session_id: session_id.clone(),
                            method: request.method,
                            params: request.params.unwrap_or(Value::Null),
                            response: response_tx,
                        })
                        .map_err(|_| anyhow::anyhow!("ACP approval handler closed"))?;
                    let response = response_rx
                        .await
                        .map_err(|_| anyhow::anyhow!("ACP approval response was dropped"))?;
                    self.respond_to_server(request.id, response).await?;
                }
                _ => {}
            }
        }
    }
}
