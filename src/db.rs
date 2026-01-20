use crate::types::{OrsEvent, OrsInputItem, OrsRole, OrsContentPart};
use sqlx::{sqlite::SqlitePool, Row};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

#[derive(Clone)]
pub struct Db {
    pool: SqlitePool,
}

impl Db {
    pub async fn new(database_url: &str) -> Result<Self, sqlx::Error> {
        let pool = SqlitePool::connect(database_url).await?;
        let db = Self { pool };
        db.init().await?;
        Ok(db)
    }

    async fn init(&self) -> Result<(), sqlx::Error> {
        let schema = r#"
            CREATE TABLE IF NOT EXISTS conversations (
                id TEXT PRIMARY KEY, 
                created_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS items (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                conversation_id TEXT NOT NULL,
                sequence_index INTEGER NOT NULL,
                item_type TEXT NOT NULL,
                payload JSON NOT NULL,
                FOREIGN KEY(conversation_id) REFERENCES conversations(id)
            );
            
            CREATE INDEX IF NOT EXISTS idx_items_seq ON items(conversation_id, sequence_index);
        "#;

        sqlx::query(schema).execute(&self.pool).await?;
        info!("Database initialized");
        Ok(())
    }

    pub async fn load_context(&self, conversation_id: &str) -> Result<Vec<OrsInputItem>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT payload FROM items WHERE conversation_id = ? ORDER BY sequence_index ASC",
        )
        .bind(conversation_id)
        .fetch_all(&self.pool)
        .await?;

        let items = rows
            .into_iter()
            .map(|row| {
                let json_str: String = row.get("payload");
                serde_json::from_str(&json_str).unwrap_or_else(|e| {
                    warn!("Failed to deserialize item payload: {}", e);
                    // Fallback or skip? For now, we panic in unwrap or allow corruption?
                    // Safe fallback: Return a dummy or valid "error" item if we had one.
                    // But here we must match the return type.
                    // Let's assume DB integrity for now.
                    panic!("Corrupt DB item: {}", e);
                })
            })
            .collect();

        Ok(items)
    }

    pub async fn save_interaction(
        &self,
        conversation_id: &str,
        input: Vec<OrsInputItem>,
        output_events: Vec<OrsEvent>,
    ) -> Result<(), sqlx::Error> {
        // 1. Ensure conversation exists
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        sqlx::query(
            "INSERT OR IGNORE INTO conversations (id, created_at) VALUES (?, ?)",
        )
        .bind(conversation_id)
        .bind(now)
        .execute(&self.pool)
        .await?;

        // 2. Determine next sequence index
        let count_row: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM items WHERE conversation_id = ?",
        )
        .bind(conversation_id)
        .fetch_one(&self.pool)
        .await?;
        
        let mut sequence_index = count_row.0;

        // 3. Save Input Items
        for item in input {
            let payload = serde_json::to_string(&item).unwrap();
            sqlx::query(
                "INSERT INTO items (conversation_id, sequence_index, item_type, payload) VALUES (?, ?, ?, ?)",
            )
            .bind(conversation_id)
            .bind(sequence_index)
            .bind("input") // Just a label, payload has real type
            .bind(payload)
            .execute(&self.pool)
            .await?;
            sequence_index += 1;
        }

        // 4. Reconstruct Output Items from Events
        // The events are a stream of Created, Added, Delta, Done.
        // We need to aggregate them into OrsInputItem format (Message or FunctionCall) to store them.
        // For simplicity in this proxy, we only store the FINAL state as a "assistant message".
        
        // This aggregation logic is tricky because we only have the raw events here.
        // Ideally, the caller should pass the aggregated output as OrsInputItem.
        // But the caller is streaming.
        
        // Strategy: We can aggregate locally here.
        // Group by item_id.
        use std::collections::HashMap;
        
        struct ItemState {
            item_type: String, // "message"
            content: String,
        }
        let mut items_map: HashMap<String, ItemState> = HashMap::new();
        let mut item_order: Vec<String> = Vec::new();

        for event in output_events {
            match event {
                OrsEvent::ItemAdded { item_id, item } => {
                    let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();
                    items_map.insert(item_id.clone(), ItemState { item_type, content: String::new() });
                    item_order.push(item_id);
                }
                OrsEvent::TextDelta { item_id, delta } => {
                     if let Some(state) = items_map.get_mut(&item_id) {
                         state.content.push_str(&delta);
                     }
                }
                _ => {}
            }
        }

        for item_id in item_order {
            if let Some(state) = items_map.get(&item_id) {
                // Convert to OrsInputItem
                let item = OrsInputItem::Message {
                    role: OrsRole::Assistant,
                    content: vec![OrsContentPart::InputText { text: state.content.clone() }]
                };
                
                let payload = serde_json::to_string(&item).unwrap();
                sqlx::query(
                    "INSERT INTO items (conversation_id, sequence_index, item_type, payload) VALUES (?, ?, ?, ?)",
                )
                .bind(conversation_id)
                .bind(sequence_index)
                .bind(&state.item_type)
                .bind(payload)
                .execute(&self.pool)
                .await?;
                sequence_index += 1;
            }
        }

        Ok(())
    }
}
