use acp::{
    Client as _, PermissionOption, PermissionOptionKind, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, ToolCallStatus, ToolCallUpdate,
    ToolCallUpdateFields,
};
use agent_client_protocol as acp;
use codex_app_server_protocol::{
    CommandExecutionApprovalDecision, CommandExecutionRequestApprovalParams,
    CommandExecutionRequestApprovalResponse, FileChangeApprovalDecision,
    FileChangeRequestApprovalParams, FileChangeRequestApprovalResponse,
};
use serde::Serialize;
use serde_json::Value;

pub(super) async fn handle_approval(
    client: &acp::AgentSideConnection,
    method: &str,
    session_id: String,
    params: Value,
) -> Result<Value, String> {
    match method {
        "item/commandExecution/requestApproval" => {
            let params: CommandExecutionRequestApprovalParams = serde_json::from_value(params)
                .map_err(|error| format!("invalid command approval request: {error}"))?;
            let title = params
                .command
                .clone()
                .unwrap_or_else(|| "Run command".into());
            let details = serde_json::json!({
                "command": params.command,
                "cwd": params.cwd,
                "reason": params.reason,
                "proposedExecpolicyAmendment": params.proposed_execpolicy_amendment,
                "proposedNetworkPolicyAmendments": params.proposed_network_policy_amendments,
            });
            let choices = command_decision_options(params.available_decisions.as_deref());
            let options = choices
                .iter()
                .map(|(option, _)| option.clone())
                .collect::<Vec<_>>();
            if options.is_empty() {
                return Err(
                    "Codex offered no command approval decisions supported by this bridge".into(),
                );
            }
            let response =
                request_permission(client, session_id, params.item_id, &title, details, options)
                    .await?;
            let decision = match response {
                RequestPermissionOutcome::Cancelled => CommandExecutionApprovalDecision::Cancel,
                RequestPermissionOutcome::Selected(selection) => {
                    command_decision_for_option(choices, &selection.option_id.to_string())
                }
                _ => CommandExecutionApprovalDecision::Decline,
            };
            to_value(CommandExecutionRequestApprovalResponse { decision })
        }
        "item/fileChange/requestApproval" => {
            let params: FileChangeRequestApprovalParams = serde_json::from_value(params)
                .map_err(|error| format!("invalid file change approval request: {error}"))?;
            let title = params
                .reason
                .clone()
                .unwrap_or_else(|| "Codex requests permission to apply file changes".into());
            let details = serde_json::json!({
                "reason": params.reason,
                "grantRoot": params.grant_root,
            });
            let response = request_permission(
                client,
                session_id,
                params.item_id,
                &title,
                details,
                permission_options(true, true, true),
            )
            .await?;
            let decision = match response {
                RequestPermissionOutcome::Cancelled => FileChangeApprovalDecision::Cancel,
                RequestPermissionOutcome::Selected(selection) => {
                    match selection.option_id.to_string().as_str() {
                        "allow-once" => FileChangeApprovalDecision::Accept,
                        "allow-session" => FileChangeApprovalDecision::AcceptForSession,
                        _ => FileChangeApprovalDecision::Decline,
                    }
                }
                _ => FileChangeApprovalDecision::Decline,
            };
            to_value(FileChangeRequestApprovalResponse { decision })
        }
        _ => Err(format!("unsupported app-server request: {method}")),
    }
}

async fn request_permission(
    client: &acp::AgentSideConnection,
    session_id: String,
    item_id: String,
    title: &str,
    details: Value,
    options: Vec<PermissionOption>,
) -> Result<RequestPermissionOutcome, String> {
    let tool_call = ToolCallUpdate::new(
        item_id,
        ToolCallUpdateFields::new()
            .title(title)
            .status(ToolCallStatus::Pending)
            .raw_input(details),
    );
    let response: RequestPermissionResponse = client
        .request_permission(RequestPermissionRequest::new(
            session_id, tool_call, options,
        ))
        .await
        .map_err(|error| error.to_string())?;
    Ok(response.outcome)
}

fn permission_options(allow_once: bool, allow_session: bool, deny: bool) -> Vec<PermissionOption> {
    let mut options = Vec::with_capacity(3);
    if allow_once {
        options.push(PermissionOption::new(
            "allow-once",
            "Allow once",
            PermissionOptionKind::AllowOnce,
        ));
    }
    if allow_session {
        options.push(PermissionOption::new(
            "allow-session",
            "Allow for session",
            PermissionOptionKind::AllowAlways,
        ));
    }
    if deny {
        options.push(PermissionOption::new(
            "deny",
            "Deny",
            PermissionOptionKind::RejectOnce,
        ));
    }
    options
}

