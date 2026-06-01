// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use aes_gcm::{
    Aes256Gcm, Key, Nonce,
    aead::{Aead, AeadCore, KeyInit, OsRng},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

use rusqlite::{Connection, params};
#[cfg(windows)]
use windows::Win32::Foundation::{CloseHandle, HWND};
#[cfg(windows)]
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AppData {
    name: String,
    app_id: String,
    category: String,
    icon_base64: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CategoryData {
    categories: Vec<String>,
    apps: Vec<AppData>,
}

#[derive(Debug, Clone)]
struct ActiveSession {
    session_id: i64,
    app_id: String,
    app_name: String,
    started_at: std::time::Instant,
}

struct AppState {
    data: Arc<Mutex<CategoryData>>,
    session: Arc<Mutex<Option<ActiveSession>>>,
    bg_session: Arc<Mutex<Option<BgSession>>>,
}

#[derive(Debug, Clone)]
struct BgSession {
    exe_name: String,
    started_at: std::time::Instant,
    opened_at_str: String,
}

fn get_config_path() -> PathBuf {
    let local_data = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
    let mut path = PathBuf::from(local_data);
    path.push("AppDrawer");
    fs::create_dir_all(&path).ok();
    path.push("config.json");
    path
}

fn get_db_path() -> PathBuf {
    let local_data = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
    let mut path = PathBuf::from(local_data);
    path.push("AppDrawer");
    fs::create_dir_all(&path).ok();
    path.push("analytics.db");
    path
}

/// Opens the analytics DB — we use an in-memory DB seeded from the encrypted file,
/// and flush back to disk (encrypted) after each write operation.
fn open_db() -> rusqlite::Result<Connection> {
    let conn = Connection::open(get_db_path())?;
    // Apply performance pragmas
    conn.execute_batch(
        "
        PRAGMA journal_mode=WAL;
        PRAGMA synchronous=NORMAL;
        PRAGMA cache_size=-8000;
    ",
    )?;
    Ok(conn)
}

fn setup_db() {
    // If DB is corrupted or empty, remove and start fresh
    let db_path = get_db_path();
    if db_path.exists() {
        if let Ok(conn) = Connection::open(&db_path) {
            let ok: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='app_sessions'",
                    [],
                    |row| row.get::<_, i64>(0).map(|n| n > 0),
                )
                .unwrap_or(false);
            if !ok {
                let _ = fs::remove_file(&db_path);
            }
        } else {
            let _ = fs::remove_file(&db_path);
        }
    }
    let conn = open_db().expect("Failed to open analytics DB");
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS app_sessions (
            id            INTEGER PRIMARY KEY AUTOINCREMENT,
            app_id        TEXT    NOT NULL,
            app_name      TEXT    NOT NULL,
            category      TEXT    NOT NULL DEFAULT 'Uncategorized',
            opened_at     TEXT    NOT NULL,
            closed_at     TEXT,
            duration_secs INTEGER
        );
        CREATE TABLE IF NOT EXISTS daily_summary (
            date       TEXT NOT NULL,
            app_id     TEXT NOT NULL,
            app_name   TEXT NOT NULL,
            total_secs INTEGER NOT NULL DEFAULT 0,
            open_count INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (date, app_id)
        );
        CREATE INDEX IF NOT EXISTS idx_sessions_opened ON app_sessions(opened_at);
        CREATE INDEX IF NOT EXISTS idx_sessions_app    ON app_sessions(app_id);
        CREATE INDEX IF NOT EXISTS idx_sessions_dur    ON app_sessions(duration_secs);
        CREATE INDEX IF NOT EXISTS idx_summary_date    ON daily_summary(date);
        CREATE INDEX IF NOT EXISTS idx_summary_app     ON daily_summary(app_id);
    ",
    )
    .expect("Failed to create analytics tables");
}

// ── Encryption ───────────────────────────────────────────────────────────────

/// Derives a 32-byte AES key from the machine's unique identifier
/// so the key is tied to this specific machine — no hardcoded secrets
fn derive_machine_key() -> [u8; 32] {
    // Use machine SID + app identifier as key material
    let machine_id = get_machine_id();
    let mut hasher = Sha256::new();
    hasher.update(b"AppDrawer-v1-");
    hasher.update(machine_id.as_bytes());
    hasher.update(b"-analytics-key");
    hasher.finalize().into()
}

fn get_machine_id() -> String {
    // Use Windows MachineGuid as unique device identifier
    let output = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "(Get-ItemProperty -Path 'HKLM:\\SOFTWARE\\Microsoft\\Cryptography').MachineGuid",
        ])
        .output()
        .unwrap_or_else(|_| std::process::Output {
            status: std::process::ExitStatus::default(),
            stdout: b"fallback-id".to_vec(),
            stderr: vec![],
        });
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Encrypts bytes with AES-256-GCM — returns nonce(12) + ciphertext
pub fn encrypt_bytes(data: &[u8]) -> Vec<u8> {
    let raw_key = derive_machine_key();
    let key = Key::<Aes256Gcm>::from_slice(&raw_key);
    let cipher = Aes256Gcm::new(key);
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher.encrypt(&nonce, data).unwrap_or_default();
    // Prepend nonce to ciphertext
    let mut out = nonce.to_vec();
    out.extend_from_slice(&ciphertext);
    out
}

/// Decrypts bytes encrypted with encrypt_bytes
pub fn decrypt_bytes(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 12 {
        return None;
    }
    let raw_key = derive_machine_key();
    let key = Key::<Aes256Gcm>::from_slice(&raw_key);
    let cipher = Aes256Gcm::new(key);
    let nonce = Nonce::from_slice(&data[..12]);
    cipher.decrypt(nonce, &data[12..]).ok()
}

fn get_icons_dir() -> PathBuf {
    let local_data = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
    let mut path = PathBuf::from(local_data);
    path.push("AppDrawer");
    path.push("icons");
    fs::create_dir_all(&path).ok();
    path
}

fn load_config() -> CategoryData {
    // Try reading as encrypted first, fall back to plaintext for migration
    if let Ok(encrypted) = fs::read(get_config_path()) {
        if let Some(decrypted) = decrypt_bytes(&encrypted) {
            if let Ok(data) = serde_json::from_slice(&decrypted) {
                return data;
            }
        }
        // Fallback: try as plain JSON (first run / migration)
        if let Ok(text) = String::from_utf8(encrypted) {
            if let Ok(data) = serde_json::from_str(&text) {
                return data;
            }
        }
    }
    CategoryData {
        categories: vec![
            "Uncategorized".to_string(),
            "Browser".to_string(),
            "Entertainment".to_string(),
            "Tools".to_string(),
            "System Applications".to_string(),
        ],
        apps: vec![],
    }
}

