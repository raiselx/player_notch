//! Windows integration shared by the application shell.
use std::path::PathBuf;

#[cfg(windows)]
use windows::{
    Win32::{
        Foundation::{
            CloseHandle, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, GetLastError, HANDLE, HWND,
            LPARAM, LRESULT, POINT, WPARAM,
        },
        Storage::Packaging::Appx::GetApplicationUserModelId,
        System::LibraryLoader::GetModuleHandleW,
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoUninitialize,
            },
            Registry::{
                HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_SAM_FLAGS, REG_SZ,
                REG_VALUE_TYPE, RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
                RegSetValueExW,
            },
            Threading::{
                CreateMutexW, OpenProcess, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
                QueryFullProcessImageNameW,
            },
        },
        UI::{
            Shell::{
                IShellLinkW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_SETVERSION,
                NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
            },
            WindowsAndMessaging::{
                BringWindowToTop, CreateWindowExW, DestroyWindow, DispatchMessageW, EnumWindows,
                GetCursorPos, GetMessageW, GetSystemMetrics, GetWindowLongW, GetWindowTextLengthW,
                GetWindowThreadProcessId, IsIconic, IsWindowVisible, LR_DEFAULTSIZE, MSG,
                PostMessageW, RegisterClassW, SM_CXSCREEN, SM_CYSCREEN, SW_RESTORE,
                SetForegroundWindow, ShowWindow, TranslateMessage, WM_APP, WNDCLASSW,
                WS_OVERLAPPED,
            },
        },
    },
    core::{PCWSTR, w},
};

#[cfg(windows)]
static CONTEXT_MENU_OPEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub struct InstanceGuard {
    #[cfg(windows)]
    handle: HANDLE,
}

impl InstanceGuard {
    pub fn acquire() -> Result<Option<Self>, String> {
        #[cfg(windows)]
        unsafe {
            let h = CreateMutexW(None, false, w!("Local\\FloatingNotchPlayerSingleInstance"))
                .map_err(|e| format!("Create single instance mutex: {e}"))?;
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(h);
                return Ok(None);
            }
            return Ok(Some(Self { handle: h }));
        }
        #[cfg(not(windows))]
        {
            Ok(Some(Self {}))
        }
    }
}
impl Drop for InstanceGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

#[cfg(windows)]
fn run_key(access: REG_SAM_FLAGS) -> Result<HKEY, String> {
    use windows::core::PCWSTR;
    let mut key = HKEY::default();
    let path: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Run\0"
        .encode_utf16()
        .collect();
    unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(path.as_ptr()),
            0,
            access,
            &mut key,
        )
        .ok()
        .map_err(|e| e.to_string())?;
    }
    Ok(key)
}
pub fn autostart_enabled() -> bool {
    #[cfg(windows)]
    unsafe {
        let Ok(k) = run_key(KEY_READ) else {
            return false;
        };
        let name: Vec<u16> = "FloatingNotchPlayer\0".encode_utf16().collect();
        let mut ty = REG_VALUE_TYPE(0);
        let mut len = 0;
        let yes = RegQueryValueExW(
            k,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut ty),
            None,
            Some(&mut len),
        )
        .is_ok();
        let _ = RegCloseKey(k);
        return yes;
    }
    #[cfg(not(windows))]
    {
        false
    }
}
pub fn set_autostart(on: bool) -> Result<(), String> {
    #[cfg(windows)]
    unsafe {
        // Resolve the executable before opening the registry key so an error cannot leak it.
        let exe = if on {
            Some(std::env::current_exe().map_err(|e| e.to_string())?)
        } else {
            None
        };
        let k = run_key(KEY_SET_VALUE)?;
        let name: Vec<u16> = "FloatingNotchPlayer\0".encode_utf16().collect();
        let result = if on {
            use std::os::windows::ffi::OsStrExt;
            let mut wide: Vec<u16> = "\"".encode_utf16().collect();
            wide.extend(exe.as_ref().unwrap().as_os_str().encode_wide());
            wide.extend("\" --autostart\0".encode_utf16());
            RegSetValueExW(
                k,
                PCWSTR(name.as_ptr()),
                0,
                REG_SZ,
                Some(std::slice::from_raw_parts(
                    wide.as_ptr().cast(),
                    wide.len() * 2,
                )),
            )
            .ok()
            .map_err(|e| e.to_string())
        } else {
            let r = RegDeleteValueW(k, PCWSTR(name.as_ptr()));
            if r == ERROR_FILE_NOT_FOUND {
                Ok(())
            } else {
                r.ok().map_err(|e| e.to_string())
            }
        };
        let _ = RegCloseKey(k);
        result?;
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        let _ = on;
        Err("Autostart is only available on Windows".into())
    }
}

