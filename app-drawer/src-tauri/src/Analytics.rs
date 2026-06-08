use crate::crypto::get_db_path;
use crate::state::{ActiveSession, AppState, BgSession};
use rusqlite::{Connection, params};
use serde::Serialize;
use std::fs;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::State;

// ── DB setup ──────────────────────────────────────────────────────────────────

pub fn open_db() -> rusqlite::Result<Connection> {
    let conn = Connection::open(get_db_path())?;
    conn.execute_batch(
        "
        PRAGMA journal_mode=WAL;
        PRAGMA synchronous=NORMAL;
        PRAGMA cache_size=-8000;
    ",
    )?;
    Ok(conn)
}

pub fn setup_db() {
    let db_path = get_db_path();
    if db_path.exists() {
        if let Ok(conn) = Connection::open(&db_path) {
            let ok: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='app_sessions'",
                    [],
                    |r| r.get::<_, i64>(0).map(|n| n > 0),
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
            app_id        TEXT NOT NULL,
            app_name      TEXT NOT NULL,
            category      TEXT NOT NULL DEFAULT 'Uncategorized',
            opened_at     TEXT NOT NULL,
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

// ── Time helpers ──────────────────────────────────────────────────────────────

pub fn chrono_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = secs / 86400;
    let time = secs % 86400;
    let (h, m, s) = (time / 3600, (time % 3600) / 60, time % 60);
    let mut y = 1970u64;
    let mut d = days;
    loop {
        let dy = if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
            366
        } else {
            365
        };
        if d < dy {
            break;
        }
        d -= dy;
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
    for dm in &months {
        if d < *dm {
            break;
        }
        d -= dm;
        mo += 1;
    }
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", y, mo, d + 1, h, m, s)
}

pub fn duration_secs(opened: &str, closed: &str) -> i64 {
    let parse = |s: &str| -> Option<i64> {
        let p: Vec<&str> = s.split(|c| c == '-' || c == ' ' || c == ':').collect();
        if p.len() < 6 {
            return None;
        }
        let (y, mo, d, h, mi, s): (i64, i64, i64, i64, i64, i64) = (
            p[0].parse().ok()?,
            p[1].parse().ok()?,
            p[2].parse().ok()?,
            p[3].parse().ok()?,
            p[4].parse().ok()?,
            p[5].parse().ok()?,
        );
        Some(((y * 365 + mo * 30 + d) * 86400) + h * 3600 + mi * 60 + s)
    };
    match (parse(opened), parse(closed)) {
        (Some(a), Some(b)) => (b - a).max(0),
        _ => 0,
    }
}

// ── Session management ────────────────────────────────────────────────────────

pub fn close_current_session(state: &State<AppState>) -> Result<(), String> {
    let sess = {
        let mut l = state.session.lock().unwrap();
        l.take()
    };
    if let Some(s) = sess {
        write_session_close(s.session_id, &s.app_id, &s.app_name)?;
    }
    Ok(())
}

pub fn write_session_close(id: i64, app_id: &str, app_name: &str) -> Result<(), String> {
    let conn = open_db().map_err(|e| e.to_string())?;
    let now = chrono_now();
    let opened: String = conn
        .query_row(
            "SELECT opened_at FROM app_sessions WHERE id=?1",
            params![id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let dur = duration_secs(&opened, &now);
    conn.execute(
        "UPDATE app_sessions SET closed_at=?1, duration_secs=?2 WHERE id=?3",
        params![now, dur, id],
    )
    .map_err(|e| e.to_string())?;
    let date = &now[..10];
    conn.execute(
        "INSERT INTO daily_summary (date,app_id,app_name,total_secs,open_count) VALUES(?1,?2,?3,?4,1)
         ON CONFLICT(date,app_id) DO UPDATE SET total_secs=total_secs+?4, open_count=open_count+1",
        params![date, app_id, app_name, dur],
    ).map_err(|e| e.to_string())?;
    Ok(())
}

// ── Background tracker ────────────────────────────────────────────────────────

#[cfg(windows)]
use windows::Win32::Foundation::{CloseHandle, HWND};
#[cfg(windows)]
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

#[cfg(windows)]
pub fn get_foreground_exe() -> Option<String> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd == HWND(std::ptr::null_mut()) {
            return None;
        }
        let mut pid = 0u32;
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
pub fn get_foreground_exe() -> Option<String> {
    None
}

pub fn exe_to_app_name(exe: &str) -> Option<&'static str> {
    match exe {
        "brave.exe" => Some("Brave"),
        "chrome.exe" => Some("Google Chrome"),
        "firefox.exe" => Some("Firefox"),
        "msedge.exe" => Some("Microsoft Edge"),
        "opera.exe" => Some("Opera"),
        "code.exe" => Some("Visual Studio Code"),
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
        "claude.exe" => Some("Claude"),
        "cursor.exe" => Some("Cursor"),
        "wt.exe" => Some("Windows Terminal"),
        "powershell.exe" => Some("PowerShell"),
        "cmd.exe" => Some("Command Prompt"),
        "taskmgr.exe" => Some("Task Manager"),
        "mspaint.exe" => Some("Paint"),
        "wordpad.exe" => Some("WordPad"),
        "calc.exe" => Some("Calculator"),
        "snippingtool.exe" => Some("Snipping Tool"),
        "postman.exe" => Some("Postman"),
        "insomnia.exe" => Some("Insomnia"),
        "dbeaver.exe" => Some("DBeaver"),
        "telegram.exe" => Some("Telegram"),
        "whatsapp.exe" => Some("WhatsApp"),
        "zoom.exe" => Some("Zoom"),
        _ => None,
    }
}

pub fn start_background_tracker(bg_session: Arc<Mutex<Option<BgSession>>>) {
    thread::spawn(move || {
        let poll = std::time::Duration::from_secs(5);
        let min_s = std::time::Duration::from_secs(10);
        loop {
            thread::sleep(poll);
            let Some(exe) = get_foreground_exe() else {
                continue;
            };
            // Use mapped name or fall back to exe name (capitalized) so ALL apps get tracked
            let app_name_owned =
                exe_to_app_name(&exe)
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| {
                        // Convert "myapp.exe" -> "Myapp"
                        let base = exe.trim_end_matches(".exe");
                        let mut c = base.chars();
                        match c.next() {
                            None => base.to_string(),
                            Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                        }
                    });
            let app_name = app_name_owned.as_str();
            let mut lock = bg_session.lock().unwrap();
            match lock.as_ref() {
                Some(s) if s.exe_name == exe => {}
                _ => {
                    if let Some(old) = lock.take() {
                        if old.started_at.elapsed() >= min_s {
                            let dur = old.started_at.elapsed().as_secs() as i64;
                            write_bg_session_close(&old.exe_name, &old.opened_at_str, dur);
                        }
                    }
                    let now = chrono_now();
                    *lock = Some(BgSession {
                        exe_name: exe.clone(),
                        started_at: std::time::Instant::now(),
                        opened_at_str: now.clone(),
                    });
                    if let Ok(conn) = open_db() {
                        let _ = conn.execute(
                            "INSERT INTO app_sessions (app_id,app_name,category,opened_at) VALUES(?1,?2,'Background',?3)",
                            params![exe, app_name, now],
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
        let date = now[..10].to_string();
        if let Ok(id) = conn.query_row::<i64, _, _>(
            "SELECT id FROM app_sessions WHERE app_id=?1 AND opened_at=?2 AND closed_at IS NULL",
            params![exe, opened_at],
            |r| r.get(0),
        ) {
            let _ = conn.execute(
                "UPDATE app_sessions SET closed_at=?1,duration_secs=?2 WHERE id=?3",
                params![now, duration, id],
            );
        }
        let app_name = exe_to_app_name(exe).unwrap_or(exe);
        let _ = conn.execute(
            "INSERT INTO daily_summary (date,app_id,app_name,total_secs,open_count) VALUES(?1,?2,?3,?4,1)
             ON CONFLICT(date,app_id) DO UPDATE SET total_secs=total_secs+?4,open_count=open_count+1",
            params![date, exe, app_name, duration],
        );
    }
}

// ── Row types ─────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct SessionRow {
    pub id: i64,
    pub app_id: String,
    pub app_name: String,
    pub category: String,
    pub opened_at: String,
    pub closed_at: Option<String>,
    pub duration_secs: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct DailySummaryRow {
    pub date: String,
    pub app_id: String,
    pub app_name: String,
    pub total_secs: i64,
    pub open_count: i64,
}

#[derive(Debug, Serialize)]
pub struct HourlyUsageRow {
    pub hour: i64,
    pub total_secs: i64,
}

#[derive(Debug, Serialize)]
pub struct AnalyticsPayload {
    pub daily_totals: Vec<DailySummaryRow>,
    pub app_totals: Vec<DailySummaryRow>,
    pub hourly_usage: Vec<HourlyUsageRow>,
    pub long_sessions: Vec<SessionRow>,
}

pub fn query_analytics(days: i64) -> Result<AnalyticsPayload, String> {
    let conn = open_db().map_err(|e| e.to_string())?;
    let arg = format!("-{} days", days);

    let filter = "closed_at IS NOT NULL AND duration_secs IS NOT NULL AND duration_secs > 0";

    let daily_totals = {
        let sql = format!(
            "SELECT date(opened_at), app_id, app_name,
                    SUM(MIN(duration_secs, 7200)), COUNT(*)
             FROM app_sessions
             WHERE date(opened_at) >= date('now', ?1)
               AND {filter}
             GROUP BY date(opened_at), app_name
             ORDER BY date(opened_at)"
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![arg], |r| {
                Ok(DailySummaryRow {
                    date: r.get(0)?,
                    app_id: r.get(1)?,
                    app_name: r.get(2)?,
                    total_secs: r.get(3)?,
                    open_count: r.get(4)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        rows
    };

    let app_totals = {
        let sql = format!(
            "SELECT date(opened_at), app_id, app_name,
                    SUM(MIN(duration_secs, 7200)), COUNT(*)
             FROM app_sessions
             WHERE date(opened_at) >= date('now', ?1)
               AND {filter}
             GROUP BY app_name
             ORDER BY SUM(MIN(duration_secs, 7200)) DESC"
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![arg], |r| {
                Ok(DailySummaryRow {
                    date: r.get(0)?,
                    app_id: r.get(1)?,
                    app_name: r.get(2)?,
                    total_secs: r.get(3)?,
                    open_count: r.get(4)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        rows
    };

    let hourly_usage = {
        let sql = format!(
            "SELECT CAST(strftime('%H', opened_at) AS INTEGER),
                    SUM(MIN(duration_secs, 7200))
             FROM app_sessions
             WHERE opened_at >= datetime('now', ?1)
               AND {filter}
             GROUP BY 1
             ORDER BY 1"
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![arg], |r| {
                Ok(HourlyUsageRow {
                    hour: r.get(0)?,
                    total_secs: r.get(1)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        rows
    };

    let long_sessions = {
        let sql = format!(
            "SELECT id, app_id, app_name, category, opened_at, closed_at, duration_secs
             FROM app_sessions
             WHERE opened_at >= datetime('now', ?1)
               AND {filter}
               AND duration_secs > 1200
             ORDER BY duration_secs DESC
             LIMIT 50"
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![arg], |r| {
                Ok(SessionRow {
                    id: r.get(0)?,
                    app_id: r.get(1)?,
                    app_name: r.get(2)?,
                    category: r.get(3)?,
                    opened_at: r.get(4)?,
                    closed_at: r.get(5)?,
                    duration_secs: r.get(6)?,
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

pub fn get_db_path_str() -> String {
    get_db_path().to_string_lossy().to_string()
}