fn save_config(data: &CategoryData) {
    // Strip icon_base64 before saving — icons are cached separately in the icons/ folder
    let lean = CategoryData {
        categories: data.categories.clone(),
        apps: data
            .apps
            .iter()
            .map(|a| AppData {
                name: a.name.clone(),
                app_id: a.app_id.clone(),
                category: a.category.clone(),
                icon_base64: None,
            })
            .collect(),
    };
    if let Ok(json) = serde_json::to_string_pretty(&lean) {
        // Encrypt before writing to disk
        let encrypted = encrypt_bytes(json.as_bytes());
        let _ = fs::write(get_config_path(), encrypted);
    }
}

/// Uses SHGetFileInfo via PowerShell + WinForms to extract icon for any app.
/// Saves to a cache file and returns base64.
/// Extracts icons for ALL apps in one single PowerShell call
/// Much faster than one process per app
fn batch_extract_icons(app_ids: &[(String, std::path::PathBuf)]) {
    if app_ids.is_empty() {
        return;
    }

    // Build a JSON array of {id, path} for PowerShell to process
    let jobs: String = app_ids
        .iter()
        .map(|(id, path)| {
            let safe_id = id.replace('\'', "''");
            let safe_path = path.to_string_lossy().replace('\\', "\\\\");
            format!("@{{Id='{}';Out='{}'}}", safe_id, safe_path)
        })
        .collect::<Vec<_>>()
        .join(",");

    let ps = format!(
        r#"
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Drawing;
using System.Runtime.InteropServices;
public class IE {{
    [DllImport("shell32.dll", CharSet=CharSet.Unicode)]
    public static extern uint SHGetFileInfo(string p, uint fa, ref SHFI fi, uint cb, uint fl);
    [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)]
    public struct SHFI {{
        public IntPtr hIcon; public int iIcon; public uint dwAttr;
        [MarshalAs(UnmanagedType.ByValTStr,SizeConst=260)] public string szDisplayName;
        [MarshalAs(UnmanagedType.ByValTStr,SizeConst=80)]  public string szTypeName;
    }}
    [DllImport("user32.dll")] public static extern bool DestroyIcon(IntPtr h);
}}
"@

function Save-Icon($icon, $outPath) {{
    $bmp     = $icon.ToBitmap()
    $resized = New-Object System.Drawing.Bitmap(64,64)
    $g       = [System.Drawing.Graphics]::FromImage($resized)
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.DrawImage($bmp, 0, 0, 64, 64)
    $g.Dispose()
    $resized.Save($outPath, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose(); $resized.Dispose()
}}

$shell     = New-Object -ComObject Shell.Application
$appsFolder = $shell.Namespace('shell:AppsFolder')
$allItems  = $appsFolder.Items()
$jobs      = @({jobs})

foreach ($job in $jobs) {{
    $appId   = $job.Id
    $outPath = $job.Out
    if (Test-Path $outPath) {{ continue }}
    $success = $false

    # Method 1: SHGetFileInfo (works for UWP + Win32)
    try {{
        $fi = New-Object IE+SHFI
        $r  = [IE]::SHGetFileInfo("shell:AppsFolder\$appId", 0, [ref]$fi,
              [System.Runtime.InteropServices.Marshal]::SizeOf($fi), 0x100)
        if ($fi.hIcon -ne [IntPtr]::Zero) {{
            $icon = [System.Drawing.Icon]::FromHandle($fi.hIcon)
            Save-Icon $icon $outPath
            [IE]::DestroyIcon($fi.hIcon)
            $success = $true
        }}
    }} catch {{}}

    # Method 2: ExtractAssociatedIcon for Win32
    if (-not $success) {{
        try {{
            $item = $allItems | Where-Object {{ $_.Path -eq $appId }} | Select-Object -First 1
            if ($item -and (Test-Path $item.Path)) {{
                $icon = [System.Drawing.Icon]::ExtractAssociatedIcon($item.Path)
                if ($icon) {{ Save-Icon $icon $outPath; $success = $true }}
            }}
        }} catch {{}}
    }}

    # Method 3: UWP manifest
    if (-not $success) {{
        try {{
            $pkg = Get-AppxPackage | Where-Object {{ $_.PackageFamilyName -eq $appId.Split('!')[0] }} | Select-Object -First 1
            if ($pkg) {{
                [xml]$mf = Get-Content (Join-Path $pkg.InstallLocation 'AppxManifest.xml') -Raw
                $rel = $mf.Package.Properties.Logo
                if (-not $rel) {{ $rel = $mf.Package.Applications.Application.VisualElements.Square44x44Logo }}
                if ($rel) {{
                    $base = Join-Path $pkg.InstallLocation $rel
                    @('.scale-200.png','.scale-150.png','.scale-100.png','') | ForEach-Object {{
                        $c = $base -replace '\.png$',$_
                        if (-not $success -and (Test-Path $c)) {{
                            $src = [System.Drawing.Image]::FromFile($c)
                            $bmp = New-Object System.Drawing.Bitmap(64,64)
                            $g   = [System.Drawing.Graphics]::FromImage($bmp)
                            $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
                            $g.DrawImage($src,0,0,64,64); $g.Dispose(); $src.Dispose()
                            $bmp.Save($outPath,[System.Drawing.Imaging.ImageFormat]::Png)
                            $bmp.Dispose(); $success = $true
                        }}
                    }}
                }}
            }}
        }} catch {{}}
    }}
    Write-Output "$appId=$success"
}}
"#,
        jobs = jobs
    );

    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            &ps,
        ])
        .output();

    if let Ok(out) = output {
        let results = String::from_utf8_lossy(&out.stdout);
        for line in results.lines() {
            if line.contains("=True") {
                // Find which app this was and encrypt + cache it
                let app_id = line.split('=').next().unwrap_or("").trim();
                if let Some((_, path)) = app_ids.iter().find(|(id, _)| id == app_id) {
                    if let Ok(bytes) = fs::read(path) {
                        if !bytes.is_empty() {
                            let encrypted = encrypt_bytes(&bytes);
                            let _ = fs::write(path, &encrypted);
                        }
                    }
                }
            }
        }
    }
}

/// Reads an icon file from disk, decrypting if needed, returns base64
fn icon_path_to_b64(icon_path: &std::path::PathBuf) -> Option<String> {
    if !icon_path.exists() {
        return None;
    }
    let data = fs::read(icon_path).ok()?;
    if data.is_empty() {
        return None;
    }
    // Try decrypt first (encrypted cache), fall back to raw PNG
    let bytes = decrypt_bytes(&data).unwrap_or(data);
    if bytes.is_empty() {
        return None;
    }
    Some(base64_encode(&bytes))
}

