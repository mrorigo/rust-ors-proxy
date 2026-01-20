use crate::types::{LegacyMessage, OrsContentPart, OrsInputItem, OrsRole};

pub fn transform_ors_to_legacy(input: Vec<OrsInputItem>) -> Vec<LegacyMessage> {
    input
        .into_iter()
        .filter_map(|item| match item {
            OrsInputItem::Message { role, content } => {
                let role_str = match role {
                    OrsRole::User => "user",
                    OrsRole::Assistant => "assistant",
                    OrsRole::Developer => "system",
                };

                // Concatenate all text parts.
                // TODO: Support multimodal content array if upstream supports it (requires updating LegacyMessage struct).
                let mut text_content = String::new();
                for part in content {
                    match part {
                        OrsContentPart::InputText { text } => text_content.push_str(&text),
                        OrsContentPart::InputImage { .. } => {
                            // Warn or ignore for now?
                            tracing::warn!("Ignoring image content in legacy transformation (not yet supported)");
                        }
                    }
                }

                Some(LegacyMessage {
                    role: role_str.to_string(),
                    content: Some(text_content),
                    tool_calls: None,
                })
            }
            OrsInputItem::FunctionCall { .. } => {
                // TODO: Implement Function Call mapping
                None
            }
            OrsInputItem::FunctionCallOutput { .. } => {
                // TODO: Implement Function Call Output mapping
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{OrsContentPart, OrsInputItem, OrsRole};

    #[test]
    fn test_transform_simple_message() {
        let input = vec![OrsInputItem::Message {
            role: OrsRole::User,
            content: vec![OrsContentPart::InputText {
                text: "Hello world".to_string(),
            }],
        }];

        let legacy = transform_ors_to_legacy(input);
        assert_eq!(legacy.len(), 1);
        assert_eq!(legacy[0].role, "user");
        assert_eq!(legacy[0].content.as_deref(), Some("Hello world"));
    }

    #[test]
    fn test_transform_developer_role() {
        let input = vec![OrsInputItem::Message {
            role: OrsRole::Developer,
            content: vec![OrsContentPart::InputText {
                text: "System prompt".to_string(),
            }],
        }];

        let legacy = transform_ors_to_legacy(input);
        assert_eq!(legacy.len(), 1);
        assert_eq!(legacy[0].role, "system");
    }

    #[test]
    fn test_transform_multi_part_text() {
        let input = vec![OrsInputItem::Message {
            role: OrsRole::User,
            content: vec![
                OrsContentPart::InputText { text: "Part 1 ".to_string() },
                OrsContentPart::InputText { text: "Part 2".to_string() },
            ],
        }];

        let legacy = transform_ors_to_legacy(input);
        assert_eq!(legacy[0].content.as_deref(), Some("Part 1 Part 2"));
    }
}
