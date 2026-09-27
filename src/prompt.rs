use agent_client_protocol as acp;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use codex_app_server_protocol::UserInput;

pub fn codex_input(blocks: &[acp::ContentBlock]) -> Result<Vec<UserInput>, String> {
    blocks
        .iter()
        .map(|block| match block {
            acp::ContentBlock::Text(text) => Ok(UserInput::Text {
                text: text.text.clone(),
                text_elements: Vec::new(),
            }),
            acp::ContentBlock::Image(image) => {
                let data = STANDARD
                    .decode(&image.data)
                    .map_err(|error| format!("invalid ACP image data: {error}"))?;
                Ok(UserInput::Image {
                    detail: None,
                    url: format!("data:{};base64,{}", image.mime_type, STANDARD.encode(data)),
                })
            }
            acp::ContentBlock::ResourceLink(resource) => Ok(UserInput::Text {
                text: format!(
                    "Resource link: {} ({}){}",
                    resource.title.as_deref().unwrap_or(&resource.name),
                    resource.uri,
                    resource
                        .description
                        .as_deref()
                        .map(|description| format!("\n{description}"))
                        .unwrap_or_default()
                ),
                text_elements: Vec::new(),
            }),
            acp::ContentBlock::Resource(resource) => match &resource.resource {
                acp::EmbeddedResourceResource::TextResourceContents(resource) => {
                    Ok(UserInput::Text {
                        text: format_resource_text(
                            &resource.uri,
                            resource.mime_type.as_deref(),
                            &resource.text,
                        ),
                        text_elements: Vec::new(),
                    })
                }
                acp::EmbeddedResourceResource::BlobResourceContents(resource) => {
                    let mime_type = resource.mime_type.as_deref().unwrap_or_default();
                    if !mime_type.starts_with("image/") {
                        return Err(format!(
                            "unsupported embedded resource blob MIME type: {mime_type}"
                        ));
                    }
                    let data = STANDARD
                        .decode(&resource.blob)
                        .map_err(|error| format!("invalid embedded image blob: {error}"))?;
                    Ok(UserInput::Image {
                        detail: None,
                        url: format!("data:{mime_type};base64,{}", STANDARD.encode(data)),
                    })
                }
                _ => Err("unsupported embedded resource content".to_owned()),
            },
            _ => Err("unsupported ACP prompt content block".to_owned()),
        })
        .collect()
}

fn format_resource_text(uri: &str, mime_type: Option<&str>, text: &str) -> String {
    let mime_type = mime_type
        .map(|mime_type| format!(" ({mime_type})"))
        .unwrap_or_default();
    format!("Embedded resource{mime_type}: {uri}\n\n{text}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_text_and_converts_image_data_uri() {
        let blocks = [
            acp::ContentBlock::from("describe this"),
            acp::ContentBlock::Image(acp::ImageContent::new("aGVsbG8=", "image/png")),
        ];
        let input = codex_input(&blocks).unwrap();
        assert!(matches!(&input[0], UserInput::Text { text, .. } if text == "describe this"));
        assert!(
            matches!(&input[1], UserInput::Image { url, .. } if url == "data:image/png;base64,aGVsbG8=")
        );
    }

    #[test]
    fn rejects_invalid_image_base64() {
        let blocks = [acp::ContentBlock::Image(acp::ImageContent::new(
            "%%%",
            "image/png",
        ))];
        assert!(codex_input(&blocks).is_err());
    }

    #[test]
    fn converts_embedded_text_and_image_blob() {
        let blocks = [
            serde_json::from_value::<acp::ContentBlock>(serde_json::json!({
                "type": "resource",
                "resource": {
                    "uri": "file:///guide.md",
                    "mimeType": "text/markdown",
                    "text": "Use stable APIs"
                }
            }))
            .unwrap(),
            serde_json::from_value::<acp::ContentBlock>(serde_json::json!({
                "type": "resource",
                "resource": {
                    "uri": "file:///diagram.png",
                    "mimeType": "image/png",
                    "blob": "aGVsbG8="
                }
            }))
            .unwrap(),
        ];

        let input = codex_input(&blocks).unwrap();
        assert!(matches!(&input[0], UserInput::Text { text, .. }
            if text == "Embedded resource (text/markdown): file:///guide.md\n\nUse stable APIs"));
        assert!(matches!(&input[1], UserInput::Image { url, .. }
            if url == "data:image/png;base64,aGVsbG8="));
    }

    #[test]
    fn rejects_non_image_embedded_blobs() {
        let block = serde_json::from_value::<acp::ContentBlock>(serde_json::json!({
            "type": "resource",
            "resource": {
                "uri": "file:///archive.bin",
                "mimeType": "application/octet-stream",
                "blob": "aGVsbG8="
            }
        }))
        .unwrap();

        assert!(
            codex_input(&[block])
                .unwrap_err()
                .contains("unsupported embedded resource blob MIME type")
        );
    }
}