fn get_icon_base64(app_id: &str) -> Option<String> {
    // Use a safe filename for the cache
    let safe_name: String = app_id
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect();
    let icon_path = get_icons_dir().join(format!("{}.png", &safe_name[..safe_name.len().min(80)]));

    // Return cached version if it exists (stored encrypted)
    if icon_path.exists() {
        if let Ok(encrypted) = fs::read(&icon_path) {
            if let Some(bytes) = decrypt_bytes(&encrypted) {
                if !bytes.is_empty() {
                    return Some(base64_encode(&bytes));
                }
            }
        }
    }

    let icon_path_str = icon_path.to_string_lossy().replace('\\', "\\\\");

    // This script uses SHGetFileInfo which works for EVERY app type on Windows
    let ps = format!(
        r#"
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Drawing;
using System.Runtime.InteropServices;
public class IconExtractor {{
    [DllImport("shell32.dll", CharSet = CharSet.Auto)]
    public static extern IntPtr ExtractIcon(IntPtr hInst, string lpszExeFileName, int nIconIndex);

    [DllImport("shell32.dll", CharSet = CharSet.Unicode)]
    public static extern uint SHGetFileInfo(string pszPath, uint dwFileAttributes, ref SHFILEINFO psfi, uint cbSizeFileInfo, uint uFlags);

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct SHFILEINFO {{
        public IntPtr hIcon;
        public int iIcon;
        public uint dwAttributes;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 260)]
        public string szDisplayName;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 80)]
        public string szTypeName;
    }}
    public const uint SHGFI_ICON = 0x100;
    public const uint SHGFI_LARGEICON = 0x0;
    public const uint SHGFI_USEFILEATTRIBUTES = 0x10;

    [DllImport("user32.dll", SetLastError = true)]
    public static extern bool DestroyIcon(IntPtr hIcon);
}}
"@

$appId = '{app_id}'
$outPath = '{out_path}'
$success = $false

# Method 1: Shell.Application thumbnail (best quality, works for UWP + Win32)
try {{
    $shell = New-Object -ComObject Shell.Application
    $appsFolder = $shell.Namespace('shell:AppsFolder')
    $item = $appsFolder.Items() | Where-Object {{ $_.Path -eq $appId }} | Select-Object -First 1
    if ($item) {{
        # Get the icon via SHFILEINFO on the virtual path
        $fi = New-Object IconExtractor+SHFILEINFO
        $r = [IconExtractor]::SHGetFileInfo("shell:AppsFolder\$appId", 0, [ref]$fi, [System.Runtime.InteropServices.Marshal]::SizeOf($fi), [IconExtractor]::SHGFI_ICON -bor [IconExtractor]::SHGFI_LARGEICON)
        if ($fi.hIcon -ne [IntPtr]::Zero) {{
            $icon = [System.Drawing.Icon]::FromHandle($fi.hIcon)
            $bmp = $icon.ToBitmap()
            # Resize to 64x64
            $resized = New-Object System.Drawing.Bitmap(64, 64)
            $g = [System.Drawing.Graphics]::FromImage($resized)
            $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
            $g.DrawImage($bmp, 0, 0, 64, 64)
            $g.Dispose()
            $resized.Save($outPath, [System.Drawing.Imaging.ImageFormat]::Png)
            [IconExtractor]::DestroyIcon($fi.hIcon)
            $success = $true
        }}
    }}
}} catch {{ }}

# Method 2: For Win32 apps, try ExtractAssociatedIcon directly on the exe
if (-not $success) {{
    try {{
        $shell = New-Object -ComObject Shell.Application
        $appsFolder = $shell.Namespace('shell:AppsFolder')
        $item = $appsFolder.Items() | Where-Object {{ $_.Path -eq $appId }} | Select-Object -First 1
        if ($item) {{
            $exePath = $item.Path
            if (Test-Path $exePath) {{
                $icon = [System.Drawing.Icon]::ExtractAssociatedIcon($exePath)
                if ($icon) {{
                    $bmp = $icon.ToBitmap()
                    $resized = New-Object System.Drawing.Bitmap(64, 64)
                    $g = [System.Drawing.Graphics]::FromImage($resized)
                    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
                    $g.DrawImage($bmp, 0, 0, 64, 64)
                    $g.Dispose()
                    $resized.Save($outPath, [System.Drawing.Imaging.ImageFormat]::Png)
                    $success = $true
                }}
            }}
        }}
    }} catch {{ }}
}}

# Method 3: UWP — find logo in package manifest
if (-not $success) {{
    try {{
        $familyName = $appId.Split('!')[0]
        $pkg = Get-AppxPackage | Where-Object {{ $_.PackageFamilyName -eq $familyName }} | Select-Object -First 1
        if ($pkg) {{
            [xml]$mf = Get-Content (Join-Path $pkg.InstallLocation 'AppxManifest.xml') -Raw
            $logoRel = $mf.Package.Properties.Logo
            if (-not $logoRel) {{
                $logoRel = $mf.Package.Applications.Application.VisualElements.Square44x44Logo
            }}
            if ($logoRel) {{
                $base = Join-Path $pkg.InstallLocation $logoRel
                $candidates = @(
                    ($base -replace '\.png$', '.scale-200.png'),
                    ($base -replace '\.png$', '.scale-150.png'),
                    ($base -replace '\.png$', '.scale-100.png'),
                    $base
                )
                foreach ($c in $candidates) {{
                    if (Test-Path $c) {{
                        $src = [System.Drawing.Image]::FromFile($c)
                        $resized = New-Object System.Drawing.Bitmap(64, 64)
                        $g = [System.Drawing.Graphics]::FromImage($resized)
                        $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
                        $g.DrawImage($src, 0, 0, 64, 64)
                        $g.Dispose()
                        $src.Dispose()
                        $resized.Save($outPath, [System.Drawing.Imaging.ImageFormat]::Png)
                        $success = $true
                        break
                    }}
                }}
            }}
        }}
    }} catch {{ }}
}}

Write-Output $success
"#,
        app_id = app_id.replace("'", "''"),
        out_path = icon_path_str
    );

    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            &ps,
        ])
        .output()
        .ok()?;

    let result = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if result.contains("True") {
        if let Ok(bytes) = fs::read(&icon_path) {
            if !bytes.is_empty() {
                // Encrypt before caching to disk
                let encrypted = encrypt_bytes(&bytes);
                let _ = fs::write(&icon_path, &encrypted);
                return Some(base64_encode(&bytes));
            }
        }
    }
    None
}

fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as usize;
        let b1 = if chunk.len() > 1 {
            chunk[1] as usize
        } else {
            0
        };
        let b2 = if chunk.len() > 2 {
            chunk[2] as usize
        } else {
            0
        };
        result.push(CHARS[b0 >> 2] as char);
        result.push(CHARS[((b0 & 3) << 4) | (b1 >> 4)] as char);
        result.push(if chunk.len() > 1 {
            CHARS[((b1 & 15) << 2) | (b2 >> 6)] as char
        } else {
            '='
        });
        result.push(if chunk.len() > 2 {
            CHARS[b2 & 63] as char
        } else {
            '='
        });
    }
    result
}

#[tauri::command]
fn get_installed_apps(app_handle: AppHandle, state: State<AppState>) -> Vec<AppData> {
    // Return cached apps instantly if we already have them
    // but still check for missing icons in background
    {
        let app_state = state.data.lock().unwrap();
        if !app_state.apps.is_empty() {
            let apps = app_state.apps.clone();
            // Check if any apps still need icons
            let missing: Vec<(String, std::path::PathBuf)> = apps
                .iter()
                .filter(|a| a.icon_base64.is_none())
                .map(|a| {
                    let safe_name: String = a
                        .app_id
                        .chars()
                        .map(|c| if c.is_alphanumeric() { c } else { '_' })
                        .collect();
                    let path = get_icons_dir()
                        .join(format!("{}.png", &safe_name[..safe_name.len().min(80)]));
                    (a.app_id.clone(), path)
                })
                .collect();
            if !missing.is_empty() {
                let data_arc = Arc::clone(&state.data);
                thread::spawn(move || {
                    batch_extract_icons(&missing);
                    for (app_id, icon_path) in &missing {
                        let b64 = icon_path_to_b64(icon_path);
                        if let Some(b64) = b64 {
                            let mut lock = data_arc.lock().unwrap();
                            if let Some(app) = lock.apps.iter_mut().find(|a| a.app_id == *app_id) {
                                app.icon_base64 = Some(b64.clone());
                            }
                            let _ = app_handle.emit(
                                "icon-ready",
                                serde_json::json!({
                                    "app_id": app_id,
                                    "icon_base64": b64,
                                }),
                            );
                        }
                    }
                });
            }
            return apps;
        }
    }

    // First ever launch — fetch from PowerShell
    let output = Command::new("powershell")
        .args([
            "-Command",
            r#"
            Get-StartApps |
            Where-Object {
                $_.Name -notmatch 'Windows (Security|Defender|Update|Terminal|Subsystem|Accessories|Administrative|Backup|Ease|Media|Mobility|Narrator|Recovery|Remote|Speech|System|Tools|Utility|Photo)' -and
                $_.Name -notmatch '^(Microsoft (Store|Edge|Teams|OneDrive|Outlook|To Do|News|Weather|Maps|Bing|Xbox Game Bar|Clipchamp|Get Help|Mixed Reality|Phone Link|Quick Assist|Sticky Notes|Tips|Whiteboard|Family Safety))$' -and
                $_.Name -notmatch 'Runtime|Security Center|Shell Experience' -and
                $_.AppID -notmatch 'windows\.'
            } |
            Sort-Object Name |
            ForEach-Object {
                "$($_.Name)|$($_.AppID)"
            }
            "#,
        ])
        .output()
        .expect("Failed to execute PowerShell");

    let result = String::from_utf8_lossy(&output.stdout);
    let mut app_state = state.data.lock().unwrap();
    let mut updated_apps: Vec<AppData> = vec![];
    let mut needs_icons: Vec<(usize, String)> = vec![]; // (index, app_id)

    for line in result.lines().filter(|l| !l.trim().is_empty()) {
        let mut parts = line.split('|');
        let name = parts.next().unwrap_or("").trim().to_string();
        let app_id = parts.next().unwrap_or("").trim().to_string();
        if name.is_empty() || app_id.is_empty() {
            continue;
        }

        let existing = app_state.apps.iter().find(|a| a.app_id == app_id);
        if let Some(existing_app) = existing {
            let mut app = existing_app.clone();
            // Check if icon is already on disk cache even if not in memory
            if app.icon_base64.is_none() {
                let safe_name: String = app_id
                    .chars()
                    .map(|c| if c.is_alphanumeric() { c } else { '_' })
                    .collect();
                let icon_path =
                    get_icons_dir().join(format!("{}.png", &safe_name[..safe_name.len().min(80)]));
                if icon_path.exists() {
                    if let Ok(encrypted) = fs::read(&icon_path) {
                        if let Some(bytes) = decrypt_bytes(&encrypted) {
                            if !bytes.is_empty() {
                                app.icon_base64 = Some(base64_encode(&bytes));
                            }
                        }
                    }
                }
            }
            if app.icon_base64.is_none() {
                needs_icons.push((updated_apps.len(), app_id.clone()));
            }
            updated_apps.push(app);
        } else {
            // New app — check disk cache first
            let safe_name: String = app_id
                .chars()
                .map(|c| if c.is_alphanumeric() { c } else { '_' })
                .collect();
            let icon_path =
                get_icons_dir().join(format!("{}.png", &safe_name[..safe_name.len().min(80)]));
            let icon_base64 = if icon_path.exists() {
                fs::read(&icon_path)
                    .ok()
                    .and_then(|enc| decrypt_bytes(&enc))
                    .filter(|b| !b.is_empty())
                    .map(|b| base64_encode(&b))
            } else {
                needs_icons.push((updated_apps.len(), app_id.clone()));
                None
            };
            updated_apps.push(AppData {
                name,
                app_id,
                category: "Uncategorized".to_string(),
                icon_base64,
            });
        }
    }

    app_state.apps = updated_apps;
    if !app_state.categories.contains(&"Uncategorized".to_string()) {
        app_state.categories.insert(0, "Uncategorized".to_string());
    }
    save_config(&app_state);

    // Step 2: spawn background thread — batch extract ALL missing icons in one PowerShell call
    if !needs_icons.is_empty() {
        let data_arc = Arc::clone(&state.data);
        let icons_dir = get_icons_dir();

        // Build (app_id, output_path) pairs for batch extraction
        let batch: Vec<(String, std::path::PathBuf)> = needs_icons
            .iter()
            .map(|(_, id)| {
                let safe_name: String = id
                    .chars()
                    .map(|c| if c.is_alphanumeric() { c } else { '_' })
                    .collect();
                let path = icons_dir.join(format!("{}.png", &safe_name[..safe_name.len().min(80)]));
                (id.clone(), path)
            })
            .collect();

        thread::spawn(move || {
            // One PowerShell call for all apps
            batch_extract_icons(&batch);
            // Read, decrypt and emit each icon
            for (app_id, icon_path) in &batch {
                if let Some(b64) = icon_path_to_b64(icon_path) {
                    {
                        let mut lock = data_arc.lock().unwrap();
                        if let Some(app) = lock.apps.iter_mut().find(|a| a.app_id == *app_id) {
                            app.icon_base64 = Some(b64.clone());
                        }
                    }
                    let _ = app_handle.emit(
                        "icon-ready",
                        serde_json::json!({
                            "app_id": app_id,
                            "icon_base64": b64,
                        }),
                    );
                }
            }
        });
    }

    app_state.apps.clone()
}

