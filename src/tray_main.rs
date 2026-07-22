#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() {
    eprintln!("print-util-tray is only supported on Windows");
}

#[cfg(windows)]
mod win_tray {
    use anyhow::{Context, Result};
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;
    use std::process::Command;
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows::Win32::System::Console::{FreeConsole, GetConsoleWindow};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Shell::{
        ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
        NIM_SETVERSION, NOTIFYICONDATAW, NOTIFYICON_VERSION_4,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
        DispatchMessageW, GetCursorPos, GetMessageW, LoadIconW, MessageBoxW, PostQuitMessage,
        RegisterClassW, SetForegroundWindow, TrackPopupMenu, TranslateMessage, HICON, HMENU,
        HWND_MESSAGE, IDC_ARROW, IDI_APPLICATION, IMAGE_ICON, LR_DEFAULTSIZE, LR_LOADFROMFILE,
        MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MF_SEPARATOR, MF_STRING, MSG, SW_SHOWNORMAL,
        TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RIGHTBUTTON, WM_APP, WM_COMMAND, WM_CONTEXTMENU,
        WM_DESTROY, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_RBUTTONUP, WNDCLASSW, WS_OVERLAPPED,
    };

    const APP_NAME: &str = "VNPT Print Util";
    const SUPPORT_URL: &str = "https://github.com/MYuitsu/print-util/issues";
    const WM_TRAYICON: u32 = WM_APP + 1;
    const TRAY_ICON_ID: u32 = 1;
    const MENU_SUPPORT: u16 = 1001;
    const MENU_CONFIG: u16 = 1002;
    const MENU_RESTART_SERVICE: u16 = 1003;
    const MENU_EXIT: u16 = 1099;
    const ERROR_INSUFFICIENT_BUFFER: i32 = 122;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentPackageFullName(
            package_full_name_length: *mut u32,
            package_full_name: *mut u16,
        ) -> i32;
    }

    pub fn run() -> Result<()> {
        detach_from_console();
        if is_msix_mode() {
            start_packaged_server()?;
        }

        let instance = unsafe { GetModuleHandleW(PCWSTR::null()) }.context("GetModuleHandleW")?;
        let hinstance = HINSTANCE(instance.0);
        let class_name = w!("PrintUtilTrayWindow");

        let cursor = unsafe {
            windows::Win32::UI::WindowsAndMessaging::LoadCursorW(HINSTANCE::default(), IDC_ARROW)
        }?;
        let wnd_class = WNDCLASSW {
            hCursor: cursor,
            hInstance: hinstance,
            lpszClassName: class_name,
            lpfnWndProc: Some(wnd_proc),
            ..Default::default()
        };

        let atom = unsafe { RegisterClassW(&wnd_class) };
        if atom == 0 {
            anyhow::bail!("RegisterClassW failed");
        }

        let hwnd = unsafe {
            CreateWindowExW(
                Default::default(),
                class_name,
                w!(""),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                HMENU::default(),
                hinstance,
                None,
            )
        }
        .context("CreateWindowExW")?;

        add_tray_icon(hwnd)?;

        let mut msg = MSG::default();
        while unsafe { GetMessageW(&mut msg, HWND::default(), 0, 0) }.as_bool() {
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        Ok(())
    }

    fn detach_from_console() {
        let has_console = unsafe { !GetConsoleWindow().is_invalid() };
        if has_console {
            let _ = unsafe { FreeConsole() };
        }
    }

    fn is_msix_mode() -> bool {
        if std::env::args().any(|arg| arg == "--msix") {
            return true;
        }

        let mut package_name_length = 0;
        let result =
            unsafe { GetCurrentPackageFullName(&mut package_name_length, std::ptr::null_mut()) };
        result == ERROR_INSUFFICIENT_BUFFER
    }

    fn start_packaged_server() -> Result<()> {
        let exe_dir = std::env::current_exe()?
            .parent()
            .ok_or_else(|| anyhow::anyhow!("cannot resolve package directory"))?
            .to_path_buf();
        Command::new(exe_dir.join("print-util.exe"))
            .arg("--console")
            .creation_flags(0x08000000)
            .spawn()
            .context("start packaged print server")?;
        Ok(())
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            WM_TRAYICON => {
                let event = loword(lparam.0 as usize) as u32;
                if event == WM_RBUTTONUP || event == WM_CONTEXTMENU || event == WM_LBUTTONUP {
                    show_tray_menu(hwnd);
                } else if event == WM_LBUTTONDBLCLK {
                    let _ = open_config_file();
                }
                LRESULT(0)
            }
            WM_COMMAND => {
                let cmd = loword(wparam.0);
                match cmd {
                    MENU_SUPPORT => {
                        let _ = open_url(SUPPORT_URL);
                    }
                    MENU_CONFIG => {
                        let _ = open_config_file();
                    }
                    MENU_RESTART_SERVICE => {
                        let _ = restart_service_with_feedback();
                    }
                    MENU_EXIT => {
                        let _ = DestroyWindow(hwnd);
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                remove_tray_icon(hwnd);
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }

    fn add_tray_icon(hwnd: HWND) -> Result<()> {
        let icon = load_tray_icon();
        let mut nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: TRAY_ICON_ID,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: WM_TRAYICON,
            hIcon: icon,
            ..Default::default()
        };
        fill_wide_buffer(APP_NAME, &mut nid.szTip);

        let added = unsafe { Shell_NotifyIconW(NIM_ADD, &mut nid) }.as_bool();
        if !added {
            anyhow::bail!("Shell_NotifyIconW NIM_ADD failed");
        }

        nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        let _ = unsafe { Shell_NotifyIconW(NIM_SETVERSION, &mut nid) };
        Ok(())
    }

    fn remove_tray_icon(hwnd: HWND) {
        let mut nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: TRAY_ICON_ID,
            ..Default::default()
        };
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &mut nid);
        }
    }

    fn load_tray_icon() -> HICON {
        if let Some(icon_path) = vnpt_icon_path() {
            let wide = to_wide(icon_path.as_os_str());
            let handle = unsafe {
                windows::Win32::UI::WindowsAndMessaging::LoadImageW(
                    HINSTANCE::default(),
                    PCWSTR(wide.as_ptr()),
                    IMAGE_ICON,
                    0,
                    0,
                    LR_LOADFROMFILE | LR_DEFAULTSIZE,
                )
            };
            if let Ok(handle) = handle {
                if !handle.is_invalid() {
                    return HICON(handle.0);
                }
            }
        }

        unsafe { LoadIconW(HINSTANCE::default(), IDI_APPLICATION) }.unwrap_or_default()
    }

    fn vnpt_icon_path() -> Option<PathBuf> {
        if let Some(explicit) = std::env::var_os("PRINT_UTIL_TRAY_ICON").map(PathBuf::from) {
            if explicit.exists() {
                return Some(explicit);
            }
        }

        let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let candidates = [
            exe_dir.join("vnpt.ico"),
            exe_dir.join("assets").join("vnpt.ico"),
            cwd.join("vnpt.ico"),
            cwd.join("installer").join("vendor").join("vnpt.ico"),
        ];
        candidates.into_iter().find(|p| p.exists())
    }

    fn show_tray_menu(hwnd: HWND) {
        let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
            return;
        };

        let support = to_wide(OsStr::new("Hỗ trợ"));
        let config = to_wide(OsStr::new("Cấu hình"));
        let restart_service = to_wide(OsStr::new("Khởi động lại dịch vụ"));
        let exit_label = to_wide(OsStr::new("Thoát"));

        unsafe {
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                MENU_SUPPORT as usize,
                PCWSTR(support.as_ptr()),
            );
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                MENU_CONFIG as usize,
                PCWSTR(config.as_ptr()),
            );
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                MENU_RESTART_SERVICE as usize,
                PCWSTR(restart_service.as_ptr()),
            );
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                MENU_EXIT as usize,
                PCWSTR(exit_label.as_ptr()),
            );

            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let _ = SetForegroundWindow(hwnd);
            let _ = TrackPopupMenu(
                menu,
                TPM_LEFTALIGN | TPM_BOTTOMALIGN | TPM_RIGHTBUTTON,
                pt.x,
                pt.y,
                0,
                hwnd,
                None,
            );
            let _ = DestroyMenu(menu);
        }
    }

    fn open_url(url: &str) -> Result<()> {
        open_shell_target(url)
    }

    fn open_config_file() -> Result<()> {
        let path = ensure_config_file()?;
        open_shell_target(
            path.to_str()
                .ok_or_else(|| anyhow::anyhow!("invalid config path"))?,
        )
    }

    fn open_shell_target(target: &str) -> Result<()> {
        let wide_target = to_wide(OsStr::new(target));
        let result = unsafe {
            ShellExecuteW(
                HWND::default(),
                w!("open"),
                PCWSTR(wide_target.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };

        let code = result.0 as usize;
        if code <= 32 {
            anyhow::bail!("ShellExecuteW failed with code {code}");
        }
        Ok(())
    }

    fn restart_service_with_feedback() -> Result<()> {
        if is_msix_mode() {
            start_packaged_server()?;
            show_info_message("Đã khởi động lại print-util.");
            return Ok(());
        }
        match restart_print_service() {
            Ok(_) => {
                show_info_message("Đã khởi động lại dịch vụ 'print-util' thành công.");
                Ok(())
            }
            Err(e) => {
                show_error_message(&format!(
                    "Khởi động lại dịch vụ thất bại:\n{e:#}\n\nHãy mở Terminal (Run as Administrator) và chạy:\nsc stop print-util\nsc start print-util"
                ));
                Err(e)
            }
        }
    }

    fn restart_print_service() -> Result<()> {
        let script_path = std::env::temp_dir().join("print-util-restart-service.ps1");
        let script = "\
$ErrorActionPreference = 'Stop'\n\
Stop-Service -Name 'print-util' -ErrorAction Stop\n\
Start-Sleep -Milliseconds 1200\n\
Start-Service -Name 'print-util' -ErrorAction Stop\n";
        std::fs::write(&script_path, script).context("write restart script")?;

        let script_q = ps_single_quote(
            script_path
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("invalid script path"))?,
        );
        let elevate_cmd = format!(
            "$ErrorActionPreference='Stop';\
$argList=@('-NoProfile','-ExecutionPolicy','Bypass','-File','{script_q}');\
$p=Start-Process -FilePath 'powershell.exe' -Verb RunAs -PassThru -Wait -ArgumentList $argList;\
exit $p.ExitCode"
        );

        let output = Command::new("powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                &elevate_cmd,
            ])
            .output()
            .context("start elevated restart")?;

        let _ = std::fs::remove_file(&script_path);

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let detail = format!("{}\n{}", stdout.trim(), stderr.trim())
                .trim()
                .to_string();
            if detail.is_empty() {
                anyhow::bail!("restart failed with status {}", output.status);
            }
            anyhow::bail!("restart failed: {detail}");
        }
        Ok(())
    }

    fn ps_single_quote(input: &str) -> String {
        input.replace('\'', "''")
    }

    fn ensure_config_file() -> Result<PathBuf> {
        let base_dir = if is_msix_mode() {
            std::env::var("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from(r"C:\Users\Public\AppData\Local"))
        } else {
            std::env::var("ProgramData")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from(r"C:\ProgramData"))
        };
        let config_dir = base_dir.join("print-util");
        std::fs::create_dir_all(&config_dir).context("create config dir")?;

        let config_path = config_dir.join("config.json");
        if !config_path.exists() {
            let default_cfg = serde_json::json!({
                "api_base": "http://127.0.0.1:17474",
                "support_url": SUPPORT_URL,
                "notes": "Edit values then save."
            });
            let bytes = serde_json::to_vec_pretty(&default_cfg)?;
            std::fs::write(&config_path, bytes).context("write config file")?;
        }
        Ok(config_path)
    }

    fn loword(v: usize) -> u16 {
        (v & 0xffff) as u16
    }

    fn show_info_message(msg: &str) {
        let title = to_wide(OsStr::new(APP_NAME));
        let text = to_wide(OsStr::new(msg));
        unsafe {
            let _ = MessageBoxW(
                HWND::default(),
                PCWSTR(text.as_ptr()),
                PCWSTR(title.as_ptr()),
                MB_OK | MB_ICONINFORMATION,
            );
        }
    }

    fn show_error_message(msg: &str) {
        let title = to_wide(OsStr::new(APP_NAME));
        let text = to_wide(OsStr::new(msg));
        unsafe {
            let _ = MessageBoxW(
                HWND::default(),
                PCWSTR(text.as_ptr()),
                PCWSTR(title.as_ptr()),
                MB_OK | MB_ICONERROR,
            );
        }
    }

    fn to_wide(s: &OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    fn fill_wide_buffer(input: &str, out: &mut [u16]) {
        let mut wide = to_wide(OsStr::new(input));
        if wide.len() > out.len() {
            wide.truncate(out.len());
            if let Some(last) = wide.last_mut() {
                *last = 0;
            }
        }
        out.fill(0);
        let len = wide.len().min(out.len());
        out[..len].copy_from_slice(&wide[..len]);
    }
}

#[cfg(windows)]
fn main() {
    if let Err(e) = win_tray::run() {
        eprintln!("print-util-tray error: {e:#}");
    }
}
