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

// ── paper size ──────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
enum PaperSize {
    A4,
    A5,
}

impl PaperSize {
    fn gs_name(self) -> &'static str {
        match self { PaperSize::A4 => "a4", PaperSize::A5 => "a5" }
    }
    fn sumatra_name(self) -> &'static str {
        match self { PaperSize::A4 => "A4",  PaperSize::A5 => "A5" }
    }
    fn lp_media(self) -> &'static str {
        match self { PaperSize::A4 => "a4",  PaperSize::A5 => "a5" }
    }
}

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
//
// Chạy theo 2 mode:
//   1. Windows Service  – khi được SCM khởi động (không có terminal)
//   2. Console          – khi chạy thẳng từ terminal (debug / portable)

fn resolve_port() -> u16 {
    std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .or_else(|| std::env::var("PORT").ok().and_then(|s| s.parse().ok()))
        .unwrap_or(17474)
}

fn main() -> Result<()> {
    #[cfg(windows)]
    {
        // Nếu process được SCM gọi, nó sẽ không có console → chạy service mode.
        // Nếu có console (chạy tay) thì chạy thẳng console mode.
        use windows_service::service_dispatcher;
        if !is_interactive() {
            service_dispatcher::start("print-util", ffi_service_main)
                .context("service_dispatcher::start")?;
            return Ok(());
        }
    }
    // Console mode
    run_console()
}

/// Kiểm tra process có đang chạy tương tác (có console) hay không.
#[cfg(windows)]
fn is_interactive() -> bool {
    use windows::Win32::System::Console::GetConsoleWindow;
    unsafe { !GetConsoleWindow().is_invalid() }
}

// ── Windows Service boilerplate ──────────────────────────────────────────────

#[cfg(windows)]
windows_service::define_windows_service!(ffi_service_main, service_main);

#[cfg(windows)]
fn service_main(_args: Vec<std::ffi::OsString>) {
    if let Err(e) = run_service() {
        eprintln!("service error: {e:#}");
    }
}

#[cfg(windows)]
fn run_service() -> Result<()> {
    use std::time::Duration;
    use windows_service::{
        service::ServiceControl,
        service_control_handler::{self, ServiceControlHandlerResult},
    };

    let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel::<()>();

    let event_handler = move |ctrl| match ctrl {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = shutdown_tx.send(());
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };

    let status_handle =
        service_control_handler::register("print-util", event_handler)
            .context("register service control handler")?;

    use windows_service::service::{
        ServiceState, ServiceStatus, ServiceType,
    };

    // Report: starting
    status_handle.set_service_status(ServiceStatus {
        service_type:             ServiceType::OWN_PROCESS,
        current_state:            ServiceState::StartPending,
        controls_accepted:        ServiceControlAccept::empty(),
        exit_code:                windows_service::service::ServiceExitCode::Win32(0),
        checkpoint:               0,
        wait_hint:                Duration::from_secs(5),
        process_id:               None,
    })?;

    // Start the async runtime in a background thread
    let port = resolve_port();
    let rt = tokio::runtime::Runtime::new().context("tokio runtime")?;
    let _guard = rt.enter();

    let server_handle = rt.spawn(async move {
        if let Err(e) = run_server(port).await {
            error!("server error: {e:#}");
        }
    });

    // Report: running
    use windows_service::service::ServiceControlAccept;
    status_handle.set_service_status(ServiceStatus {
        service_type:      ServiceType::OWN_PROCESS,
        current_state:     ServiceState::Running,
        controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        exit_code:         windows_service::service::ServiceExitCode::Win32(0),
        checkpoint:        0,
        wait_hint:         Duration::ZERO,
        process_id:        None,
    })?;

    info!("print-util service running on port {port}");

    // Block until SCM sends Stop/Shutdown
    let _ = shutdown_rx.recv();
    server_handle.abort();

    // Report: stopped
    status_handle.set_service_status(ServiceStatus {
        service_type:      ServiceType::OWN_PROCESS,
        current_state:     ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code:         windows_service::service::ServiceExitCode::Win32(0),
        checkpoint:        0,
        wait_hint:         Duration::ZERO,
        process_id:        None,
    })?;

    Ok(())
}

// ── console mode ─────────────────────────────────────────────────────────────

fn run_console() -> Result<()> {
    tracing_subscriber::fmt::init();
    let port = resolve_port();
    tokio::runtime::Runtime::new()
        .context("tokio runtime")?
        .block_on(run_server(port))
}

// ── shared server ────────────────────────────────────────────────────────────

