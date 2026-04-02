use anyhow::{Context, Result};
use axum::{
    extract::Multipart,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::io::Write;
use tokio::net::TcpListener;
use tracing::{error, info};
use uuid::Uuid;

// ── unified response type ────────────────────────────────────────────────────

type Resp = (StatusCode, Json<Value>);

fn ok() -> Resp {
    (StatusCode::OK, Json(json!({ "status": "ok" })))
}

fn bad(msg: impl ToString) -> Resp {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": msg.to_string() })),
    )
}

fn internal(msg: impl ToString) -> Resp {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": msg.to_string() })),
    )
}

// ── entry point ──────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    // Port: first CLI arg  →  $PORT env var  →  default 3000
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .or_else(|| std::env::var("PORT").ok().and_then(|s| s.parse().ok()))
        .unwrap_or(3000);

    let app = Router::new()
        .route("/health", get(health))
        .route("/print", post(handle_print));

    let addr = format!("127.0.0.1:{port}");
    let listener = TcpListener::bind(&addr)
        .await
        .with_context(|| format!("cannot bind to {addr}"))?;

    info!("print-util listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

// ── handlers ─────────────────────────────────────────────────────────────────

async fn health() -> Resp {
    ok()
}

/// POST /print
///
/// Multipart fields:
///   file    – PDF binary (required)
///   printer – printer name (optional, defaults to system default)
async fn handle_print(mut multipart: Multipart) -> Resp {
    let mut pdf_bytes: Option<Vec<u8>> = None;
    let mut printer: Option<String> = None;

    loop {
        match multipart.next_field().await {
            Ok(Some(field)) => match field.name() {
                Some("file") => match field.bytes().await {
                    Ok(b) => pdf_bytes = Some(b.to_vec()),
                    Err(e) => return bad(format!("read field: {e}")),
                },
                Some("printer") => {
                    if let Ok(name) = field.text().await {
                        if !name.trim().is_empty() {
                            printer = Some(name.trim().to_owned());
                        }
                    }
                }
                _ => {}
            },
            Ok(None) => break,
            Err(e) => return bad(format!("multipart error: {e}")),
        }
    }

    let data = match pdf_bytes {
        Some(d) if !d.is_empty() => d,
        _ => return bad("missing required 'file' field"),
    };

    match tokio::task::spawn_blocking(move || silent_print(&data, printer.as_deref())).await {
        Ok(Ok(())) => ok(),
        Ok(Err(e)) => {
            error!("{e:#}");
            internal(e)
        }
        Err(e) => internal(format!("task panic: {e}")),
    }
}

// ── printing logic ───────────────────────────────────────────────────────────

fn silent_print(data: &[u8], printer: Option<&str>) -> Result<()> {
    // Write to a named temp file so the spooler can read it after we return.
    // We intentionally keep the file alive for 60 s via a cleanup thread.
    let tmp_dir = std::env::temp_dir().join("print-util");
    std::fs::create_dir_all(&tmp_dir).context("create temp dir")?;

    let path = tmp_dir.join(format!("{}.pdf", Uuid::new_v4()));

    {
        let mut f = std::fs::File::create(&path).context("create temp file")?;
        f.write_all(data).context("write PDF data")?;
        f.flush().context("flush temp file")?;
    }

    // Spawn cleanup thread – 60 s is plenty for the spooler to read the file.
    let cleanup = path.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(60));
        let _ = std::fs::remove_file(cleanup);
    });

    let path_str = path.to_str().context("non-UTF-8 temp path")?;

    #[cfg(windows)]
    windows_print(path_str, printer)?;

    #[cfg(not(windows))]
    unix_print(path_str, printer)?;

    Ok(())
}

// ── Windows silent print ─────────────────────────────────────────────────────
//
// Priority:
//   1. SumatraPDF        – fully silent, no window ever
//   2. Ghostscript DLL   – gsdll64.dll loaded in-process, zero subprocess
//   3. Ghostscript CLI   – subprocess, installed GS on PATH / ProgramFiles
//   4. Adobe Acrobat / Reader – hidden with /h flag, may flash briefly
//   5. ShellExecuteW fallback  – works but Chrome/Edge can prompt a dialog

#[cfg(windows)]
fn windows_print(path: &str, printer: Option<&str>) -> Result<()> {
    if try_sumatrapdf(path, printer)? {
        return Ok(());
    }
    if try_ghostscript_dll(path, printer)? {
        return Ok(());
    }
    if try_ghostscript(path, printer)? {
        return Ok(());
    }
    if try_acrobat(path, printer)? {
        return Ok(());
    }
    shell_execute_print(path, printer)
}

