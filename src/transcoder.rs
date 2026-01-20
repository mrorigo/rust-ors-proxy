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

                // Start the first item. 
                // TODO: Logic to detect if it's a Tool Call from the first chunk.
                // For now, default to Message.
                let item_id = format!("msg_{}", Uuid::new_v4().simple());
                self.current_item_id = Some(item_id.clone());

                events.push(OrsEvent::ItemAdded {
                    item_id,
                    item_type: "message".to_string(), // Explicitly lower-case as per spec examples
                });

                self.state = TranscoderState::Streaming;
            }

            let item_id = self.current_item_id.as_ref().unwrap().clone();

            // 2. Handle Content Deltas
            if let Some(content) = &choice.delta.content {
                if !content.is_empty() {
                    events.push(OrsEvent::TextDelta {
                        item_id: item_id.clone(),
                        delta: content.clone(),
                    });
                }
            }
            
            // TODO: Handle Tool Call Deltas

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

        // 1. First chunk: Role "assistant", empty content (common in OpenAI)
        let chunk1 = make_chunk(Some(""), None); 
        // Note: Sometimes first chunk has no content field at all, but let's assume empty string or handled by optional
        
        // Let's ensure our make_chunk helper produces what we expect. 
        // Real OpenAI first chunk: {"choices":[{"delta":{"role":"assistant"},"index":0,"finish_reason":null}]}
        // Our LegacyDelta struct has 'content' as Option<String>.
        
        let events = transcoder.process(chunk1);
        
        // Should have Created AND ItemAdded
        assert_eq!(events.len(), 2);
        match &events[0] {
            OrsEvent::Created { .. } => {},
            _ => panic!("First event should be Created"),
        }
        match &events[1] {
            OrsEvent::ItemAdded { item_type, .. } => assert_eq!(item_type, "message"),
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
}
