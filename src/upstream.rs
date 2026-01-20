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

                let mut content_parts: Vec<serde_json::Value> = Vec::new();
                let mut has_image = false;

                for part in content {
                    match part {
                        OrsContentPart::InputText { text } => {
                             if !text.is_empty() {
                                 content_parts.push(serde_json::json!({
                                     "type": "text",
                                     "text": text
                                 }));
                             }
                        },
                        OrsContentPart::InputImage { image_url } => {
                            has_image = true;
                            // ORS image_url is already a Value (object or string) matching OpenAI format mostly.
                            // If it's just a string URI, we might need to wrap it.
                            // But types.rs says image_url: Value.
                            // Let's assume it matches OpenAI {"url": "..."} or is compatible.
                            // OpenAI expects: {"type": "image_url", "image_url": {"url": "..."}}
                            
                            content_parts.push(serde_json::json!({
                                "type": "image_url",
                                "image_url": image_url
                            }));
                        }
                    }
                }
                
                let legacy_content = if has_image {
                    Some(serde_json::Value::Array(content_parts))
                } else {
                    // Optimized: simple string if text only (and if only one part? Or strict join?)
                    // OpenAI supports string content. 
                    // Let's join text parts for simple message.
                    let full_text: String = content_parts.iter()
                        .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("");
                        
                    if !full_text.is_empty() {
                        Some(serde_json::Value::String(full_text))
                    } else {
                        None
                    }
                };

                messages.push(LegacyMessage {
                    role: role_str.to_string(),
                    content: legacy_content,
                    tool_calls: None,
                    tool_call_id: None,
                });
            }
            OrsInputItem::FunctionCall { id: _, call_id, name, arguments } => {
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
            OrsInputItem::FunctionCallOutput { id: _, call_id, output } => {
                // ORS FunctionCallOutput maps to a Legacy tool role message
                messages.push(LegacyMessage {
                    role: "tool".to_string(),
                    content: Some(serde_json::Value::String(output)),
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
        assert_eq!(legacy[0].content, Some(serde_json::Value::String("Hello world".to_string())));
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
        assert_eq!(legacy[0].content, Some(serde_json::Value::String("Part 1 Part 2".to_string())));
    }

    #[test]
    fn test_transform_image_multimodal() {
        let input = vec![OrsInputItem::Message {
            role: OrsRole::User,
            content: vec![
                OrsContentPart::InputText { text: "Look at this:".to_string() },
                OrsContentPart::InputImage { image_url: serde_json::json!({"url": "http://img.png"}) },
            ],
        }];

        let legacy = transform_ors_to_legacy(input);
        let content = legacy[0].content.as_ref().unwrap();
        assert!(content.is_array());
        let array = content.as_array().unwrap();
        assert_eq!(array.len(), 2);
        assert_eq!(array[0]["type"], "text");
        assert_eq!(array[1]["type"], "image_url");
        assert_eq!(array[1]["image_url"]["url"], "http://img.png");
    }

    #[test]
    fn test_transform_tool_calls() {
        let input = vec![
            OrsInputItem::FunctionCall {
                id: "item_1".to_string(),
                call_id: "call_abc".to_string(),
                name: "get_weather".to_string(),
                arguments: serde_json::json!({"city": "SF"}),
            },
            OrsInputItem::FunctionCallOutput {
                id: "item_2".to_string(),
                call_id: "call_abc".to_string(),
                output: "Sunny".to_string(),
            }
        ];

        let legacy = transform_ors_to_legacy(input);
        assert_eq!(legacy.len(), 2);
        
        // Tool Call (Assistant)
        assert_eq!(legacy[0].role, "assistant");
        let tc = legacy[0].tool_calls.as_ref().unwrap();
        assert_eq!(tc[0]["id"], "call_abc");
        assert_eq!(tc[0]["function"]["name"], "get_weather");

        // Tool Output (Tool)
        assert_eq!(legacy[1].role, "tool");
        assert_eq!(legacy[1].tool_call_id.as_deref(), Some("call_abc"));
        assert_eq!(legacy[1].content.as_ref().unwrap().as_str(), Some("Sunny"));
    }
}