// ── 1. SumatraPDF ────────────────────────────────────────────────────────────
//
// SumatraPDF.exe [-print-to "Printer"] -print-to-default -silent "file.pdf"

#[cfg(windows)]
fn sumatra_candidates() -> Vec<std::path::PathBuf> {
    let mut paths: Vec<std::path::PathBuf> = vec![
        // Anywhere on PATH
        std::path::PathBuf::from("SumatraPDF.exe"),
    ];
    // Per-machine install
    if let Ok(pf) = std::env::var("ProgramFiles") {
        paths.push(format!(r"{pf}\SumatraPDF\SumatraPDF.exe").into());
    }
    if let Ok(pf) = std::env::var("ProgramFiles(x86)") {
        paths.push(format!(r"{pf}\SumatraPDF\SumatraPDF.exe").into());
    }
    // Per-user install (common in newer versions)
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        paths.push(format!(r"{local}\SumatraPDF\SumatraPDF.exe").into());
        paths.push(format!(r"{local}\Programs\SumatraPDF\SumatraPDF.exe").into());
    }
    paths
}

#[cfg(windows)]
fn try_sumatrapdf(path: &str, printer: Option<&str>) -> Result<bool> {
    let exe = match sumatra_candidates()
        .into_iter()
        .find(|p| is_executable(p))
    {
        Some(e) => e,
        None => return Ok(false),
    };

    let mut cmd = std::process::Command::new(&exe);
    match printer {
        Some(p) => { cmd.arg("-print-to").arg(p); }
        None     => { cmd.arg("-print-to-default"); }
    }
    cmd.arg("-silent").arg(path);

    // CREATE_NO_WINDOW so the process is completely invisible
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }

    let status = cmd.status().with_context(|| format!("launch {}", exe.display()))?;
    anyhow::ensure!(status.success(), "SumatraPDF exited with {status}");
    info!("print job submitted via SumatraPDF for '{path}'");
    Ok(true)
}

// ── 2. Ghostscript DLL (in-process) ─────────────────────────────────────────
//
// Loads gsdll64.dll directly into the process – no subprocess, no window.
// DLL search order:
//   a) <exe dir>/gsdll64.dll          ← bundled alongside the binary
//   b) <exe dir>/vendor/gs/gsdll64.dll
//   c) %ProgramFiles%/gs/<ver>/bin/gsdll64.dll
//   d) %ProgramFiles(x86)%/gs/…
//
// Ghostscript API used:
//   gsapi_new_instance / gsapi_init_with_args / gsapi_exit / gsapi_delete_instance

#[cfg(windows)]
fn find_gsdll() -> Option<std::path::PathBuf> {
    // a/b: next to or under vendor/ relative to the running exe
    if let Ok(exe) = std::env::current_exe() {
        let dir = exe.parent().unwrap_or(std::path::Path::new("."));
        let candidates = [
            dir.join("gsdll64.dll"),
            dir.join("vendor").join("gs").join("gsdll64.dll"),
        ];
        for c in &candidates {
            if c.is_file() {
                return Some(c.clone());
            }
        }
    }
    // c/d: standard install locations
    for pf in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Ok(root) = std::env::var(pf) {
            if let Ok(entries) = std::fs::read_dir(format!(r"{root}\gs")) {
                for entry in entries.flatten() {
                    let dll = entry.path().join("bin").join("gsdll64.dll");
                    if dll.is_file() {
                        return Some(dll);
                    }
                }
            }
        }
    }
    None
}