#[tauri::command]
fn get_categories(state: State<AppState>) -> Vec<String> {
    let app_state = state.data.lock().unwrap();
    app_state.categories.clone()
}

#[tauri::command]
fn add_category(name: String, state: State<AppState>) {
    let mut app_state = state.data.lock().unwrap();
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return;
    }
    if !app_state.categories.contains(&trimmed.to_string()) {
        app_state.categories.push(trimmed.to_string());
        save_config(&app_state);
    }
}

#[tauri::command]
#[allow(non_snake_case)]
fn assign_app_to_category(
    appId: String,
    category: String,
    state: State<AppState>,
) -> Result<(), String> {
    let app_id = appId;
    let mut app_state = state.data.lock().unwrap();
    match app_state.apps.iter_mut().find(|a| a.app_id == app_id) {
        Some(app) => {
            app.category = category;
            save_config(&app_state);
            Ok(())
        }
        None => Err(format!("App '{}' not found", app_id)),
    }
}

#[tauri::command]
#[allow(non_snake_case)]
fn open_app(appId: String) -> Result<(), String> {
    let app_id = appId;
    let ps_script = format!(
        "Start-Process 'shell:appsFolder\\{}'",
        app_id.replace("'", "''")
    );
    let status = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            &ps_script,
        ])
        .status();
    match status {
        Ok(s) if s.success() => Ok(()),
        _ => {
            Command::new("explorer")
                .arg(format!("shell:appsFolder\\{}", app_id))
                .spawn()
                .map_err(|e| e.to_string())?;
            Ok(())
        }
    }
}

// ── Analytics Commands ──────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct SessionRow {
    id: i64,
    app_id: String,
    app_name: String,
    category: String,
    opened_at: String,
    closed_at: Option<String>,
    duration_secs: Option<i64>,
}

#[derive(Debug, Serialize)]
struct DailySummaryRow {
    date: String,
    app_id: String,
    app_name: String,
    total_secs: i64,
    open_count: i64,
}

#[derive(Debug, Serialize)]
struct HourlyUsageRow {
    hour: i64,
    total_secs: i64,
}

/// Called when user opens an app — closes previous session, starts new one,
/// and spawns a background thread that auto-closes after 30 min of inactivity.
#[tauri::command]
#[allow(non_snake_case)]
fn log_app_open(
    appId: String,
    appName: String,
    category: String,
    state: State<AppState>,
) -> Result<(), String> {
    let app_id = appId;
    let app_name = appName;
    // Close existing session first
    close_current_session(&state)?;

    let conn = open_db().map_err(|e| e.to_string())?;
    let now = chrono_now();
    conn.execute(
        "INSERT INTO app_sessions (app_id, app_name, category, opened_at) VALUES (?1, ?2, ?3, ?4)",
        params![app_id, app_name, category, now],
    )
    .map_err(|e| e.to_string())?;

    let session_id = conn.last_insert_rowid();

    // Store active session
    {
        let mut session = state.session.lock().unwrap();
        *session = Some(ActiveSession {
            session_id,
            app_id: app_id.clone(),
            app_name: app_name.clone(),
            started_at: std::time::Instant::now(),
        });
    }

    // Spawn background auto-close after 30 min
    let session_arc = Arc::clone(&state.session);
    thread::spawn(move || {
        thread::sleep(std::time::Duration::from_secs(30 * 60));
        // Only close if this session is still active
        let sess = {
            let s = session_arc.lock().unwrap();
            s.clone()
        };
        if let Some(s) = sess {
            if s.session_id == session_id {
                let _ = write_session_close(session_id, &s.app_id, &s.app_name);
                let mut lock = session_arc.lock().unwrap();
                *lock = None;
            }
        }
    });

    Ok(())
}

/// Called when app loses focus or user switches away — closes the active session
#[tauri::command]
fn log_app_close(state: State<AppState>) -> Result<(), String> {
    close_current_session(&state)
}

fn close_current_session(state: &State<AppState>) -> Result<(), String> {
    let sess = {
        let mut lock = state.session.lock().unwrap();
        lock.take() // atomically take and clear
    };
    if let Some(s) = sess {
        write_session_close(s.session_id, &s.app_id, &s.app_name)?;
    }
    Ok(())
}

