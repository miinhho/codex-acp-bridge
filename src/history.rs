use acp::{ContentBlock, ContentChunk, SessionUpdate};
use agent_client_protocol as acp;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use codex_app_server_protocol::{ThreadItem, Turn};

pub fn session_updates(turns: impl IntoIterator<Item = Turn>) -> Vec<SessionUpdate> {
    let mut updates = Vec::new();
    for item in turns.into_iter().flat_map(|turn| turn.items) {
        match item {
            ThreadItem::UserMessage { content, .. } => {
                let blocks = content
                    .into_iter()
                    .flat_map(user_input_blocks)
                    .collect::<Vec<_>>();
                for block in blocks {
                    updates.push(SessionUpdate::UserMessageChunk(ContentChunk::new(block)));
                }
            }
            ThreadItem::AgentMessage { text, .. } => {
                updates.push(SessionUpdate::AgentMessageChunk(ContentChunk::new(
                    ContentBlock::from(text),
                )));
            }
            ThreadItem::Reasoning { summary, .. } => {
                let text = summary.join("\n");
                if !text.is_empty() {
                    updates.push(SessionUpdate::AgentThoughtChunk(ContentChunk::new(
                        ContentBlock::from(text),
                    )));
                }
            }
            item => {
                if let Some(update) = crate::projection::project_item(item.clone(), true) {
                    updates.push(update);
                }
                if let Some(update) = crate::projection::project_item(item, false) {
                    updates.push(update);
                }
            }
        }
    }
    updates
}

fn user_input_blocks(input: codex_app_server_protocol::UserInput) -> Vec<ContentBlock> {
    use codex_app_server_protocol::UserInput;

    match input {
        UserInput::Text { text, .. } => {
            if text.is_empty() {
                Vec::new()
            } else {
                vec![ContentBlock::from(text)]
            }
        }
        UserInput::Image { url, .. } => match image_from_data_url(&url) {
            Ok(image) => vec![image],
            Err(reason) => vec![ContentBlock::from(format!(
                "[Image from session history could not be restored: {reason}]"
            ))],
        },
        UserInput::LocalImage { path, .. } => vec![ContentBlock::from(format!(
            "[Image from session history is stored at: {}]",
            path.display()
        ))],
        _ => Vec::new(),
    }
}

fn image_from_data_url(url: &str) -> Result<ContentBlock, String> {
    let data_url = url
        .strip_prefix("data:")
        .ok_or_else(|| "Codex stored a non-embedded image URL".to_owned())?;
    let (metadata, encoded) = data_url
        .split_once(',')
        .ok_or_else(|| "Codex stored an invalid image data URL".to_owned())?;
    let mut metadata_parts = metadata.split(';');
    let mime_type = metadata_parts.next().unwrap_or_default();
    if !mime_type.starts_with("image/") {
        return Err(format!("unexpected image MIME type {mime_type}"));
    }
    if !metadata_parts.any(|part| part == "base64") {
        return Err("Codex stored an image URL that is not base64 encoded".to_owned());
    }
    let data = STANDARD
        .decode(encoded)
        .map_err(|error| format!("Codex stored invalid base64 image data: {error}"))?;
    Ok(ContentBlock::Image(acp::ImageContent::new(
        STANDARD.encode(data),
        mime_type,
    )))
}