pub fn ensure_start_menu_shortcut() -> Result<(), String> {
    #[cfg(windows)]
    unsafe {
        use std::os::windows::ffi::OsStrExt;
        use windows::{Win32::System::Com::IPersistFile, core::Interface};
        let com_initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let result = (|| {
            let link: IShellLinkW = CoCreateInstance(
                &windows::Win32::UI::Shell::ShellLink,
                None,
                CLSCTX_INPROC_SERVER,
            )
            .map_err(|e| e.to_string())?;
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let path: Vec<u16> = exe.as_os_str().encode_wide().chain(Some(0)).collect();
            link.SetPath(PCWSTR(path.as_ptr()))
                .map_err(|e| e.to_string())?;
            let persist: IPersistFile = link.cast().map_err(|e| e.to_string())?;
            let programs = std::env::var_os("APPDATA")
                .map(PathBuf::from)
                .ok_or("APPDATA unavailable")?
                .join("Microsoft/Windows/Start Menu/Programs");
            std::fs::create_dir_all(&programs)
                .map_err(|e| format!("Create Start Menu folder: {e}"))?;
            let programs = programs.join("PlayerNotch.lnk");
            let dest: Vec<u16> = programs.as_os_str().encode_wide().chain(Some(0)).collect();
            persist
                .Save(PCWSTR(dest.as_ptr()), true)
                .map_err(|e| e.to_string())?;
            Ok(())
        })();
        if com_initialized {
            CoUninitialize();
        }
        return result;
    }
    #[cfg(not(windows))]
    {
        Err("Start menu shortcut is only available on Windows".into())
    }
}

pub fn open_source_app(app_id: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        let id = app_id.trim();
        if id.is_empty() {
            return Ok(());
        }
        if let Some(hwnd) = find_matching_app_window(id) {
            unsafe {
                if IsIconic(hwnd).as_bool() {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                }
                let _ = BringWindowToTop(hwnd);
                let _ = SetForegroundWindow(hwnd);
            }
            return Ok(());
        }
        if id.to_ascii_lowercase().ends_with(".exe") {
            return std::process::Command::new(id)
                .spawn()
                .map(|_| ())
                .map_err(|e| format!("Launch {id}: {e}"));
        }
        // AUMID activation handles packaged apps that have no regular executable launch path.
        let arg = format!("shell:AppsFolder\\{id}");
        std::process::Command::new("explorer.exe")
            .arg(arg)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("Activate app: {e}"))
    }
    #[cfg(not(windows))]
    {
        let _ = app_id;
        Err("App activation is only available on Windows".into())
    }
}

