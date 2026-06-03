use aes_gcm::{
    Aes256Gcm, Key, Nonce,
    aead::{Aead, AeadCore, KeyInit, OsRng},
};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

macro_rules! no_window {
    ($cmd:expr) => {{
        #[cfg(windows)]
        {
            $cmd.creation_flags(CREATE_NO_WINDOW)
        }
        #[cfg(not(windows))]
        {
            $cmd
        }
    }};
}

pub fn get_app_dir() -> PathBuf {
    let local = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    let mut p = PathBuf::from(local);
    p.push("AppDrawer");
    fs::create_dir_all(&p).ok();
    p
}

pub fn get_config_path() -> PathBuf {
    get_app_dir().join("config.json")
}
pub fn get_db_path() -> PathBuf {
    get_app_dir().join("analytics.db")
}
pub fn get_icons_dir() -> PathBuf {
    let p = get_app_dir().join("icons");
    fs::create_dir_all(&p).ok();
    p
}

fn get_machine_id() -> String {
    let out = no_window!(Command::new("powershell").args([
        "-NoProfile",
        "-Command",
        "(Get-ItemProperty -Path 'HKLM:\\SOFTWARE\\Microsoft\\Cryptography').MachineGuid",
    ]))
    .output()
    .unwrap_or_else(|_| std::process::Output {
        status: std::process::ExitStatus::default(),
        stdout: b"fallback-id".to_vec(),
        stderr: vec![],
    });
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

pub fn derive_key() -> [u8; 32] {
    let id = get_machine_id();
    let mut h = Sha256::new();
    h.update(b"AppDrawer-v1-");
    h.update(id.as_bytes());
    h.update(b"-analytics-key");
    h.finalize().into()
}

pub fn encrypt_bytes(data: &[u8]) -> Vec<u8> {
    let key = derive_key();
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ct = cipher.encrypt(&nonce, data).unwrap_or_default();
    let mut out = nonce.to_vec();
    out.extend_from_slice(&ct);
    out
}

pub fn decrypt_bytes(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 12 {
        return None;
    }
    let key = derive_key();
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let nonce = Nonce::from_slice(&data[..12]);
    cipher.decrypt(nonce, &data[12..]).ok()
}
