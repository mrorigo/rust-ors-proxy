use crate::types::{LegacyMessage, OrsContentPart, OrsInputItem, OrsRole};

pub fn transform_ors_to_legacy(input: Vec<OrsInputItem>) -> Vec<LegacyMessage> {
    let mut messages = Vec::new();

    for item in input {
        match item {
            OrsInputItem::Message { role, content } => {
                let role_str = match role {
                    OrsRole::User => "user",
                    OrsRole::Assistant => "assistant",
                    OrsRole::Developer => "system",
                };

                let mut text_content = String::new();
                for part in content {
                    match part {
                        OrsContentPart::InputText { text } => text_content.push_str(&text),
                        OrsContentPart::InputImage { .. } => {
                            // Warn or ignore for now?
                        }
                    }
                }

                messages.push(LegacyMessage {
                    role: role_str.to_string(),
                    content: Some(text_content),
                    tool_calls: None,
                    tool_call_id: None,
                });
            }
            OrsInputItem::FunctionCall { id, call_id, name, arguments } => {
                // ORS FunctionCall maps to a Legacy assistant message with tool_calls
                messages.push(LegacyMessage {
                    role: "assistant".to_string(),
                    content: None, // usually null for tool calls
                    tool_calls: Some(vec![serde_json::json!({
                        "id": call_id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": arguments.to_string() // arguments is Value, legacy expects stringified JSON?
                            // Wait, legacy `tool_calls` `function.arguments` is usually a string.
                            // But LegacyMessage.tool_calls is `Option<Vec<Value>>`.
                            // Let's check typical OpenAI format:
                            // "tool_calls": [{"id": "...", "type": "function", "function": {"name": "...", "arguments": "{...}"}}]
                        }
                    })]),
                    tool_call_id: None,
                });
            }
            OrsInputItem::FunctionCallOutput { id, call_id, output } => {
                // ORS FunctionCallOutput maps to a Legacy tool role message
                messages.push(LegacyMessage {
                    role: "tool".to_string(),
                    content: Some(output),
                    tool_calls: None,
                    tool_call_id: Some(call_id),
                });
            }
        }
    }
    messages
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
