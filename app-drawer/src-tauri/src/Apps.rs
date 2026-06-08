use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::thread;
use tauri::{AppHandle, Emitter};

use crate::crypto::{decrypt_bytes, encrypt_bytes, get_icons_dir};
use crate::state::AppData;

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

pub fn icon_safe_name(app_id: &str) -> String {
    app_id
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect()
}

pub fn icon_path(app_id: &str) -> PathBuf {
    let name = icon_safe_name(app_id);
    get_icons_dir().join(format!("{}.png", &name[..name.len().min(80)]))
}

pub fn icon_path_to_b64(path: &PathBuf) -> Option<String> {
    if !path.exists() {
        return None;
    }
    let data = fs::read(path).ok()?;
    if data.is_empty() {
        return None;
    }
    let bytes = decrypt_bytes(&data).unwrap_or(data);
    if bytes.is_empty() {
        return None;
    }
    Some(base64_encode(&bytes))
}

pub fn base64_encode(data: &[u8]) -> String {
    const C: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut r = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let (b0, b1, b2) = (
            chunk[0] as usize,
            if chunk.len() > 1 {
                chunk[1] as usize
            } else {
                0
            },
            if chunk.len() > 2 {
                chunk[2] as usize
            } else {
                0
            },
        );
        r.push(C[b0 >> 2] as char);
        r.push(C[((b0 & 3) << 4) | (b1 >> 4)] as char);
        r.push(if chunk.len() > 1 {
            C[((b1 & 15) << 2) | (b2 >> 6)] as char
        } else {
            '='
        });
        r.push(if chunk.len() > 2 {
            C[b2 & 63] as char
        } else {
            '='
        });
    }
    r
}

