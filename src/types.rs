use serde::{Deserialize, Serialize};
use serde_json::Value;

// ================================================================================================
// ORS INBOUND (STRICT)
// ================================================================================================

#[derive(Deserialize, Debug)]
pub struct OrsRequest {
    pub model: String,
    pub input: Vec<OrsInputItem>,
    #[serde(default)]
    #[allow(dead_code)]
    pub store: bool,
    pub previous_response_id: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub stream: bool,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OrsInputItem {
    Message {
        role: OrsRole,
        content: Vec<OrsContentPart>,
    },
    FunctionCall {
        id: String,
        call_id: String,
        name: String,
        arguments: Value,
    },
    FunctionCallOutput {
        id: String,
        call_id: String,
        output: String, // Value?
    },
}

#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OrsRole {
    User,
    Assistant,
    Developer,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OrsContentPart {
    InputText { text: String },
    InputImage { image_url: Value },
}

// ================================================================================================
// LEGACY UPSTREAM (LOOSE)
// ================================================================================================

#[derive(Serialize, Debug)]
pub struct LegacyChatRequest {
    pub model: String,
    pub messages: Vec<LegacyMessage>,
    pub stream: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LegacyMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>, // Can be String or Vec<ContentPart>
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<Value>>, // Upstream tool format (OpenAI compatible)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct LegacyChunk {
    pub choices: Vec<LegacyChoice>,
}

#[derive(Deserialize, Debug)]
pub struct LegacyChoice {
    pub delta: LegacyDelta,
    pub finish_reason: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct LegacyDelta {
    pub content: Option<String>,
    pub tool_calls: Option<Vec<Value>>,
    #[serde(flatten)]
    #[allow(dead_code)]
    pub extra: Value,
}

// ================================================================================================
// ORS OUTBOUND EVENTS
// ================================================================================================

#[derive(Serialize, Debug, Clone)]
#[serde(tag = "event", content = "data")]
pub enum OrsEvent {
    #[serde(rename = "response.created")]
    Created { id: String },

    #[serde(rename = "response.output_item.added")]
    ItemAdded {
        item_id: String,
        item: Value,
    },

    #[serde(rename = "response.output_text.delta")]
    TextDelta {
        item_id: String,
        delta: String,
    },

    #[serde(rename = "response.function_call_arguments.delta")]
    FunctionCallArgumentsDelta {
        item_id: String,
        delta: String,
    },

    #[serde(rename = "response.output_item.done")]
    ItemDone {
        item_id: String,
        status: String,
    },
}