#[cfg(windows)]
fn find_matching_app_window(app_id: &str) -> Option<HWND> {
    struct Search {
        query: String,
        stem: String,
        exact_stem: String,
        hwnd: Option<HWND>,
    }
    unsafe extern "system" fn visit_child(
        child: HWND,
        data: LPARAM,
    ) -> windows::Win32::Foundation::BOOL {
        let search = unsafe { &mut *(data.0 as *mut Search) };
        let mut pid = 0u32;
        unsafe {
            GetWindowThreadProcessId(child, Some(&mut pid));
        }
        if pid == 0 {
            return true.into();
        }
        if let Ok(process) = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
            let mut buf = [0u16; 512];
            let mut len = buf.len() as u32;
            let result = unsafe {
                GetApplicationUserModelId(process, &mut len, windows::core::PWSTR(buf.as_mut_ptr()))
            };
            let aumid = if result.is_ok() {
                String::from_utf16_lossy(&buf[..len.min(buf.len() as u32) as usize])
                    .trim_end_matches('\0')
                    .to_owned()
            } else {
                String::new()
            };
            let _ = unsafe { CloseHandle(process) };
            if aumid.eq_ignore_ascii_case(&search.query) {
                search.hwnd = Some(child);
                return false.into();
            }
        }
        true.into()
    }
    unsafe extern "system" fn visit(hwnd: HWND, data: LPARAM) -> windows::Win32::Foundation::BOOL {
        use std::path::Path;
        let search = unsafe { &mut *(data.0 as *mut Search) };
        if !unsafe { IsWindowVisible(hwnd) }.as_bool() || unsafe { GetWindowTextLengthW(hwnd) } == 0
        {
            return true.into();
        }
        if unsafe { GetWindowLongW(hwnd, windows::Win32::UI::WindowsAndMessaging::GWL_EXSTYLE) }
            & 0x80
            != 0
        {
            return true.into();
        }
        let mut pid = 0u32;
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
        }
        if pid == 0 {
            return true.into();
        }
        if let Ok(process) = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
            let mut path_buf = [0u16; 1024];
            let mut n = path_buf.len() as u32;
            let path = unsafe {
                QueryFullProcessImageNameW(
                    process,
                    PROCESS_NAME_FORMAT(0),
                    windows::core::PWSTR(path_buf.as_mut_ptr()),
                    &mut n,
                )
            }
            .ok()
            .map(|_| String::from_utf16_lossy(&path_buf[..n as usize]));
            let mut buf = [0u16; 512];
            let mut len = buf.len() as u32;
            let result = unsafe {
                GetApplicationUserModelId(process, &mut len, windows::core::PWSTR(buf.as_mut_ptr()))
            };
            let aumid = if result.is_ok() {
                String::from_utf16_lossy(&buf[..len.min(buf.len() as u32) as usize])
                    .trim_end_matches('\0')
                    .to_owned()
            } else {
                String::new()
            };
            let _ = unsafe { CloseHandle(process) };
            if !search.query.is_empty() && aumid.eq_ignore_ascii_case(&search.query) {
                search.hwnd = Some(hwnd);
                return false.into();
            }
            if let Some(path) = path {
                let exe_stem = Path::new(&path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if exe_stem == "applicationframehost" {
                    // UWP apps expose their AUMID through the child process; activate the frame window.
                    let _ = unsafe {
                        windows::Win32::UI::WindowsAndMessaging::EnumChildWindows(
                            hwnd,
                            Some(visit_child),
                            LPARAM(search as *mut Search as isize),
                        )
                    };
                    if search.hwnd.is_some() {
                        search.hwnd = Some(hwnd);
                        return false.into();
                    }
                }
                if exe_stem == search.exact_stem
                    || (!search.stem.is_empty() && exe_stem == search.stem)
                {
                    search.hwnd = Some(hwnd);
                    return false.into();
                }
            }
        }
        true.into()
    }
    let low = app_id.to_ascii_lowercase();
    let exact_stem = if low.contains('!') {
        String::new()
    } else {
        std::path::Path::new(&low)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&low)
            .to_owned()
    };
    let mut stem = exact_stem.clone();
    for suffix in [
        "win", "desktop", "client", "player", "music", "app", "64", "32",
    ] {
        if let Some(s) = stem.strip_suffix(suffix) {
            stem = s.to_owned();
        }
    }
    let mut search = Search {
        query: low,
        stem,
        exact_stem,
        hwnd: None,
    };
    // SAFETY: the stack context lives until EnumWindows completes synchronously.
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM((&mut search as *mut Search) as isize));
    }
    search.hwnd
}

#[derive(Clone, Copy, Debug)]
pub struct DesktopInfo {
    pub origin: [f32; 2],
    pub size: [f32; 2],
    pub scale: f32,
}
pub fn desktop_info() -> DesktopInfo {
    #[cfg(windows)]
    unsafe {
        let width = GetSystemMetrics(SM_CXSCREEN) as f32;
        let height = GetSystemMetrics(SM_CYSCREEN) as f32;
        let dpi = windows::Win32::UI::HiDpi::GetDpiForSystem();
        let scale = dpi as f32 / 96.0;
        let scale = if scale.is_finite() && scale > 0.1 {
            scale
        } else {
            1.0
        };
        return DesktopInfo {
            origin: [0.0, 0.0],
            size: [width / scale, height / scale],
            scale,
        };
    }
    #[cfg(not(windows))]
    {
        DesktopInfo {
            origin: [0.0, 0.0],
            size: [0.0, 0.0],
            scale: 1.0,
        }
    }
}
pub fn cursor_pos(scale: f32) -> [f32; 2] {
    #[cfg(windows)]
    unsafe {
        let mut p = POINT::default();
        if GetCursorPos(&mut p).is_ok() {
            return [p.x as f32 / scale, p.y as f32 / scale];
        }
    }
    [0.0, 0.0]
}

