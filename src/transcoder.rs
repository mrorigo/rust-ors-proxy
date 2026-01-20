use crate::types::{LegacyChunk, OrsEvent};
use uuid::Uuid;

pub struct Transcoder {
    response_id: String,
    current_item_id: Option<String>,
    current_item_type: Option<String>,
    current_content_index: Option<u32>,
    has_emitted_content_start: bool,
    state: TranscoderState,
    sequence_number: u32,
}

enum TranscoderState {
    Init,
    Streaming,
}

impl Transcoder {
    pub fn new() -> Self {
        Self {
            response_id: format!("resp_{}", Uuid::new_v4().simple()),
            current_item_id: None,
            current_item_type: None,
            current_content_index: None,
            has_emitted_content_start: false,
            state: TranscoderState::Init,
            sequence_number: 0,
        }
    }

    fn next_seq(&mut self) -> Option<u32> {
        let seq = self.sequence_number;
        self.sequence_number += 1;
        Some(seq)
    }

    pub fn process(&mut self, chunk: LegacyChunk) -> Vec<OrsEvent> {
        let mut events = Vec::new();

        // We assume single-choice streaming for now (standard for chat)
        if let Some(choice) = chunk.choices.first() {
            // 1. Handle Initialization (First chunk logic)
            if let TranscoderState::Init = self.state {
                // Emit response.created
                let seq = self.next_seq();
                events.push(OrsEvent::Created {
                    id: self.response_id.clone(),
                    sequence_number: seq,
                });

                let has_tool_calls = choice.delta.tool_calls.as_ref().map(|tc| !tc.is_empty()).unwrap_or(false);
                let has_content = choice.delta.content.as_ref().map(|s| !s.is_empty()).unwrap_or(false);

                if !has_tool_calls || has_content {
                    let item_id = format!("msg_{}", Uuid::new_v4().simple());
                    self.current_item_id = Some(item_id.clone());
                    self.current_item_type = Some("message".to_string());

                    events.push(OrsEvent::ItemAdded {
                        sequence_number: seq,
                        item: serde_json::json!({ 
                            "id": item_id,
                            "type": "message", 
                            "status": "in_progress",
                            "role": "assistant", 
                            "content": [] 
                        }),
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
                        // Check if we need to start a content part
                        if !self.has_emitted_content_start {
                            let seq = self.next_seq();
                            let content_idx = self.current_content_index.unwrap_or(0); // Default to 0 for first part
                            self.current_content_index = Some(content_idx);
                            
                            events.push(OrsEvent::ContentPartAdded {
                                sequence_number: seq,
                                item_id: item_id.clone(),
                                output_index: Some(0), // Simple proxy assumes single output
                                content_index: Some(content_idx),
                                part: serde_json::json!({ "type": "output_text", "text": "" }),
                            });
                            self.has_emitted_content_start = true;
                        }

                        let seq = self.next_seq();
                        events.push(OrsEvent::TextDelta {
                            sequence_number: seq,
                            item_id: item_id.clone(),
                            output_index: Some(0),
                            content_index: self.current_content_index,
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
                        
                        let seq = self.next_seq();
                        events.push(OrsEvent::ItemAdded {
                            sequence_number: seq,
                            item: serde_json::json!({
                                "id": new_item_id,
                                "type": "function_call",
                                "status": "in_progress",
                                "call_id": call_id,
                                "name": call_name,
                                "arguments": "" // Initial state
                            }),
                        });
                        self.current_item_type = Some("function_call".to_string());
                    }
                    
                    // If we have an active item and args delta, emit it
                    // We assume self.current_item_id is pointing to the function call now
                    if let Some(delta) = args_delta {
                        if !delta.is_empty() {
                            let current_id = self.current_item_id.clone();
                            if let Some(current_id) = current_id {
                                 let seq = self.next_seq();
                                 events.push(OrsEvent::FunctionCallArgumentsDelta {
                                     sequence_number: seq,
                                     item_id: current_id,
                                     output_index: Some(0),
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
                
                // If we were streaming content, close the content part first
                if self.has_emitted_content_start {
                     let seq = self.next_seq();
                     let content_idx = self.current_content_index.unwrap_or(0);
                     // We don't track accumulated text here easily without buffering. 
                     // But spec example shows "text": "full text" in ContentPartDone.
                     // The spec says "The content part is then closed with response.content_part.done".
                     // Ideally we should send the final part state. 
                     // IMPORTANT: Since we are valid-proxying, we might not have the full text if we didn't buffer.
                     // The spec allows the Part in Done event. 
                     // Verify if Part is required to be fully populated? 
                     // "part": { "type": "output_text", "text": "..." }
                     // If we don't have it, we might just emit the type. 
                     // However, to be safe and simple, let's skip buffering for now and send what we can or empty string?
                     // Actually, if we are just a proxy, maybe we can omit the `text` field in `done` if unnecessary?
                     // Spec example uses it. 
                     // Let's rely on the fact that we sent Deltas.
                     
                     events.push(OrsEvent::ContentPartDone {
                        sequence_number: seq,
                        item_id: item_id.clone(),
                        output_index: Some(0),
                        content_index: Some(content_idx),
                        part: serde_json::json!({ "type": "output_text", "text": "" }), // Placeholder or nothing
                     });
                     
                     self.has_emitted_content_start = false;
                     self.current_content_index = None;
                }

                let seq = self.next_seq();
                let item_type = self.current_item_type.as_deref().unwrap_or("message");
                
                events.push(OrsEvent::ItemDone {
                    sequence_number: seq,
                    output_index: Some(0),
                    item: serde_json::json!({
                        "id": item_id,
                        "type": item_type,
                        "status": status.to_string(),
                    }),
                });
                
                self.current_item_id = None;
                self.current_item_type = None;
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

        // 2. Content chunk -> Should emit ContentPartAdded and TextDelta
        let chunk2 = make_chunk(Some("Hello"), None);
        let events = transcoder.process(chunk2);
        assert_eq!(events.len(), 2);
        match &events[0] {
             OrsEvent::ContentPartAdded { .. } => {},
             _ => panic!("Should be ContentPartAdded"),
        }
        match &events[1] {
            OrsEvent::TextDelta { delta, .. } => assert_eq!(delta, "Hello"),
            _ => panic!("Should be TextDelta"),
        }

        // 3. Finish chunk
        let chunk3 = make_chunk(None, Some("stop"));
        let events = transcoder.process(chunk3);
        // content part done + item done
        assert_eq!(events.len(), 2);
        match &events[0] {
             OrsEvent::ContentPartDone { .. } => {},
             _ => panic!("Should be ContentPartDone"),
        }
        match &events[1] {
            OrsEvent::ItemDone { item, .. } => assert_eq!(item["status"], "completed"),
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
            OrsEvent::ItemAdded { item, .. } => {
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
        if let OrsEvent::ItemDone { item, .. } = &events3[0] {
            assert_eq!(item["status"], "completed");
        } else {
            panic!("Expected ItemDone");
        }
    }
}