#[cfg(windows)]
fn try_ghostscript_dll(path: &str, printer: Option<&str>) -> Result<bool> {
    use libloading::{Library, Symbol};
    use std::ffi::CString;

    let dll_path = match find_gsdll() {
        Some(p) => p,
        None => return Ok(false),
    };

    // SAFETY: we hold `lib` alive until after gsapi_delete_instance.
    let lib = unsafe { Library::new(&dll_path) }
        .with_context(|| format!("load {}", dll_path.display()))?;

    type GsNewInstance =
        unsafe extern "C" fn(*mut *mut std::ffi::c_void, *mut std::ffi::c_void) -> i32;
    type GsInitWithArgs =
        unsafe extern "C" fn(*mut std::ffi::c_void, i32, *mut *mut i8) -> i32;
    type GsExit = unsafe extern "C" fn(*mut std::ffi::c_void) -> i32;
    type GsDeleteInstance = unsafe extern "C" fn(*mut std::ffi::c_void);

    let gs_new: Symbol<GsNewInstance> =
        unsafe { lib.get(b"gsapi_new_instance\0") }.context("gsapi_new_instance")?;
    let gs_init: Symbol<GsInitWithArgs> =
        unsafe { lib.get(b"gsapi_init_with_args\0") }.context("gsapi_init_with_args")?;
    let gs_exit: Symbol<GsExit> =
        unsafe { lib.get(b"gsapi_exit\0") }.context("gsapi_exit")?;
    let gs_delete: Symbol<GsDeleteInstance> =
        unsafe { lib.get(b"gsapi_delete_instance\0") }.context("gsapi_delete_instance")?;

    // Build argv
    let mut args_c: Vec<CString> = vec![
        CString::new("gs")?,
        CString::new("-dBATCH")?,
        CString::new("-dNOPAUSE")?,
        CString::new("-dPrinted")?,
        CString::new("-dNOSAFER")?,
        CString::new("-q")?,
        CString::new("-sDEVICE=mswinpr2")?,
    ];
    if let Some(p) = printer {
        args_c.push(CString::new(format!("-sOutputFile=%printer%{p}"))?);
    }
    args_c.push(CString::new(path)?);

    let mut argv: Vec<*mut i8> = args_c
        .iter()
        .map(|s| s.as_ptr() as *mut i8)
        .collect();

    let mut instance: *mut std::ffi::c_void = std::ptr::null_mut();

    let rc = unsafe { gs_new(&mut instance, std::ptr::null_mut()) };
    anyhow::ensure!(rc == 0, "gsapi_new_instance failed: {rc}");

    let rc = unsafe { gs_init(instance, argv.len() as i32, argv.as_mut_ptr()) };
    let _ = unsafe { gs_exit(instance) };
    unsafe { gs_delete(instance) };

    // GS returns -101 (e_Quit) as a normal "finished" code; treat as success.
    anyhow::ensure!(
        rc == 0 || rc == -101,
        "gsapi_init_with_args failed: {rc}"
    );
    info!("print job submitted via Ghostscript DLL for '{path}'");
    Ok(true)
}

// ── 3. Ghostscript CLI ────────────────────────────────────────────────────────
//
// gswin64c.exe / gswin32c.exe  (console build – zero GUI)
// Default printer:  -sDEVICE=mswinpr2  (picks Windows default)
// Specific printer: -sOutputFile="%printer%Name"
//
// Key flags:
//   -dBATCH   – exit after last page (no interactive mode)
//   -dNOPAUSE – don't wait between pages
//   -dPrinted – mark PDF as printed (updates LastPrinted field)
//   -dNOSAFER – needed on some GS builds to access the printer device
//   -q        – quiet (suppress banner)

#[cfg(windows)]
fn ghostscript_candidates() -> Vec<std::path::PathBuf> {
    let mut paths: Vec<std::path::PathBuf> = vec![
        std::path::PathBuf::from("gswin64c.exe"),
        std::path::PathBuf::from("gswin32c.exe"),
        std::path::PathBuf::from("gs.exe"),
    ];
    for pf in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Ok(root) = std::env::var(pf) {
            // Ghostscript installs under  %PF%\gs\gsX.XX\bin\
            if let Ok(entries) = std::fs::read_dir(format!(r"{root}\gs")) {
                for entry in entries.flatten() {
                    let bin = entry.path().join("bin");
                    paths.push(bin.join("gswin64c.exe"));
                    paths.push(bin.join("gswin32c.exe"));
                }
            }
        }
    }
    paths
}

#[cfg(windows)]
fn try_ghostscript(path: &str, printer: Option<&str>) -> Result<bool> {
    let exe = match ghostscript_candidates()
        .into_iter()
        .find(|p| is_executable(p))
    {
        Some(e) => e,
        None => return Ok(false),
    };

    let mut cmd = std::process::Command::new(&exe);
    cmd.args(["-dBATCH", "-dNOPAUSE", "-dPrinted", "-dNOSAFER", "-q"]);
    cmd.arg("-sDEVICE=mswinpr2");

    match printer {
        Some(p) => { cmd.arg(format!("-sOutputFile=%printer%{p}")); }
        None => {
            // mswinpr2 without -sOutputFile uses the Windows default printer
        }
    }

    cmd.arg(path);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }

    let output = cmd.output().with_context(|| format!("launch {}", exe.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Ghostscript exited with {}: {}", output.status, stderr.trim());
    }
    info!("print job submitted via Ghostscript for '{path}'");
    Ok(true)
}

