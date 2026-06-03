use crate::crypto::{decrypt_bytes, encrypt_bytes, get_config_path};
use crate::state::{AppData, CategoryData};
use std::fs;

pub fn load_config() -> CategoryData {
    if let Ok(raw) = fs::read(get_config_path()) {
        // Try decrypt first, then plain JSON fallback
        let bytes = decrypt_bytes(&raw).unwrap_or(raw);
        if let Ok(data) = serde_json::from_slice(&bytes) {
            return data;
        }
    }
    CategoryData::default()
}

pub fn save_config(data: &CategoryData) {
    let lean = CategoryData {
        categories: data.categories.clone(),
        apps: data
            .apps
            .iter()
            .map(|a| AppData {
                name: a.name.clone(),
                app_id: a.app_id.clone(),
                category: a.category.clone(),
                icon_base64: None, // never persist icons
            })
            .collect(),
    };
    if let Ok(json) = serde_json::to_string_pretty(&lean) {
        let _ = fs::write(get_config_path(), encrypt_bytes(json.as_bytes()));
    }
}
