use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppData {
    pub name: String,
    pub app_id: String,
    pub category: String,
    pub icon_base64: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryData {
    pub categories: Vec<String>,
    pub apps: Vec<AppData>,
}

impl Default for CategoryData {
    fn default() -> Self {
        Self {
            categories: vec![
                "Uncategorized".into(),
                "Browser".into(),
                "Entertainment".into(),
                "Tools".into(),
                "System Applications".into(),
            ],
            apps: vec![],
        }
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ActiveSession {
    pub session_id: i64,
    pub app_id: String,
    pub app_name: String,
    pub started_at: Instant,
}

#[derive(Debug, Clone)]
pub struct BgSession {
    pub exe_name: String,
    pub started_at: Instant,
    pub opened_at_str: String,
}

pub struct AppState {
    pub data: Arc<Mutex<CategoryData>>,
    pub session: Arc<Mutex<Option<ActiveSession>>>,
    pub bg_session: Arc<Mutex<Option<BgSession>>>,
}

impl AppState {
    pub fn new(data: CategoryData) -> Self {
        Self {
            data: Arc::new(Mutex::new(data)),
            session: Arc::new(Mutex::new(None)),
            bg_session: Arc::new(Mutex::new(None)),
        }
    }
}