fn command_decision_options(
    decisions: Option<&[CommandExecutionApprovalDecision]>,
) -> Vec<(PermissionOption, CommandExecutionApprovalDecision)> {
    use CommandExecutionApprovalDecision as Decision;

    let default_decisions = [
        Decision::Accept,
        Decision::AcceptForSession,
        Decision::Decline,
    ];
    let decisions = decisions.unwrap_or(&default_decisions);
    decisions
        .iter()
        .enumerate()
        .map(|(index, decision)| {
            let (id, title, kind) = match decision {
                Decision::Accept => (
                    "allow-once".to_owned(),
                    "Allow once".to_owned(),
                    PermissionOptionKind::AllowOnce,
                ),
                Decision::AcceptForSession => (
                    "allow-session".to_owned(),
                    "Allow for session".to_owned(),
                    PermissionOptionKind::AllowAlways,
                ),
                Decision::AcceptWithExecpolicyAmendment { .. } => (
                    "allow-execpolicy-amendment".to_owned(),
                    "Allow and remember this command pattern".to_owned(),
                    PermissionOptionKind::AllowAlways,
                ),
                Decision::ApplyNetworkPolicyAmendment { .. } => (
                    format!("allow-network-policy-{index}"),
                    "Apply network policy change".to_owned(),
                    PermissionOptionKind::AllowAlways,
                ),
                Decision::Decline => (
                    "deny".to_owned(),
                    "Deny".to_owned(),
                    PermissionOptionKind::RejectOnce,
                ),
                Decision::Cancel => (
                    "cancel-turn".to_owned(),
                    "Cancel turn".to_owned(),
                    PermissionOptionKind::RejectOnce,
                ),
            };
            (PermissionOption::new(id, title, kind), decision.clone())
        })
        .collect()
}

fn command_decision_for_option(
    choices: Vec<(PermissionOption, CommandExecutionApprovalDecision)>,
    selected_option_id: &str,
) -> CommandExecutionApprovalDecision {
    choices
        .into_iter()
        .find(|(option, _)| option.option_id.to_string() == selected_option_id)
        .map(|(_, decision)| decision)
        .unwrap_or(CommandExecutionApprovalDecision::Decline)
}

fn to_value(value: impl Serialize) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_app_server_protocol::{ExecPolicyAmendment, NetworkPolicyAmendment};

    #[test]
    fn command_options_preserve_all_offered_decisions() {
        use CommandExecutionApprovalDecision as Decision;

        let network_decision = Decision::ApplyNetworkPolicyAmendment {
            network_policy_amendment: NetworkPolicyAmendment {
                host: "example.com".into(),
                action: codex_app_server_protocol::NetworkPolicyRuleAction::Allow,
            },
        };
        let offered = vec![
            Decision::Accept,
            Decision::AcceptForSession,
            Decision::AcceptWithExecpolicyAmendment {
                execpolicy_amendment: ExecPolicyAmendment {
                    command: vec!["printf".into(), "ok".into()],
                },
            },
            network_decision.clone(),
            Decision::Decline,
            Decision::Cancel,
        ];
        let choices = command_decision_options(Some(&offered));
        let option_ids = choices
            .iter()
            .map(|(option, _)| option.option_id.to_string())
            .collect::<Vec<_>>();

        assert_eq!(
            option_ids,
            [
                "allow-once",
                "allow-session",
                "allow-execpolicy-amendment",
                "allow-network-policy-3",
                "deny",
                "cancel-turn",
            ]
        );
        assert_eq!(
            command_decision_for_option(choices, "allow-network-policy-3"),
            network_decision
        );
    }

    #[test]
    fn unknown_command_option_fails_closed() {
        let choices = command_decision_options(None);
        assert_eq!(
            command_decision_for_option(choices, "unknown"),
            CommandExecutionApprovalDecision::Decline
        );
    }

    #[test]
    fn command_approval_request_decodes_protocol_amendments() {
        let request: CommandExecutionRequestApprovalParams = serde_json::from_value(
            serde_json::json!({
                "threadId": "thread-1",
                "turnId": "turn-1",
                "itemId": "item-1",
                "startedAtMs": 0,
                "command": "printf safe",
                "cwd": "/workspace",
                "availableDecisions": [
                    "accept",
                    "acceptForSession",
                    {"acceptWithExecpolicyAmendment": {"execpolicy_amendment": ["printf", "safe"]}},
                    {"applyNetworkPolicyAmendment": {"network_policy_amendment": {"host": "example.com", "action": "allow"}}},
                    "decline",
                    "cancel"
                ]
            }),
        )
        .expect("Codex approval decisions should decode");

        let choices = command_decision_options(request.available_decisions.as_deref());
        let ids = choices
            .iter()
            .map(|(option, _)| option.option_id.to_string())
            .collect::<Vec<_>>();
        assert_eq!(ids.len(), 6);
        assert!(ids.contains(&"allow-execpolicy-amendment".to_owned()));
        assert!(ids.contains(&"allow-network-policy-3".to_owned()));
    }
}