async fn run_server(port: u16) -> Result<()> {
    let app = Router::new()
        .route("/health",    get(health))
        .route("/printers",  get(handle_printers))
        .route("/print",     post(handle_print_a4))   // default A4
        .route("/print/a4",  post(handle_print_a4))
        .route("/print/a5",  post(handle_print_a5));

    let addr = format!("127.0.0.1:{port}");
    let listener = TcpListener::bind(&addr)
        .await
        .with_context(|| format!("cannot bind to {addr}"))?;

    info!("print-util listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

/// GET /health
async fn health() -> Resp {
    ok()
}

/// GET /printers
/// Returns JSON: { "default": "HP LaserJet", "printers": ["HP LaserJet", "Microsoft Print to PDF", ...] }
async fn handle_printers() -> Resp {
    match tokio::task::spawn_blocking(list_printers).await {
        Ok(Ok(result)) => (StatusCode::OK, Json(result)),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(format!("task panic: {e}")),
    }
}

/// POST /print
///
/// Multipart fields:
///   file    – PDF binary (required)
///   printer – printer name (optional, defaults to system default)
///   name    – print job name shown in spooler (optional, auto-generated if absent)
async fn handle_print_a4(multipart: Multipart) -> Resp {
    handle_print_with_size(multipart, PaperSize::A4).await
}

async fn handle_print_a5(multipart: Multipart) -> Resp {
    handle_print_with_size(multipart, PaperSize::A5).await
}

async fn handle_print_with_size(mut multipart: Multipart, paper_size: PaperSize) -> Resp {
    let mut pdf_bytes: Option<Vec<u8>> = None;
    let mut printer: Option<String> = None;
    let mut job_name: Option<String> = None;

    loop {
        match multipart.next_field().await {
            Ok(Some(field)) => match field.name() {
                Some("file") => match field.bytes().await {
                    Ok(b) => pdf_bytes = Some(b.to_vec()),
                    Err(e) => return bad(format!("read field: {e}")),
                },
                Some("printer") => {
                    if let Ok(v) = field.text().await {
                        if !v.trim().is_empty() {
                            printer = Some(v.trim().to_owned());
                        }
                    }
                }
                Some("name") => {
                    if let Ok(v) = field.text().await {
                        if !v.trim().is_empty() {
                            job_name = Some(v.trim().to_owned());
                        }
                    }
                }
                _ => {}
            },
            Ok(None) => break,
            Err(e) => return bad(format!("multipart error: {e}")),
        }
    }

    // Auto-generate job name: doc-YYYYMMDD-HHMMSS
    let job_name = job_name.unwrap_or_else(|| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        format!("doc-{now}")
    });

    let data = match pdf_bytes {
        Some(d) if !d.is_empty() => d,
        _ => return bad("missing required 'file' field"),
    };

    match tokio::time::timeout(
        std::time::Duration::from_secs(120),
        tokio::task::spawn_blocking(move || {
            silent_print(&data, printer.as_deref(), paper_size, &job_name)
        }),
    )
    .await
    {
        Ok(Ok(Ok(()))) => ok(),
        Ok(Ok(Err(e))) => {
            error!("{e:#}");
            internal(e)
        }
        Ok(Err(e)) => internal(format!("task panic: {e}")),
        Err(_) => internal("print timed out after 120 s"),
    }
}

// ── printing logic ───────────────────────────────────────────────────────────

fn silent_print(data: &[u8], printer: Option<&str>, paper_size: PaperSize, job_name: &str) -> Result<()> {
    let tmp_dir = std::env::temp_dir().join("print-util");
    std::fs::create_dir_all(&tmp_dir).context("create temp dir")?;

    // Sanitize job name for use as filename: keep alphanumeric, space, dash, dot
    let safe_name: String = job_name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '.' || c == ' ' { c } else { '_' })
        .collect();
    let safe_name = safe_name.trim();
    let file_stem = if safe_name.is_empty() {
        Uuid::new_v4().to_string()
    } else {
        // Append short UUID to avoid collisions between concurrent jobs
        format!("{safe_name}-{}", &Uuid::new_v4().to_string()[..8])
    };

    let path = tmp_dir.join(format!("{file_stem}.pdf"));

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
    windows_print(path_str, printer, paper_size, job_name)?;

    #[cfg(not(windows))]
    unix_print(path_str, printer, paper_size, job_name)?;

    Ok(())
}

// ── Windows silent print ─────────────────────────────────────────────────────
//
// Priority:
//   1. SumatraPDF        – fully silent, no window ever
//   2. Ghostscript CLI   – subprocess with own message loop, zero dialog
//   3. Ghostscript DLL   – gsdll64.dll loaded in-process (fallback)
//   4. Adobe Acrobat / Reader – hidden with /h flag, may flash briefly
//   5. ShellExecuteW fallback  – works but Chrome/Edge can prompt a dialog