/// Whether the foreground window belongs to the current supported media source.
pub fn foreground_is_media_source(app_id: &str) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    let id = app_id.to_ascii_lowercase();
    let expected = if id.contains("applemusic") {
        "applemusic.exe"
    } else if id.contains("spotify") {
        "spotify.exe"
    } else {
        return false;
    };
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return false;
        }
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return false;
        };
        let mut buffer = [0u16; 1024];
        let mut len = buffer.len() as u32;
        let result = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_FORMAT(0),
            windows::core::PWSTR(buffer.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(process);
        result.is_ok()
            && std::path::Path::new(&String::from_utf16_lossy(&buffer[..len as usize]))
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case(expected))
    }
}

pub fn primary_display_fullscreen() -> bool {
    use windows::Win32::{
        Foundation::RECT,
        Graphics::Gdi::ClientToScreen,
        UI::WindowsAndMessaging::{GetClientRect, GetForegroundWindow, GetShellWindow},
    };
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() || hwnd == GetShellWindow() || !IsWindowVisible(hwnd).as_bool() {
            return false;
        }
        let mut client = RECT::default();
        if GetClientRect(hwnd, &mut client).is_err() {
            return false;
        }
        // The client area excludes a maximized window's caption and border.
        let mut top_left = POINT {
            x: client.left,
            y: client.top,
        };
        let mut bottom_right = POINT {
            x: client.right,
            y: client.bottom,
        };
        ClientToScreen(hwnd, &mut top_left).as_bool()
            && ClientToScreen(hwnd, &mut bottom_right).as_bool()
            && top_left.x <= 0
            && top_left.y <= 0
            && bottom_right.x >= GetSystemMetrics(SM_CXSCREEN)
            && bottom_right.y >= GetSystemMetrics(SM_CYSCREEN)
    }
}

pub fn set_overlay_hidden(hwnd_value: isize, hidden: bool) -> bool {
    use windows::Win32::Graphics::Gdi::{CreateRectRgn, DeleteObject, HRGN, SetWindowRgn};
    let hwnd = HWND(hwnd_value as *mut std::ffi::c_void);
    unsafe {
        // An empty region removes drawing and hit testing while the window can still repaint.
        if !hidden {
            return SetWindowRgn(hwnd, HRGN::default(), true) != 0;
        }
        let region = CreateRectRgn(0, 0, 0, 0);
        if region.is_invalid() {
            return false;
        }
        if SetWindowRgn(hwnd, region, true) != 0 {
            return true; // Windows owns the region after a successful call.
        }
        let _ = DeleteObject(region);
        false
    }
}

static TRAY_ENGLISH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_language(language: crate::settings::Language) {
    TRAY_ENGLISH.store(
        language == crate::settings::Language::English,
        std::sync::atomic::Ordering::Release,
    );
}