// ── 4. Adobe Acrobat / Reader ─────────────────────────────────────────────────
//
// AcroRd32.exe /h /t "file.pdf" ["printer"]
// Acrobat.exe  /h /t "file.pdf" ["printer"]

#[cfg(windows)]
fn acrobat_candidates() -> Vec<std::path::PathBuf> {
    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    for pf in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Ok(root) = std::env::var(pf) {
            paths.push(format!(r"{root}\Adobe\Acrobat DC\Acrobat\Acrobat.exe").into());
            paths.push(
                format!(r"{root}\Adobe\Acrobat Reader DC\Reader\AcroRd32.exe").into(),
            );
            paths.push(format!(r"{root}\Adobe\Reader 11.0\Reader\AcroRd32.exe").into());
        }
    }
    paths
}

#[cfg(windows)]
fn try_acrobat(path: &str, printer: Option<&str>) -> Result<bool> {
    let exe = match acrobat_candidates()
        .into_iter()
        .find(|p| is_executable(p))
    {
        Some(e) => e,
        None => return Ok(false),
    };

    let mut cmd = std::process::Command::new(&exe);
    // /h = hidden, /t = print-to
    cmd.arg("/h").arg("/t").arg(path);
    if let Some(p) = printer {
        cmd.arg(p);
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }

    let status = cmd.status().with_context(|| format!("launch {}", exe.display()))?;
    anyhow::ensure!(status.success(), "Acrobat exited with {status}");
    info!("print job submitted via Acrobat for '{path}'");
    Ok(true)
}

// ── 5. ShellExecuteW fallback ────────────────────────────────────────────────

#[cfg(windows)]
fn shell_execute_print(path: &str, printer: Option<&str>) -> Result<()> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
    use windows::core::PCWSTR;

    tracing::warn!(
        "SumatraPDF and Acrobat not found – falling back to ShellExecuteW. \
         The system default PDF handler (Chrome/Edge) may show a dialog."
    );

    fn wide(s: &str) -> Vec<u16> {
        OsStr::new(s).encode_wide().chain(Some(0u16)).collect()
    }

    let file_w = wide(path);
    let (verb, params_buf): (&str, Option<Vec<u16>>) = match printer {
        Some(p) => ("printto", Some(wide(&format!("\"{p}\"")))),
        None => ("print", None),
    };
    let verb_w = wide(verb);

    let ret = unsafe {
        ShellExecuteW(
            HWND(std::ptr::null_mut()),
            PCWSTR(verb_w.as_ptr()),
            PCWSTR(file_w.as_ptr()),
            params_buf
                .as_ref()
                .map_or(PCWSTR::null(), |p| PCWSTR(p.as_ptr())),
            PCWSTR::null(),
            SW_HIDE,
        )
    };

    let code = ret.0 as usize;
    anyhow::ensure!(
        code > 32,
        "ShellExecuteW failed (code={code}): {}",
        shell_error_str(code)
    );
    info!("print job submitted via ShellExecuteW for '{path}'");
    Ok(())
}

#[cfg(windows)]
fn shell_error_str(code: usize) -> &'static str {
    match code {
        0 => "out of memory or resources",
        2 => "file not found",
        3 => "path not found",
        5 => "access denied",
        8 => "out of memory",
        32 => "no application associated with this file type",
        _ => "unknown error",
    }
}

// ── helper ───────────────────────────────────────────────────────────────────

#[cfg(windows)]
fn is_executable(p: &std::path::Path) -> bool {
    // For bare filenames (no dir component) rely on PATH; treat as present.
    if p.components().count() == 1 {
        return which_in_path(p);
    }
    p.is_file()
}

#[cfg(windows)]
fn which_in_path(exe: &std::path::Path) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| dir.join(exe).is_file())
        })
        .unwrap_or(false)
}

// ── Unix fallback: lp(1) ─────────────────────────────────────────────────────

#[cfg(not(windows))]
fn unix_print(path: &str, printer: Option<&str>) -> Result<()> {
    let mut cmd = std::process::Command::new("lp");
    if let Some(p) = printer {
        cmd.arg("-d").arg(p);
    }
    let status = cmd.arg(path).status().context("spawn lp")?;
    anyhow::ensure!(status.success(), "lp exited with {status}");
    info!("print job submitted for '{path}'");
    Ok(())
}
