use crate::types::{LegacyChunk, OrsEvent};
use uuid::Uuid;

pub struct Transcoder {
    response_id: String,
    current_item_id: Option<String>,
    state: TranscoderState,
}

enum TranscoderState {
    Init,
    Streaming,
    // Done, // Not strictly needed if we just stop processing
}

impl Transcoder {
    pub fn new() -> Self {
        Self {
            response_id: format!("resp_{}", Uuid::new_v4().simple()),
            current_item_id: None,
            state: TranscoderState::Init,
        }
    }

    pub fn process(&mut self, chunk: LegacyChunk) -> Vec<OrsEvent> {
        let mut events = Vec::new();

        // We assume single-choice streaming for now (standard for chat)
        if let Some(choice) = chunk.choices.first() {
            // 1. Handle Initialization (First chunk logic)
            if let TranscoderState::Init = self.state {
                // Emit response.created
                events.push(OrsEvent::Created {
                    id: self.response_id.clone(),
                });

                // Check if we should start a default Message item.
                // If the first chunk has tool_calls and NO content, we skip creating the message
                // because the tool_calls loop will create the FunctionCall item(s).
                let has_tool_calls = choice.delta.tool_calls.as_ref().map(|tc| !tc.is_empty()).unwrap_or(false);
                let has_content = choice.delta.content.as_ref().map(|s| !s.is_empty()).unwrap_or(false);

                if !has_tool_calls || has_content {
                    let item_id = format!("msg_{}", Uuid::new_v4().simple());
                    self.current_item_id = Some(item_id.clone());

                    events.push(OrsEvent::ItemAdded {
                        item_id,
                        item: serde_json::json!({ "type": "message", "role": "assistant", "content": [] }),
                    });
                }

                self.state = TranscoderState::Streaming;
            }

            let item_id = self.current_item_id.as_ref().cloned().unwrap_or_default(); // Fallback if no item started (should be handled by tool loop if skipped)
            
            // 2. Handle Content Deltas
            if let Some(content) = &choice.delta.content {
                if !content.is_empty() {
                    // If we skipped message creation but got content now (shouldn't happen if logic above is correct for 1st chunk),
                    // logic works because has_content would be true.
                    // But if item_id is empty (from unwrap_or_default)?
                     if !item_id.is_empty() {
                        events.push(OrsEvent::TextDelta {
                            item_id: item_id.clone(),
                            delta: content.clone(),
                        });
                     }
                }
            }

            if let Some(tool_calls) = &choice.delta.tool_calls {
                for tool_call in tool_calls {
                    // Check if this tool call starts a new item (has 'id')
                    // Note: Legacy chunks can contain multiple tool calls or updates to existing ones.
                    // We assume sequential processing for now. 
                    // A new 'id' implies a new function call item.
                    
                    // Extract relevant fields
                    let id = tool_call.get("id").and_then(|v| v.as_str());
                    let function = tool_call.get("function");
                    let name = function.and_then(|f| f.get("name").and_then(|n| n.as_str()));
                    let args_delta = function.and_then(|f| f.get("arguments").and_then(|a| a.as_str()));
                    
                    if let Some(call_id) = id {
                        // New Function Call Item!
                        let new_item_id = format!("fc_{}", Uuid::new_v4().simple());
                        self.current_item_id = Some(new_item_id.clone());
                        
                        let call_name = name.unwrap_or("unknown"); // Name usually comes with ID
                        
                        events.push(OrsEvent::ItemAdded {
                            item_id: new_item_id.clone(),
                            item: serde_json::json!({
                                "type": "function_call",
                                "call_id": call_id,
                                "name": call_name,
                                "arguments": "" // Initial state
                            }),
                        });
                    }
                    
                    // If we have an active item and args delta, emit it
                    // We assume self.current_item_id is pointing to the function call now
                    if let Some(delta) = args_delta {
                        if !delta.is_empty() {
                            if let Some(current_id) = &self.current_item_id {
                                 events.push(OrsEvent::FunctionCallArgumentsDelta {
                                     item_id: current_id.clone(),
                                     delta: delta.to_string(),
                                 });
                            }
                        }
                    }
                }
            }
            
            // 3. Handle Completion
            if let Some(finish_reason) = &choice.finish_reason {
                let status = match finish_reason.as_str() {
                    "stop" => "completed",
                    "length" => "incomplete",
                    "content_filter" => "incomplete", // or failed? Spec says incomplete is exhaustion. Content filter is effectively incomplete/refused.
                    _ => "completed",
                };
                
                events.push(OrsEvent::ItemDone {
                    item_id,
                    status: status.to_string(),
                });
            }
        }

        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{LegacyChoice, LegacyDelta};
    use serde_json::Value;

    fn make_chunk(content: Option<&str>, finish_reason: Option<&str>) -> LegacyChunk {
        LegacyChunk {
            choices: vec![LegacyChoice {
                delta: LegacyDelta {
                    content: content.map(|s| s.to_string()),
                    tool_calls: None,
                    extra: Value::Null,
                },
                finish_reason: finish_reason.map(|s| s.to_string()),
            }],
        }
    }

    #[test]
    fn test_transcoder_lifecycle() {
        let mut transcoder = Transcoder::new();

        // 1. First chunk: Role "assistant", empty content
        let chunk1 = make_chunk(Some(""), None); 
        
        let events = transcoder.process(chunk1);
        
        // Should have Created AND ItemAdded
        assert_eq!(events.len(), 2);
        match &events[0] {
            OrsEvent::Created { .. } => {},
            _ => panic!("First event should be Created"),
        }
        match &events[1] {
            OrsEvent::ItemAdded { item, .. } => {
                // item is Value
                assert_eq!(item["type"], "message");
            },
            _ => panic!("Second event should be ItemAdded"),
        }

        // 2. Content chunk
        let chunk2 = make_chunk(Some("Hello"), None);
        let events = transcoder.process(chunk2);
        assert_eq!(events.len(), 1);
        match &events[0] {
            OrsEvent::TextDelta { delta, .. } => assert_eq!(delta, "Hello"),
            _ => panic!("Should be TextDelta"),
        }

        // 3. Finish chunk
        let chunk3 = make_chunk(None, Some("stop"));
        let events = transcoder.process(chunk3);
        assert_eq!(events.len(), 1);
        match &events[0] {
            OrsEvent::ItemDone { status, .. } => assert_eq!(status, "completed"),
            _ => panic!("Should be ItemDone"),
        }
    }

    #[test]
    fn test_transcoder_tool_calls() {
        let mut transcoder = Transcoder::new();
        // 1. Start Tool Call
        let chunk1_json = serde_json::json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_123",
                        "type": "function",
                        "function": { "name": "get_weather", "arguments": "" }
                    }]
                }
            }]
        });
        let chunk1: LegacyChunk = serde_json::from_value(chunk1_json).unwrap();
        let events1 = transcoder.process(chunk1);
        
        // Initial chunk might just be Created for the very first one?
        // Wait, if this is the first chunk ever, it emits Created + ItemAdded.
        // If we reuse transcoder? It's new.
        // So we expect Created, ItemAdded.
        
        assert_eq!(events1.len(), 2);
        match &events1[0] {
             OrsEvent::Created { .. } => {},
             _ => panic!("Expected Created"),
        }
        match &events1[1] {
            OrsEvent::ItemAdded { item_id, item } => {
                assert!(item_id.starts_with("fc_"));
                assert_eq!(item["type"], "function_call");
                assert_eq!(item["call_id"], "call_123");
                assert_eq!(item["name"], "get_weather");
            },
            _ => panic!("Expected ItemAdded"),
        }
        
        // 2. Stream Arguments
        let chunk2_json = serde_json::json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "function": { "arguments": "{\"loc\"" }
                    }]
                }
            }]
        });
        let chunk2: LegacyChunk = serde_json::from_value(chunk2_json).unwrap();
        let events2 = transcoder.process(chunk2);
        assert_eq!(events2.len(), 1);
        if let OrsEvent::FunctionCallArgumentsDelta { delta, .. } = &events2[0] {
             assert_eq!(delta, "{\"loc\"");
        } else {
             panic!("Expected Args Delta");
        }

        // 3. Finish
        let chunk3_json = serde_json::json!({
            "choices": [{
                "delta": {},
                "finish_reason": "tool_calls"
            }]
        });
        let chunk3: LegacyChunk = serde_json::from_value(chunk3_json).unwrap();
        let events3 = transcoder.process(chunk3);
        assert_eq!(events3.len(), 1);
        if let OrsEvent::ItemDone { status, .. } = &events3[0] {
            assert_eq!(status, "completed");
        } else {
            panic!("Expected ItemDone");
        }
    }
}
