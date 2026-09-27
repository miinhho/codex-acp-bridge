use acp::{
    SessionUpdate, ToolCall, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields, ToolKind,
};
use agent_client_protocol as acp;
use codex_app_server_protocol::{CommandExecutionStatus, PatchApplyStatus, ThreadItem};

pub(super) fn project_item(item: ThreadItem, started: bool) -> Option<SessionUpdate> {
    match item {
        ThreadItem::CommandExecution {
            id,
            command,
            cwd,
            status,
            aggregated_output,
            exit_code,
            ..
        } => {
            let status = command_status(status);
            if started {
                Some(SessionUpdate::ToolCall(
                    ToolCall::new(id, command.clone())
                        .kind(ToolKind::Execute)
                        .status(status)
                        .raw_input(serde_json::json!({ "command": command, "cwd": cwd })),
                ))
            } else {
                Some(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                    id,
                    ToolCallUpdateFields::new()
                        .status(status)
                        .raw_output(serde_json::json!({
                            "output": aggregated_output,
                            "exitCode": exit_code,
                        })),
                )))
            }
        }
        ThreadItem::FileChange {
            id,
            changes,
            status,
        } => {
            let status = file_change_status(status);
            if started {
                Some(SessionUpdate::ToolCall(
                    ToolCall::new(id, "Apply file changes")
                        .kind(ToolKind::Edit)
                        .status(status)
                        .raw_input(serde_json::json!({ "changes": changes })),
                ))
            } else {
                Some(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                    id,
                    ToolCallUpdateFields::new()
                        .status(status)
                        .raw_output(serde_json::json!({ "changes": changes })),
                )))
            }
        }
        _ => None,
    }
}

fn command_status(status: CommandExecutionStatus) -> ToolCallStatus {
    match status {
        CommandExecutionStatus::InProgress => ToolCallStatus::InProgress,
        CommandExecutionStatus::Completed => ToolCallStatus::Completed,
        CommandExecutionStatus::Failed | CommandExecutionStatus::Declined => ToolCallStatus::Failed,
    }
}

fn file_change_status(status: PatchApplyStatus) -> ToolCallStatus {
    match status {
        PatchApplyStatus::InProgress => ToolCallStatus::InProgress,
        PatchApplyStatus::Completed => ToolCallStatus::Completed,
        PatchApplyStatus::Failed | PatchApplyStatus::Declined => ToolCallStatus::Failed,
    }
}