pub fn context_menu_open() -> bool {
    #[cfg(windows)]
    {
        CONTEXT_MENU_OPEN.load(std::sync::atomic::Ordering::Acquire)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

pub fn clock_strings(language: crate::settings::Language) -> (String, String, std::time::Duration) {
    #[cfg(windows)]
    {
        use windows::Win32::System::SystemInformation::GetLocalTime;
        let t = unsafe { GetLocalTime() };
        let weekday = ["Вс", "Пн", "Вт", "Ср", "Чт", "Пт", "Сб"]
            .get(t.wDayOfWeek as usize)
            .unwrap_or(&"");
        let month = [
            "", "янв", "фев", "мар", "апр", "мая", "июн", "июл", "авг", "сен", "окт", "ноя", "дек",
        ]
        .get(t.wMonth as usize)
        .unwrap_or(&"");
        let date = if language == crate::settings::Language::English {
            let weekday = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]
                .get(t.wDayOfWeek as usize)
                .unwrap_or(&"");
            let month = [
                "", "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov",
                "Dec",
            ]
            .get(t.wMonth as usize)
            .unwrap_or(&"");
            format!("{weekday}, {month} {}", t.wDay)
        } else {
            format!("{weekday}, {} {month}", t.wDay)
        };
        let elapsed = u64::from(t.wSecond) * 1000 + u64::from(t.wMilliseconds);
        let remaining = std::time::Duration::from_millis(60_000_u64.saturating_sub(elapsed));
        return (format!("{:02}:{:02}", t.wHour, t.wMinute), date, remaining);
    }
    #[cfg(not(windows))]
    {
        (
            String::new(),
            String::new(),
            std::time::Duration::from_secs(60),
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayEvent {
    Settings,
    Quit,
}
pub struct Tray {
    rx: std::sync::mpsc::Receiver<TrayEvent>,
}
impl Tray {
    pub fn new(ctx: eframe::egui::Context) -> Result<Self, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        #[cfg(windows)]
        {
            start_tray_thread(tx, ctx)?;
        }
        #[cfg(not(windows))]
        {
            let _ = tx;
            return Err("System tray is only available on Windows".into());
        }
        Ok(Self { rx })
    }
    pub fn events(&self) -> impl Iterator<Item = TrayEvent> + '_ {
        self.rx.try_iter()
    }
    pub fn show_menu(&self) {
        #[cfg(windows)]
        unsafe {
            let hwnd =
                HWND(TRAY_HWND.load(std::sync::atomic::Ordering::Acquire) as *mut std::ffi::c_void);
            if !hwnd.0.is_null() {
                CONTEXT_MENU_OPEN.store(true, std::sync::atomic::Ordering::Release);
                if PostMessageW(hwnd, WM_SHOW_MENU, WPARAM(0), LPARAM(0)).is_err() {
                    CONTEXT_MENU_OPEN.store(false, std::sync::atomic::Ordering::Release);
                }
            }
        }
    }
}
impl Drop for Tray {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe {
            let hwnd =
                HWND(TRAY_HWND.load(std::sync::atomic::Ordering::Acquire) as *mut std::ffi::c_void);
            if !hwnd.0.is_null() {
                // SAFETY: the icon is identified by our live hidden window and is deleted before the app exits.
                let _ = notify_icon(hwnd, NIM_DELETE);
                let _ = PostMessageW(
                    hwnd,
                    windows::Win32::UI::WindowsAndMessaging::WM_CLOSE,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }
    }
}

#[cfg(windows)]
const WM_SHOW_MENU: u32 = WM_APP + 51;
#[cfg(windows)]
const TRAY_CALLBACK: u32 = WM_APP + 52;
#[cfg(windows)]
const TRAY_RETRY_TIMER: usize = 1;
#[cfg(windows)]
static TRAY_HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
#[cfg(windows)]
static TRAY_EVENTS: std::sync::OnceLock<(
    std::sync::mpsc::Sender<TrayEvent>,
    eframe::egui::Context,
)> = std::sync::OnceLock::new();
#[cfg(windows)]
static TRAY_ICON: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
#[cfg(windows)]
static TASKBAR_CREATED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

#[cfg(windows)]
fn start_tray_thread(
    tx: std::sync::mpsc::Sender<TrayEvent>,
    ctx: eframe::egui::Context,
) -> Result<(), String> {
    use std::sync::mpsc;
    let _ = TRAY_EVENTS.set((tx, ctx));
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("player-notch-tray".into())
        .spawn(move || unsafe {
            let hinstance = GetModuleHandleW(None).unwrap_or_default().into();
            let wc = WNDCLASSW {
                lpfnWndProc: Some(tray_wnd_proc),
                hInstance: hinstance,
                lpszClassName: w!("PlayerNotchNativeTray"),
                ..Default::default()
            };
            if RegisterClassW(&wc) == 0 {
                let _ = ready_tx.send(Err("Register tray window class failed".to_owned()));
                return;
            }
            let hwnd = match CreateWindowExW(
                Default::default(),
                wc.lpszClassName,
                w!("PlayerNotch"),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                HWND::default(),
                None,
                hinstance,
                None,
            ) {
                Ok(h) => h,
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("Create tray window: {e}")));
                    return;
                }
            };
            TRAY_HWND.store(hwnd.0 as isize, std::sync::atomic::Ordering::Release);
            let icon = match icon_from_embedded_ico() {
                Ok(i) => i,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    let _ = DestroyWindow(hwnd);
                    return;
                }
            };
            TRAY_ICON.store(icon.0 as isize, std::sync::atomic::Ordering::Release);
            TASKBAR_CREATED.store(
                windows::Win32::UI::WindowsAndMessaging::RegisterWindowMessageW(w!(
                    "TaskbarCreated"
                )),
                std::sync::atomic::Ordering::Release,
            );
            add_tray_icon_or_retry(hwnd);
            let _ = ready_tx.send(Ok(()));
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, HWND::default(), 0, 0).0 > 0 {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let _ = notify_icon(hwnd, NIM_DELETE);
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyIcon(icon);
            TRAY_ICON.store(0, std::sync::atomic::Ordering::Release);
            TRAY_HWND.store(0, std::sync::atomic::Ordering::Release);
        })
        .map_err(|e| format!("Start tray thread: {e}"))?;
    ready_rx
        .recv()
        .map_err(|e| format!("Tray startup handshake: {e}"))??;
    Ok(())
}