fn write_session_close(session_id: i64, app_id: &str, app_name: &str) -> Result<(), String> {
    let conn = open_db().map_err(|e| e.to_string())?;
    let now = chrono_now();

    let opened_at: String = conn
        .query_row(
            "SELECT opened_at FROM app_sessions WHERE id = ?1",
            params![session_id],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;

    let duration = duration_secs(&opened_at, &now);

    conn.execute(
        "UPDATE app_sessions SET closed_at = ?1, duration_secs = ?2 WHERE id = ?3",
        params![now, duration, session_id],
    )
    .map_err(|e| e.to_string())?;

    let date = &now[..10];
    conn.execute(
        "INSERT INTO daily_summary (date, app_id, app_name, total_secs, open_count)
         VALUES (?1, ?2, ?3, ?4, 1)
         ON CONFLICT(date, app_id) DO UPDATE SET
             total_secs = total_secs + ?4,
             open_count = open_count + 1",
        params![date, app_id, app_name, duration],
    )
    .map_err(|e| e.to_string())?;

    Ok(())
}

/// Last N days of daily totals (all apps combined) — for the bar chart
#[tauri::command]
fn get_daily_totals(days: i64) -> Result<Vec<DailySummaryRow>, String> {
    let conn = open_db().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT date, app_id, app_name, SUM(total_secs), SUM(open_count)
         FROM daily_summary
         WHERE date >= date('now', ?1)
         GROUP BY date, app_id
         ORDER BY date ASC",
        )
        .map_err(|e| e.to_string())?;

    let arg = format!("-{} days", days);
    let rows = stmt
        .query_map(params![arg], |row| {
            Ok(DailySummaryRow {
                date: row.get(0)?,
                app_id: row.get(1)?,
                app_name: row.get(2)?,
                total_secs: row.get(3)?,
                open_count: row.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

/// Per-app totals for the donut chart
#[tauri::command]
fn get_app_totals(days: i64) -> Result<Vec<DailySummaryRow>, String> {
    let conn = open_db().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT date, app_id, app_name, SUM(total_secs), SUM(open_count)
         FROM daily_summary
         WHERE date >= date('now', ?1)
         GROUP BY app_id
         ORDER BY SUM(total_secs) DESC",
        )
        .map_err(|e| e.to_string())?;

    let arg = format!("-{} days", days);
    let rows = stmt
        .query_map(params![arg], |row| {
            Ok(DailySummaryRow {
                date: row.get(0)?,
                app_id: row.get(1)?,
                app_name: row.get(2)?,
                total_secs: row.get(3)?,
                open_count: row.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

/// Hourly usage breakdown — for the heatmap
#[tauri::command]
fn get_hourly_usage(days: i64) -> Result<Vec<HourlyUsageRow>, String> {
    let conn = open_db().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT CAST(strftime('%H', opened_at) AS INTEGER) as hour,
                SUM(COALESCE(duration_secs, 0)) as total_secs
         FROM app_sessions
         WHERE opened_at >= datetime('now', ?1)
         GROUP BY hour
         ORDER BY hour ASC",
        )
        .map_err(|e| e.to_string())?;

    let arg = format!("-{} days", days);
    let rows = stmt
        .query_map(params![arg], |row| {
            Ok(HourlyUsageRow {
                hour: row.get(0)?,
                total_secs: row.get(1)?,
            })
        })
        .map_err(|e| e.to_string())?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

/// Continuous sessions over 20 min — for eye strain detection
#[tauri::command]
fn get_long_sessions(days: i64) -> Result<Vec<SessionRow>, String> {
    let conn = open_db().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT id, app_id, app_name, category, opened_at, closed_at, duration_secs
         FROM app_sessions
         WHERE opened_at >= datetime('now', ?1)
           AND duration_secs > 1200  -- 20 minutes
         ORDER BY opened_at DESC",
        )
        .map_err(|e| e.to_string())?;

    let arg = format!("-{} days", days);
    let rows = stmt
        .query_map(params![arg], |row| {
            Ok(SessionRow {
                id: row.get(0)?,
                app_id: row.get(1)?,
                app_name: row.get(2)?,
                category: row.get(3)?,
                opened_at: row.get(4)?,
                closed_at: row.get(5)?,
                duration_secs: row.get(6)?,
            })
        })
        .map_err(|e| e.to_string())?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

/// Single combined payload — all analytics in one IPC call
#[derive(Debug, Serialize)]
struct AnalyticsPayload {
    daily_totals: Vec<DailySummaryRow>,
    app_totals: Vec<DailySummaryRow>,
    hourly_usage: Vec<HourlyUsageRow>,
    long_sessions: Vec<SessionRow>,
}

#[tauri::command]
fn get_analytics(days: i64) -> Result<AnalyticsPayload, String> {
    // Single connection, single open, all queries run together
    let conn = open_db().map_err(|e| e.to_string())?;
    let arg = format!("-{} days", days);

    // Query 1: daily totals — include unclosed sessions via julianday estimate
    let daily_totals = {
        let mut stmt = conn.prepare(
            "SELECT date(opened_at) as date, app_id, app_name,
                    SUM(COALESCE(duration_secs, CAST((julianday('now') - julianday(opened_at)) * 86400 AS INTEGER))) as total_secs,
                    COUNT(*) as open_count
             FROM app_sessions
             WHERE date(opened_at) >= date('now', ?1)
             GROUP BY date(opened_at), app_id
             ORDER BY date(opened_at) ASC",
        ).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![arg], |row| {
                Ok(DailySummaryRow {
                    date: row.get(0)?,
                    app_id: row.get(1)?,
                    app_name: row.get(2)?,
                    total_secs: row.get(3)?,
                    open_count: row.get(4)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        rows
    };

    // Query 2: per-app totals — from sessions directly
    let app_totals = {
        let mut stmt = conn.prepare(
            "SELECT date(opened_at), app_id, app_name,
                    SUM(COALESCE(duration_secs, CAST((julianday('now') - julianday(opened_at)) * 86400 AS INTEGER))) as total_secs,
                    COUNT(*) as open_count
             FROM app_sessions
             WHERE date(opened_at) >= date('now', ?1)
             GROUP BY app_id
             ORDER BY total_secs DESC",
        ).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![arg], |row| {
                Ok(DailySummaryRow {
                    date: row.get(0)?,
                    app_id: row.get(1)?,
                    app_name: row.get(2)?,
                    total_secs: row.get(3)?,
                    open_count: row.get(4)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        rows
    };

    // Query 3: hourly usage — all sessions including unclosed
    let hourly_usage = {
        let mut stmt = conn.prepare(
            "SELECT CAST(strftime('%H', opened_at) AS INTEGER) as hour,
                    SUM(COALESCE(duration_secs, CAST((julianday('now') - julianday(opened_at)) * 86400 AS INTEGER))) as total_secs
             FROM app_sessions
             WHERE opened_at >= datetime('now', ?1)
             GROUP BY hour
             ORDER BY hour ASC",
        ).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![arg], |row| {
                Ok(HourlyUsageRow {
                    hour: row.get(0)?,
                    total_secs: row.get(1)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        rows
    };

    // Query 4: long sessions > 20 min — include unclosed
    let long_sessions = {
        let mut stmt = conn.prepare(
            "SELECT id, app_id, app_name, category, opened_at, closed_at,
                    COALESCE(duration_secs, CAST((julianday('now') - julianday(opened_at)) * 86400 AS INTEGER)) as duration_secs
             FROM app_sessions
             WHERE opened_at >= datetime('now', ?1)
               AND COALESCE(duration_secs, CAST((julianday('now') - julianday(opened_at)) * 86400 AS INTEGER)) > 1200
             ORDER BY opened_at DESC
             LIMIT 50",
        ).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![arg], |row| {
                Ok(SessionRow {
                    id: row.get(0)?,
                    app_id: row.get(1)?,
                    app_name: row.get(2)?,
                    category: row.get(3)?,
                    opened_at: row.get(4)?,
                    closed_at: row.get(5)?,
                    duration_secs: row.get(6)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        rows
    };

    Ok(AnalyticsPayload {
        daily_totals,
        app_totals,
        hourly_usage,
        long_sessions,
    })
}

/// Debug command — returns raw DB state so we can see what's being stored
#[tauri::command]
fn debug_analytics() -> Result<serde_json::Value, String> {
    let conn = open_db().map_err(|e| e.to_string())?;

    // Count sessions
    let session_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM app_sessions", [], |row| row.get(0))
        .unwrap_or(0);

    // Count daily summary rows
    let summary_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM daily_summary", [], |row| row.get(0))
        .unwrap_or(0);

    // Last 5 sessions
    let mut stmt = conn.prepare(
        "SELECT id, app_name, opened_at, closed_at, duration_secs FROM app_sessions ORDER BY id DESC LIMIT 5"
    ).map_err(|e| e.to_string())?;

    let sessions: Vec<serde_json::Value> = stmt
        .query_map([], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, i64>(0)?,
                "app_name": row.get::<_, String>(1)?,
                "opened_at": row.get::<_, String>(2)?,
                "closed_at": row.get::<_, Option<String>>(3)?,
                "duration_secs": row.get::<_, Option<i64>>(4)?,
            }))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    // DB path
    let db_path = get_db_path().to_string_lossy().to_string();
    let now = chrono_now();

    Ok(serde_json::json!({
        "db_path": db_path,
        "now": now,
        "session_count": session_count,
        "summary_count": summary_count,
        "last_5_sessions": sessions,
    }))
}

// ── Background Window Tracker ────────────────────────────────────────────────

/// Returns the exe name of the currently focused window (Windows only)
#[cfg(windows)]
fn get_foreground_exe() -> Option<String> {
    unsafe {
        let hwnd = GetForegroundWindow();
        // HWND is a wrapper; check if null
        if hwnd == HWND(std::ptr::null_mut()) {
            return None;
        }

        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }

        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;

        let mut buf = vec![0u16; 512];
        let mut size = buf.len() as u32;
        let _ = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut size,
        );
        let _ = CloseHandle(handle);

        let path = String::from_utf16_lossy(&buf[..size as usize]);
        std::path::Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
    }
}

#[cfg(not(windows))]
fn get_foreground_exe() -> Option<String> {
    None
}

/// Maps exe names to friendly app names
fn exe_to_app_name(exe: &str) -> Option<&'static str> {
    match exe {
        "brave.exe" => Some("Brave"),
        "chrome.exe" => Some("Google Chrome"),
        "firefox.exe" => Some("Firefox"),
        "msedge.exe" => Some("Microsoft Edge"),
        "opera.exe" => Some("Opera"),
        "code.exe" => Some("VS Code"),
        "devenv.exe" => Some("Visual Studio"),
        "spotify.exe" => Some("Spotify"),
        "discord.exe" => Some("Discord"),
        "slack.exe" => Some("Slack"),
        "teams.exe" => Some("Microsoft Teams"),
        "notepad.exe" => Some("Notepad"),
        "explorer.exe" => Some("File Explorer"),
        "winword.exe" => Some("Microsoft Word"),
        "excel.exe" => Some("Microsoft Excel"),
        "powerpnt.exe" => Some("Microsoft PowerPoint"),
        "photoshop.exe" => Some("Adobe Photoshop"),
        "figma.exe" => Some("Figma"),
        "obsidian.exe" => Some("Obsidian"),
        "notion.exe" => Some("Notion"),
        "vlc.exe" => Some("VLC"),
        "steam.exe" => Some("Steam"),
        _ => None,
    }
}

/// Spawns the background foreground-window tracker thread
fn start_background_tracker(bg_session: Arc<Mutex<Option<BgSession>>>) {
    thread::spawn(move || {
        let poll_interval = std::time::Duration::from_secs(5);
        let min_session = std::time::Duration::from_secs(10); // ignore <10s flickers

        loop {
            thread::sleep(poll_interval);

            let current_exe = match get_foreground_exe() {
                Some(e) => e,
                None => continue,
            };

            // Skip system/unknown processes
            let app_name = match exe_to_app_name(&current_exe) {
                Some(n) => n,
                None => continue,
            };

            let mut lock = bg_session.lock().unwrap();

            match lock.as_ref() {
                Some(sess) if sess.exe_name == current_exe => {
                    // Same app still focused — nothing to do
                }
                _ => {
                    // App switched — close old session
                    if let Some(old) = lock.take() {
                        if old.started_at.elapsed() >= min_session {
                            let duration = old.started_at.elapsed().as_secs() as i64;
                            let _ =
                                write_bg_session_close(&old.exe_name, &old.opened_at_str, duration);
                        }
                    }

                    // Start new session
                    let now_str = chrono_now();
                    *lock = Some(BgSession {
                        exe_name: current_exe.clone(),
                        started_at: std::time::Instant::now(),
                        opened_at_str: now_str.clone(),
                    });

                    // Insert open record into DB
                    if let Ok(conn) = open_db() {
                        let _ = conn.execute(
                            "INSERT INTO app_sessions (app_id, app_name, category, opened_at)
                             VALUES (?1, ?2, 'Background', ?3)",
                            rusqlite::params![current_exe, app_name, now_str],
                        );
                    }
                }
            }
        }
    });
}

fn write_bg_session_close(exe: &str, opened_at: &str, duration: i64) {
    if let Ok(conn) = open_db() {
        let now = chrono_now();
        let date = &now[..10];

        // Find the open session
        let session_id: Option<i64> = conn.query_row(
            "SELECT id FROM app_sessions WHERE app_id = ?1 AND opened_at = ?2 AND closed_at IS NULL",
            rusqlite::params![exe, opened_at],
            |row| row.get(0),
        ).ok();

        if let Some(id) = session_id {
            let _ = conn.execute(
                "UPDATE app_sessions SET closed_at = ?1, duration_secs = ?2 WHERE id = ?3",
                rusqlite::params![now, duration, id],
            );
        }

        // Upsert daily summary
        let app_name = exe_to_app_name(exe).unwrap_or(exe);
        let _ = conn.execute(
            "INSERT INTO daily_summary (date, app_id, app_name, total_secs, open_count)
             VALUES (?1, ?2, ?3, ?4, 1)
             ON CONFLICT(date, app_id) DO UPDATE SET
                 total_secs = total_secs + ?4,
                 open_count = open_count + 1",
            rusqlite::params![date, exe, app_name, duration],
        );
    }
}

// ── Time helpers ─────────────────────────────────────────────────────────────

fn chrono_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Convert unix timestamp to YYYY-MM-DD HH:MM:SS (UTC)
    let s = secs;
    let days = s / 86400;
    let time = s % 86400;
    let h = time / 3600;
    let m = (time % 3600) / 60;
    let sec = time % 60;
    // Date from days since epoch
    let mut y = 1970u64;
    let mut d = days;
    loop {
        let days_in_year = if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
            366
        } else {
            365
        };
        if d < days_in_year {
            break;
        }
        d -= days_in_year;
        y += 1;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let months = [
        31u64,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut mo = 1u64;
    for days_in_month in &months {
        if d < *days_in_month {
            break;
        }
        d -= days_in_month;
        mo += 1;
    }
    let day = d + 1;
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", y, mo, day, h, m, sec)
}

fn duration_secs(opened_at: &str, closed_at: &str) -> i64 {
    let parse = |s: &str| -> Option<i64> {
        let parts: Vec<&str> = s.split(|c| c == '-' || c == ' ' || c == ':').collect();
        if parts.len() < 6 {
            return None;
        }
        let y: i64 = parts[0].parse().ok()?;
        let mo: i64 = parts[1].parse().ok()?;
        let d: i64 = parts[2].parse().ok()?;
        let h: i64 = parts[3].parse().ok()?;
        let mi: i64 = parts[4].parse().ok()?;
        let s: i64 = parts[5].parse().ok()?;
        // Simple epoch-like calculation (good enough for duration)
        Some(((y * 365 + mo * 30 + d) * 86400) + h * 3600 + mi * 60 + s)
    };
    match (parse(opened_at), parse(closed_at)) {
        (Some(a), Some(b)) => (b - a).max(0),
        _ => 0,
    }
}

fn main() {
    setup_db();

    let app_state = AppState {
        data: Arc::new(Mutex::new(load_config())),
        session: Arc::new(Mutex::new(None)),
        bg_session: Arc::new(Mutex::new(None)),
    };

    // Start background window tracker
    let bg_arc = Arc::clone(&app_state.bg_session);
    start_background_tracker(bg_arc);

    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            get_installed_apps,
            get_categories,
            add_category,
            assign_app_to_category,
            open_app,
            log_app_open,
            log_app_close,
            get_daily_totals,
            get_app_totals,
            get_hourly_usage,
            get_long_sessions,
            debug_analytics,
            get_analytics
        ])
        .setup(|app| {
            // Pre-warm the app list in background so it's ready when user opens window
            let data_arc = Arc::clone(&app.state::<AppState>().data);
            let app_handle_clone = app.handle().clone();
            thread::spawn(move || {
                // Run PowerShell scan in background
                let output = Command::new("powershell")
                    .args([
                        "-Command",
                        r#"
                        Get-StartApps |
                        Where-Object {
                            $_.Name -notmatch 'Windows (Security|Defender|Update|Terminal|Subsystem|Accessories|Administrative|Backup|Ease|Media|Mobility|Narrator|Recovery|Remote|Speech|System|Tools|Utility|Photo)' -and
                            $_.Name -notmatch '^(Microsoft (Store|Edge|Teams|OneDrive|Outlook|To Do|News|Weather|Maps|Bing|Xbox Game Bar|Clipchamp|Get Help|Mixed Reality|Phone Link|Quick Assist|Sticky Notes|Tips|Whiteboard|Family Safety))$' -and
                            $_.Name -notmatch 'Runtime|Security Center|Shell Experience' -and
                            $_.AppID -notmatch 'windows\.'
                        } |
                        Sort-Object Name |
                        ForEach-Object {
                            "$($_.Name)|$($_.AppID)"
                        }
                        "#,
                    ])
                    .output();

                if let Ok(out) = output {
                    let result = String::from_utf8_lossy(&out.stdout);
                    let mut app_state = data_arc.lock().unwrap();
                    if app_state.apps.is_empty() {
                        let config = load_config();
                        let mut updated = vec![];
                        for line in result.lines().filter(|l| !l.trim().is_empty()) {
                            let mut parts = line.split('|');
                            let name = parts.next().unwrap_or("").trim().to_string();
                            let app_id = parts.next().unwrap_or("").trim().to_string();
                            if name.is_empty() || app_id.is_empty() { continue; }
                            let existing = config.apps.iter().find(|a| a.app_id == app_id);
                            updated.push(if let Some(e) = existing {
                                e.clone()
                            } else {
                                AppData { name, app_id, category: "Uncategorized".to_string(), icon_base64: None }
                            });
                        }
                        app_state.apps = updated;
                        // Notify frontend apps are ready
                        let _ = app_handle_clone.emit("apps-ready", ());
                    }
                }
            });

            // Build tray menu
            let open_drawer = MenuItemBuilder::new("📦 Open AppDrawer")
                .id("open")
                .build(app)?;
            let show_analytics = MenuItemBuilder::new("📊 Analytics")
                .id("analytics")
                .build(app)?;
            let quit = MenuItemBuilder::new("✕ Quit")
                .id("quit")
                .build(app)?;

            let menu = MenuBuilder::new(app)
                .item(&open_drawer)
                .item(&show_analytics)
                .separator()
                .item(&quit)
                .build()?;

            // Build tray icon
            TrayIconBuilder::new()
                .menu(&menu)
                .tooltip("AppDrawer — running in background")
                .icon(app.default_window_icon().unwrap().clone())
                .on_menu_event(|app, event| {
                    match event.id().as_ref() {
                        "open" => {
                            if let Some(w) = app.get_webview_window("main") {
                                let _ = w.show();
                                let _ = w.set_focus();
                            }
                        }
                        "analytics" => {
                            if let Some(w) = app.get_webview_window("main") {
                                let _ = w.show();
                                let _ = w.set_focus();
                                let _ = w.emit("open-analytics", ());
                            }
                        }
                        "quit" => {
                            app.exit(0);
                        }
                        _ => {}
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event {
                        let app = tray.app_handle();
                        if let Some(w) = app.get_webview_window("main") {
                            if w.is_visible().unwrap_or(false) {
                                let _ = w.hide();
                            } else {
                                let _ = w.show();
                                let _ = w.set_focus();
                            }
                        }
                    }
                })
                .build(app)?;

            // Register global shortcut Ctrl+Space to toggle window
            let app_handle = app.handle().clone();
            app.global_shortcut().on_shortcut(
                "CommandOrControl+Space",
                move |_app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        if let Some(w) = app_handle.get_webview_window("main") {
                            if w.is_visible().unwrap_or(false) {
                                let _ = w.hide();
                            } else {
                                let _ = w.show();
                                let _ = w.set_focus();
                            }
                        }
                    }
                },
            )?;

            Ok(())
        })
        // Prevent closing — hide to tray instead
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("Error while running Tauri application");
}