#[cfg(windows)]
fn windows_print(path: &str, printer: Option<&str>, paper_size: PaperSize, job_name: &str) -> Result<()> {
    if try_sumatrapdf(path, printer, paper_size)? {
        return Ok(());
    }
    if try_ghostscript(path, printer, paper_size, job_name)? {
        return Ok(());
    }
    if try_ghostscript_dll(path, printer, paper_size, job_name)? {
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
fn try_sumatrapdf(path: &str, printer: Option<&str>, paper_size: PaperSize) -> Result<bool> {
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
    cmd.arg("-print-settings")
       .arg(format!("paper={}", paper_size.sumatra_name()));
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
fn try_ghostscript_dll(path: &str, printer: Option<&str>, paper_size: PaperSize, job_name: &str) -> Result<bool> {
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
    // Always pass -sOutputFile=%printer%<name>. Without it, GS shows a
    // hidden printer-chooser dialog and hangs indefinitely.
    let target_printer = printer
        .map(str::to_owned)
        .or_else(get_default_printer)
        .context("no default printer found")?;

    let mut args_c: Vec<CString> = vec![
        CString::new("gs")?,
        CString::new("-dBATCH")?,
        CString::new("-dNOPAUSE")?,
        CString::new("-dNOSAFER")?,
        CString::new("-dNoCancel")?,
        CString::new("-dFIXEDMEDIA")?,
        CString::new(format!("-sPAPERSIZE={}", paper_size.gs_name()))?,
        CString::new("-q")?,
        CString::new("-sDEVICE=mswinpr2")?,
        CString::new(format!("-sDocumentName={job_name}"))?,
        CString::new(format!("-sOutputFile=%printer%{target_printer}"))?,
    ];
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
fn try_ghostscript(path: &str, printer: Option<&str>, paper_size: PaperSize, job_name: &str) -> Result<bool> {
    let exe = match ghostscript_candidates()
        .into_iter()
        .find(|p| is_executable(p))
    {
        Some(e) => e,
        None => return Ok(false),
    };

    let mut cmd = std::process::Command::new(&exe);
    cmd.args(["-dBATCH", "-dNOPAUSE", "-dNOSAFER", "-dNoCancel", "-dFIXEDMEDIA", "-q"]);
    cmd.arg(format!("-sPAPERSIZE={}", paper_size.gs_name()));
    cmd.arg("-sDEVICE=mswinpr2");
    cmd.arg(format!("-sDocumentName={job_name}"));

    // Always pass -sOutputFile=%printer%<name>. Without it, GS shows a
    // hidden printer-chooser dialog and hangs indefinitely.
    let target_printer = printer
        .map(str::to_owned)
        .or_else(get_default_printer)
        .context("no default printer found")?;
    cmd.arg(format!("-sOutputFile=%printer%{target_printer}"));

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
fn unix_print(path: &str, printer: Option<&str>, paper_size: PaperSize, job_name: &str) -> Result<()> {
    let mut cmd = std::process::Command::new("lp");
    if let Some(p) = printer {
        cmd.arg("-d").arg(p);
    }
    cmd.arg("-t").arg(job_name);
    cmd.arg("-o").arg(format!("media={}", paper_size.lp_media()));
    let status = cmd.arg(path).status().context("spawn lp")?;
    anyhow::ensure!(status.success(), "lp exited with {status}");
    info!("print job submitted for '{path}'");
    Ok(())
}

// ── list printers ─────────────────────────────────────────────────────────────

#[cfg(windows)]
fn list_printers() -> Result<Value> {
    use windows::Win32::Graphics::Printing::{
        EnumPrintersW, PRINTER_ENUM_LOCAL, PRINTER_ENUM_CONNECTIONS, PRINTER_INFO_4W,
    };
    use windows::core::PWSTR;

    let flags = PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS;
    let level = 4u32;

    // First call: get required buffer size
    let mut needed: u32 = 0;
    let mut count: u32 = 0;
    unsafe {
        let _ = EnumPrintersW(flags, PWSTR::null(), level, None, &mut needed, &mut count);
    }

    if needed == 0 {
        return Ok(json!({ "default": get_default_printer(), "printers": [] }));
    }

    let mut buf: Vec<u8> = vec![0u8; needed as usize];
    unsafe {
        EnumPrintersW(
            flags,
            PWSTR::null(),
            level,
            Some(&mut buf),
            &mut needed,
            &mut count,
        )
    }.ok().context("EnumPrintersW")?;

    let infos = unsafe {
        std::slice::from_raw_parts(buf.as_ptr() as *const PRINTER_INFO_4W, count as usize)
    };

    let names: Vec<String> = infos
        .iter()
        .map(|info| unsafe { info.pPrinterName.to_string().unwrap_or_default() })
        .collect();

    Ok(json!({
        "default": get_default_printer(),
        "printers": names,
    }))
}

#[cfg(windows)]
fn get_default_printer() -> Option<String> {
    use windows::Win32::Graphics::Printing::GetDefaultPrinterW;
    use windows::core::PWSTR;
    let mut size: u32 = 0;
    unsafe { let _ = GetDefaultPrinterW(PWSTR::null(), &mut size); }
    if size == 0 { return None; }
    let mut buf: Vec<u16> = vec![0u16; size as usize];
    let ok = unsafe { GetDefaultPrinterW(PWSTR(buf.as_mut_ptr()), &mut size) };
    if ok.as_bool() {
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..len]))
    } else {
        None
    }
}

#[cfg(not(windows))]
fn list_printers() -> Result<Value> {
    // Unix: parse `lpstat -a`
    let out = std::process::Command::new("lpstat").arg("-a").output();
    let names: Vec<String> = match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter_map(|l| l.split_whitespace().next().map(str::to_owned))
            .collect(),
        Err(_) => vec![],
    };
    Ok(json!({ "default": null, "printers": names }))
}