#[cfg(windows)]
unsafe fn add_tray_icon_or_retry(hwnd: HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};
    unsafe {
        if notify_icon(hwnd, NIM_ADD).as_bool() {
            let _ = KillTimer(hwnd, TRAY_RETRY_TIMER);
            let mut data = tray_icon_data(hwnd);
            data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            let _ = Shell_NotifyIconW(NIM_SETVERSION, &data);
        } else {
            // Explorer can start after Run entries or restart later in the session.
            let _ = SetTimer(hwnd, TRAY_RETRY_TIMER, 2000, None);
        }
    }
}

#[cfg(windows)]
unsafe fn icon_from_embedded_ico() -> Result<windows::Win32::UI::WindowsAndMessaging::HICON, String>
{
    let ico = include_bytes!("../PlayerNotch.ico");
    if ico.len() < 22 || ico[0..4] != [0, 0, 1, 0] {
        return Err("Embedded PlayerNotch.ico is invalid".into());
    }
    let image_len = u32::from_le_bytes(ico[14..18].try_into().unwrap()) as usize;
    let offset = u32::from_le_bytes(ico[18..22].try_into().unwrap()) as usize;
    let end = offset
        .checked_add(image_len)
        .filter(|&v| v <= ico.len())
        .ok_or("ICO image entry is truncated")?;
    // SAFETY: the embedded ICO entry remains valid for the duration of the call and Windows copies the data into the icon handle.
    unsafe {
        windows::Win32::UI::WindowsAndMessaging::CreateIconFromResourceEx(
            &ico[offset..end],
            true,
            0x0003_0000,
            0,
            0,
            LR_DEFAULTSIZE,
        )
    }
    .map_err(|e| format!("Create tray icon: {e}"))
}

#[cfg(windows)]
fn tray_icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW::default();
    data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = hwnd;
    data.uID = 1;
    data.uFlags = NIF_MESSAGE | NIF_TIP | NIF_ICON;
    data.uCallbackMessage = TRAY_CALLBACK;
    data.hIcon = windows::Win32::UI::WindowsAndMessaging::HICON(
        TRAY_ICON.load(std::sync::atomic::Ordering::Acquire) as *mut _,
    );
    let tip: Vec<u16> = "PlayerNotch".encode_utf16().chain(Some(0)).collect();
    data.szTip[..tip.len()].copy_from_slice(&tip);
    data
}
#[cfg(windows)]
unsafe fn notify_icon(
    hwnd: HWND,
    op: windows::Win32::UI::Shell::NOTIFY_ICON_MESSAGE,
) -> windows::Win32::Foundation::BOOL {
    let data = tray_icon_data(hwnd);
    // SAFETY: data is fully initialized for this tray icon and lives through the synchronous shell call.
    unsafe { Shell_NotifyIconW(op, &data) }
}

