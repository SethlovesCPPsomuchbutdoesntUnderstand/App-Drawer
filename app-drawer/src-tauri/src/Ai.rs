use serde::{Deserialize, Serialize};
use std::fs;
use tauri::State;
use crate::crypto::{encrypt_bytes, decrypt_bytes, get_config_path};
use crate::state::AppState;

#[derive(Debug, Serialize, Deserialize)]
pub struct AiAction {
    pub action:        String,
    pub app_name:      Option<String>,
    pub category:      Option<String>,
    pub search_query:  Option<String>,
    pub search_engine: Option<String>,
    pub response:      String,
}

/// AI voice command is currently disabled — returns a friendly message.
/// Re-enable by uncommenting the reqwest/Claude API code when ready.
#[tauri::command]
pub async fn process_voice_command(
    _command: String,
    _api_key: String,
    _state: State<'_, AppState>,
) -> Result<AiAction, String> {
    Ok(AiAction {
        action:        "unknown".to_string(),
        app_name:      None,
        category:      None,
        search_query:  None,
        search_engine: None,
        response:      "AI voice commands are currently disabled. Local AI coming soon!".to_string(),
    })
}

#[tauri::command]
pub fn save_api_key(key: String) -> Result<(), String> {
    let mut p = get_config_path();
    p.pop(); p.push(".apikey");
    fs::write(p, encrypt_bytes(key.as_bytes())).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn load_api_key() -> Result<String, String> {
    let mut p = get_config_path();
    p.pop(); p.push(".apikey");
    if !p.exists() { return Ok(String::new()); }
    let enc = fs::read(&p).map_err(|e| e.to_string())?;
    let dec = decrypt_bytes(&enc).ok_or("Decryption failed")?;
    String::from_utf8(dec).map_err(|e| e.to_string())
}