pub fn batch_extract_icons(items: &[(String, PathBuf)]) {
    if items.is_empty() {
        return;
    }
    let jobs: String = items
        .iter()
        .map(|(id, path)| {
            let sid = id.replace('\'', "''");
            let spath = path.to_string_lossy().replace('\\', "\\\\");
            format!("@{{Id='{}';Out='{}'}}", sid, spath)
        })
        .collect::<Vec<_>>()
        .join(",");

    let ps = format!(
        r#"
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System; using System.Drawing; using System.Runtime.InteropServices;
public class IE {{
    [DllImport("shell32.dll",CharSet=CharSet.Unicode)]
    public static extern uint SHGetFileInfo(string p,uint fa,ref SHFI fi,uint cb,uint fl);
    [StructLayout(LayoutKind.Sequential,CharSet=CharSet.Unicode)]
    public struct SHFI {{
        public IntPtr hIcon; public int iIcon; public uint dwAttr;
        [MarshalAs(UnmanagedType.ByValTStr,SizeConst=260)] public string szDisplayName;
        [MarshalAs(UnmanagedType.ByValTStr,SizeConst=80)]  public string szTypeName;
    }}
    [DllImport("user32.dll")] public static extern bool DestroyIcon(IntPtr h);
}}
"@
function Save-Icon($icon,$out) {{
    $b=$icon.ToBitmap(); $r=New-Object System.Drawing.Bitmap(64,64)
    $g=[System.Drawing.Graphics]::FromImage($r)
    $g.InterpolationMode=[System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.DrawImage($b,0,0,64,64); $g.Dispose()
    $r.Save($out,[System.Drawing.Imaging.ImageFormat]::Png); $b.Dispose(); $r.Dispose()
}}
$shell=$shell=New-Object -ComObject Shell.Application
$all=$shell.Namespace('shell:AppsFolder').Items()
$jobs=@({jobs})
foreach($job in $jobs) {{
    $id=$job.Id; $out=$job.Out
    if(Test-Path $out){{continue}}
    $ok=$false
    try {{
        $fi=New-Object IE+SHFI
        [IE]::SHGetFileInfo("shell:AppsFolder\$id",0,[ref]$fi,[System.Runtime.InteropServices.Marshal]::SizeOf($fi),0x100)|Out-Null
        if($fi.hIcon -ne [IntPtr]::Zero){{$icon=[System.Drawing.Icon]::FromHandle($fi.hIcon);Save-Icon $icon $out;[IE]::DestroyIcon($fi.hIcon);$ok=$true}}
    }} catch {{}}
    if(-not $ok){{
        try {{
            $item=$all|Where-Object{{$_.Path -eq $id}}|Select-Object -First 1
            if($item -and (Test-Path $item.Path)){{$icon=[System.Drawing.Icon]::ExtractAssociatedIcon($item.Path);if($icon){{Save-Icon $icon $out;$ok=$true}}}}
        }} catch {{}}
    }}
    if(-not $ok){{
        try {{
            $pkg=Get-AppxPackage|Where-Object{{$_.PackageFamilyName -eq $id.Split('!')[0]}}|Select-Object -First 1
            if($pkg){{
                [xml]$mf=Get-Content(Join-Path $pkg.InstallLocation 'AppxManifest.xml') -Raw
                $rel=$mf.Package.Properties.Logo
                if(-not $rel){{$rel=$mf.Package.Applications.Application.VisualElements.Square44x44Logo}}
                if($rel){{
                    $base=Join-Path $pkg.InstallLocation $rel
                    @('.scale-200.png','.scale-150.png','.scale-100.png','')|ForEach-Object{{
                        $c=$base -replace '\.png$',$_
                        if(-not $ok -and (Test-Path $c)){{
                            $src=[System.Drawing.Image]::FromFile($c)
                            $bmp=New-Object System.Drawing.Bitmap(64,64)
                            $g=[System.Drawing.Graphics]::FromImage($bmp)
                            $g.InterpolationMode=[System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
                            $g.DrawImage($src,0,0,64,64);$g.Dispose();$src.Dispose()
                            $bmp.Save($out,[System.Drawing.Imaging.ImageFormat]::Png);$bmp.Dispose();$ok=$true
                        }}
                    }}
                }}
            }}
        }} catch {{}}
    }}
    Write-Output "$id=$ok"
}}
"#,
        jobs = jobs
    );

    let out = no_window!(Command::new("powershell").args([
        "-NoProfile",
        "-NonInteractive",
        "-WindowStyle",
        "Hidden",
        "-Command",
        &ps
    ]))
    .output();

    if let Ok(o) = out {
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            if line.contains("=True") {
                let id = line.split('=').next().unwrap_or("").trim();
                if let Some((_, path)) = items.iter().find(|(i, _)| i == id) {
                    if let Ok(bytes) = fs::read(path) {
                        if !bytes.is_empty() {
                            let _ = fs::write(path, encrypt_bytes(&bytes));
                        }
                    }
                }
            }
        }
    }
}

pub fn fetch_app_list() -> Vec<(String, String)> {
    let out = no_window!(Command::new("powershell").args(["-Command", r#"
        Get-StartApps |
        Where-Object {
            $_.Name -notmatch 'Windows (Security|Defender|Update|Terminal|Subsystem|Accessories|Administrative|Backup|Ease|Media|Mobility|Narrator|Recovery|Remote|Speech|System|Tools|Utility|Photo)' -and
            $_.Name -notmatch '^(Microsoft (Store|Edge|Teams|OneDrive|Outlook|To Do|News|Weather|Maps|Bing|Xbox Game Bar|Clipchamp|Get Help|Mixed Reality|Phone Link|Quick Assist|Sticky Notes|Tips|Whiteboard|Family Safety))$' -and
            $_.Name -notmatch 'Runtime|Security Center|Shell Experience' -and
            $_.AppID -notmatch 'windows\.'
        } | Sort-Object Name | ForEach-Object { "$($_.Name)|$($_.AppID)" }
    "#])).output().unwrap_or_else(|_| std::process::Output {
        status: std::process::ExitStatus::default(),
        stdout: vec![], stderr: vec![],
    });

    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| {
            let mut p = line.split('|');
            let name = p.next()?.trim().to_string();
            let app_id = p.next()?.trim().to_string();
            if name.is_empty() || app_id.is_empty() {
                return None;
            }
            Some((name, app_id))
        })
        .collect()
}

pub fn spawn_icon_fetch(
    missing: Vec<(String, PathBuf)>,
    data_arc: Arc<std::sync::Mutex<crate::state::CategoryData>>,
    app_handle: AppHandle,
) {
    thread::spawn(move || {
        batch_extract_icons(&missing);
        for (app_id, path) in &missing {
            if let Some(b64) = icon_path_to_b64(path) {
                {
                    let mut lock = data_arc.lock().unwrap();
                    if let Some(app) = lock.apps.iter_mut().find(|a| a.app_id == *app_id) {
                        app.icon_base64 = Some(b64.clone());
                    }
                }
                let _ = app_handle.emit(
                    "icon-ready",
                    serde_json::json!({
                        "app_id": app_id, "icon_base64": b64,
                    }),
                );
            }
        }
    });
}

pub fn build_app_list(
    raw: Vec<(String, String)>,
    existing: &[AppData],
) -> (Vec<AppData>, Vec<(String, PathBuf)>) {
    let mut apps = vec![];
    let mut needs = vec![];

    for (name, app_id) in raw {
        let p = icon_path(&app_id);
        let cached_icon = if p.exists() {
            icon_path_to_b64(&p)
        } else {
            None
        };

        let category = existing
            .iter()
            .find(|a| a.app_id == app_id)
            .map(|a| a.category.clone())
            .unwrap_or_else(|| "Uncategorized".into());

        if cached_icon.is_none() {
            needs.push((app_id.clone(), p));
        }
        apps.push(AppData {
            name,
            app_id,
            category,
            icon_base64: cached_icon,
        });
    }
    (apps, needs)
}