#[cfg(windows)]
unsafe extern "system" fn tray_wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::*;
    if msg == WM_CLOSE {
        // SAFETY: hwnd is the current window supplied by Windows to its procedure.
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        return LRESULT(0);
    }
    if msg == WM_DESTROY {
        // SAFETY: this runs on the tray thread's message loop.
        unsafe {
            windows::Win32::UI::WindowsAndMessaging::PostQuitMessage(0);
        }
        return LRESULT(0);
    }
    let restart = TASKBAR_CREATED.load(std::sync::atomic::Ordering::Acquire);
    if (msg == restart && restart != 0) || (msg == WM_TIMER && wp.0 == TRAY_RETRY_TIMER) {
        // SAFETY: hwnd is our live hidden tray window.
        unsafe {
            add_tray_icon_or_retry(hwnd);
        }
        return LRESULT(0);
    }
    if msg == WM_SHOW_MENU || msg == TRAY_CALLBACK {
        let event = (lp.0 as u32 & 0xffff) as u32;
        if msg == WM_SHOW_MENU
            || event == WM_RBUTTONUP
            || event == WM_CONTEXTMENU
            || event == WM_LBUTTONUP
            || event == 0x0400
        {
            CONTEXT_MENU_OPEN.store(true, std::sync::atomic::Ordering::Release);
            // SAFETY: all menu operations execute on the window's owning thread and use a live HWND/HMENU.
            if let Ok(menu) = unsafe { CreatePopupMenu() } {
                let _ = unsafe { AppendMenuW(menu, MF_STRING | MF_DISABLED, 0, w!("PlayerNotch")) };
                let _ = unsafe { AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()) };
                let english = TRAY_ENGLISH.load(std::sync::atomic::Ordering::Acquire);
                let _ = unsafe {
                    AppendMenuW(
                        menu,
                        MF_STRING,
                        1,
                        if english {
                            w!("Settings")
                        } else {
                            w!("Настройки")
                        },
                    )
                };
                let _ = unsafe {
                    AppendMenuW(
                        menu,
                        MF_STRING,
                        2,
                        if english {
                            w!("Quit")
                        } else {
                            w!("Выход")
                        },
                    )
                };
                let mut p = POINT::default();
                let _ = unsafe { GetCursorPos(&mut p) };
                let _ = unsafe { SetForegroundWindow(hwnd) };
                let selected = unsafe {
                    TrackPopupMenu(
                        menu,
                        TPM_RETURNCMD | TPM_RIGHTBUTTON,
                        p.x,
                        p.y,
                        0,
                        hwnd,
                        None,
                    )
                };
                let _ = unsafe { DestroyMenu(menu) };
                if let Some((tx, _)) = TRAY_EVENTS.get() {
                    let event = match selected.0 {
                        1 => Some(TrayEvent::Settings),
                        2 => Some(TrayEvent::Quit),
                        _ => None,
                    };
                    if let Some(event) = event {
                        let _ = tx.send(event);
                    }
                }
            }
            CONTEXT_MENU_OPEN.store(false, std::sync::atomic::Ordering::Release);
            if let Some((_, ctx)) = TRAY_EVENTS.get() {
                ctx.request_repaint_of(eframe::egui::ViewportId::ROOT);
            }
            return LRESULT(0);
        }
        if event == WM_LBUTTONDBLCLK || event == 0x0401 {
            if let Some((tx, ctx)) = TRAY_EVENTS.get() {
                let _ = tx.send(TrayEvent::Settings);
                ctx.request_repaint_of(eframe::egui::ViewportId::ROOT);
            }
            return LRESULT(0);
        }
    }
    // SAFETY: delegate unhandled messages to the standard procedure for this live window.
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

/// Apply the same non-activating, tool-window style used by the Python overlay.
pub fn configure_overlay(hwnd_value: isize) {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{
            GWL_EXSTYLE, GetWindowLongPtrW, SetWindowLongPtrW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
        };
        let hwnd = HWND(hwnd_value as *mut std::ffi::c_void);
        if !hwnd.0.is_null() {
            let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            let _ = SetWindowLongPtrW(
                hwnd,
                GWL_EXSTYLE,
                style | WS_EX_NOACTIVATE.0 as isize | WS_EX_TOOLWINDOW.0 as isize,
            );
        }
    }
    #[cfg(not(windows))]
    {
        let _ = hwnd_value;
    }
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn embedded_tray_icon_decodes_as_windows_icon() {
        let icon = unsafe { super::icon_from_embedded_ico() }.unwrap();
        assert!(!icon.0.is_null());
        unsafe { windows::Win32::UI::WindowsAndMessaging::DestroyIcon(icon) }.unwrap();
    }
}
