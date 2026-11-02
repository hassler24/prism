//! The minimum Win32 surface Prism needs: a layered popup, GDI blits, a tray
//! icon, a global hotkey, and clipboard/file actions. Deliberately no `windows`
//! crate — a handful of declarations keeps the build offline and the binary lean.
#![allow(non_snake_case, non_camel_case_types, dead_code)]

use std::ffi::c_void;

pub type HWND = *mut c_void;
pub type HINSTANCE = *mut c_void;
pub type HMODULE = *mut c_void;
pub type HICON = *mut c_void;
pub type HCURSOR = *mut c_void;
pub type HDC = *mut c_void;
pub type HBITMAP = *mut c_void;
pub type HGDIOBJ = *mut c_void;
pub type HMENU = *mut c_void;
pub type HBRUSH = *mut c_void;
pub type HMONITOR = *mut c_void;
pub type HANDLE = *mut c_void;
pub type WPARAM = usize;
pub type LPARAM = isize;
pub type LRESULT = isize;
pub type BOOL = i32;
pub type UINT = u32;
pub type DWORD = u32;

pub const INVALID_HANDLE_VALUE: HANDLE = -1isize as HANDLE;
pub const STD_INPUT_HANDLE: u32 = -10i32 as u32;
pub const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
pub const STD_ERROR_HANDLE: u32 = -12i32 as u32;
pub const ATTACH_PARENT_PROCESS: u32 = -1i32 as u32;
pub const GENERIC_READ: u32 = 0x8000_0000;
pub const GENERIC_WRITE: u32 = 0x4000_0000;
pub const FILE_SHARE_READ: u32 = 0x0000_0001;
pub const FILE_SHARE_WRITE: u32 = 0x0000_0002;
pub const FILE_SHARE_DELETE: u32 = 0x0000_0004;
pub const FILE_TRAVERSE: u32 = 0x0000_0020;
pub const FILE_ATTRIBUTE_NORMAL: u32 = 0x0000_0080;
pub const OPEN_EXISTING: u32 = 3;

pub const ERROR_INVALID_PARAMETER: u32 = 87;
pub const THREAD_PRIORITY_BELOW_NORMAL: i32 = -1;
pub const THREAD_PRIORITY_LOWEST: i32 = -2;

pub const TRUE: BOOL = 1;
pub const FALSE: BOOL = 0;

pub const CS_DBLCLKS: u32 = 0x0008;
pub const WS_POPUP: u32 = 0x8000_0000;
pub const WS_EX_LAYERED: u32 = 0x0008_0000;
pub const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
pub const WS_EX_TOPMOST: u32 = 0x0000_0008;
pub const WS_EX_NOACTIVATE: u32 = 0x0800_0000;

pub const GWLP_USERDATA: i32 = -21;
pub const GWL_EXSTYLE: i32 = -20;

pub const WM_NULL: u32 = 0x0000;
pub const WM_CREATE: u32 = 0x0001;
pub const WM_DESTROY: u32 = 0x0002;
pub const WM_MOVE: u32 = 0x0003;
pub const WM_SIZE: u32 = 0x0005;
pub const WM_ACTIVATE: u32 = 0x0006;
pub const WM_SETFOCUS: u32 = 0x0007;
pub const WM_KILLFOCUS: u32 = 0x0008;
pub const WM_ENABLE: u32 = 0x000A;
pub const WM_PAINT: u32 = 0x000F;
pub const WM_CLOSE: u32 = 0x0010;

pub const WM_QUIT: u32 = 0x0012;
pub const WM_ERASEBKGND: u32 = 0x0014;
pub const WM_SHOWWINDOW: u32 = 0x0018;
pub const WM_ACTIVATEAPP: u32 = 0x001C;
pub const WM_KEYDOWN: u32 = 0x0100;
pub const WM_KEYUP: u32 = 0x0101;
pub const WM_CHAR: u32 = 0x0102;
pub const WM_SYSKEYDOWN: u32 = 0x0104;
pub const WM_SYSCHAR: u32 = 0x0106;
pub const WM_TIMER: u32 = 0x0113;
pub const WM_MOUSEMOVE: u32 = 0x0200;
pub const WM_LBUTTONDOWN: u32 = 0x0201;
pub const WM_LBUTTONUP: u32 = 0x0202;
pub const WM_LBUTTONDBLCLK: u32 = 0x0203;
pub const WM_RBUTTONDOWN: u32 = 0x0204;
pub const WM_RBUTTONUP: u32 = 0x0205;
pub const WM_MOUSEWHEEL: u32 = 0x020A;
pub const WM_MOUSELEAVE: u32 = 0x02A3;
pub const MK_LBUTTON: u32 = 0x0001;
pub const WM_HOTKEY: u32 = 0x0312;
pub const WM_NCCREATE: u32 = 0x0081;
pub const WM_NCHITTEST: u32 = 0x0084;
pub const WM_DPICHANGED: u32 = 0x02E0;
pub const WM_APP: u32 = 0x8000;
pub const WM_TRAY: u32 = WM_APP + 1;
pub const WM_SHOW_PRISM: u32 = WM_APP + 2;
pub const WM_REBUILD_DONE: u32 = WM_APP + 3;

pub const WA_INACTIVE: u32 = 0;
pub const SW_HIDE: i32 = 0;
pub const SW_SHOWNORMAL: i32 = 1;
pub const SW_SHOW: i32 = 5;

pub const VK_BACK: i32 = 0x08;
pub const VK_TAB: i32 = 0x09;
pub const VK_RETURN: i32 = 0x0D;
pub const VK_ESCAPE: i32 = 0x1B;
pub const VK_SPACE: i32 = 0x20;
pub const VK_PRIOR: i32 = 0x21;
pub const VK_NEXT: i32 = 0x22;
pub const VK_END: i32 = 0x23;
pub const VK_HOME: i32 = 0x24;
pub const VK_LEFT: i32 = 0x25;
pub const VK_UP: i32 = 0x26;
pub const VK_RIGHT: i32 = 0x27;
pub const VK_DOWN: i32 = 0x28;
pub const VK_DELETE: i32 = 0x2E;
pub const VK_CONTROL: i32 = 0x11;
pub const VK_SHIFT: i32 = 0x10;
pub const VK_MENU: i32 = 0x12;
pub const VK_A: i32 = 0x41;
pub const VK_C: i32 = 0x43;
pub const VK_V: i32 = 0x56;
pub const VK_F: i32 = 0x46;
pub const VK_X: i32 = 0x58;

pub const IDC_ARROW: u16 = 32512;
pub const IDC_IBEAM: u16 = 32513;
pub const IDC_HAND: u16 = 32649;
pub const TME_LEAVE: u32 = 0x0000_0002;

pub const MOD_ALT: u32 = 0x0001;
pub const MOD_CONTROL: u32 = 0x0002;
pub const MOD_SHIFT: u32 = 0x0004;
pub const MOD_WIN: u32 = 0x0008;
pub const MOD_NOREPEAT: u32 = 0x4000;

pub const ULW_ALPHA: u32 = 0x0000_0002;
pub const AC_SRC_OVER: u8 = 0x00;
pub const AC_SRC_ALPHA: u8 = 0x01;
pub const BI_RGB: u32 = 0;
pub const DIB_RGB_COLORS: u32 = 0;
pub const SRCCOPY: u32 = 0x00CC_0020;
pub const DEFAULT_CHARSET: u32 = 1;

pub const NIM_ADD: u32 = 0;
pub const NIM_MODIFY: u32 = 1;
pub const NIM_DELETE: u32 = 2;
pub const NIF_MESSAGE: u32 = 0x01;
pub const NIF_ICON: u32 = 0x02;
pub const NIF_TIP: u32 = 0x04;
pub const NIF_SHOWTIP: u32 = 0x80;

pub const MF_STRING: u32 = 0x0000;
pub const MF_SEPARATOR: u32 = 0x0800;
pub const TPM_RIGHTBUTTON: u32 = 0x0002;
pub const TPM_RETURNCMD: u32 = 0x0100;
pub const TPM_LEFTALIGN: u32 = 0x0000;

pub const MONITOR_DEFAULTTONEAREST: u32 = 2;

pub const CF_UNICODETEXT: u32 = 13;
pub const GMEM_MOVEABLE: u32 = 0x0002;

pub const SWP_NOSIZE: u32 = 0x0001;
pub const SWP_NOMOVE: u32 = 0x0002;
pub const SWP_NOACTIVATE: u32 = 0x0010;
pub const SWP_SHOWWINDOW: u32 = 0x0040;

pub const ERROR_ALREADY_EXISTS: u32 = 183;
/// `RegisterHotKey` already took this combination on this desktop.
pub const ERROR_HOTKEY_ALREADY_REGISTERED: u32 = 1409;

pub const MB_ICONERROR: u32 = 0x10;
pub const MB_ICONINFORMATION: u32 = 0x40;
pub const MB_OK: u32 = 0x0000;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct POINT {
    pub x: i32,
    pub y: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SIZE {
    pub cx: i32,
    pub cy: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct RECT {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct GUID {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct RGBQUAD {
    pub rgbBlue: u8,
    pub rgbGreen: u8,
    pub rgbRed: u8,
    pub rgbReserved: u8,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct BITMAPINFOHEADER {
    pub biSize: u32,
    pub biWidth: i32,
    pub biHeight: i32,
    pub biPlanes: u16,
    pub biBitCount: u16,
    pub biCompression: u32,
    pub biSizeImage: u32,
    pub biXPelsPerMeter: i32,
    pub biYPelsPerMeter: i32,
    pub biClrUsed: u32,
    pub biClrImportant: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct BITMAPINFO {
    pub bmiHeader: BITMAPINFOHEADER,
    pub bmiColors: [RGBQUAD; 1],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct BLENDFUNCTION {
    pub BlendOp: u8,
    pub BlendFlags: u8,
    pub SourceConstantAlpha: u8,
    pub AlphaFormat: u8,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct MSG {
    pub hwnd: HWND,
    pub message: u32,
    pub wParam: WPARAM,
    pub lParam: LPARAM,
    pub time: u32,
    pub pt: POINT,
    pub lPrivate: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct TRACKMOUSEEVENT {
    pub cbSize: u32,
    pub dwFlags: u32,
    pub hwndTrack: HWND,
    pub dwHoverTime: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct PAINTSTRUCT {
    pub hdc: HDC,
    pub fErase: BOOL,
    pub rcPaint: RECT,
    pub fRestore: BOOL,
    pub fIncUpdate: BOOL,
    pub rgbReserved: [u8; 32],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct MONITORINFO {
    pub cbSize: u32,
    pub rcMonitor: RECT,
    pub rcWork: RECT,
    pub dwFlags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ICONINFO {
    pub fIcon: BOOL,
    pub xHotspot: DWORD,
    pub yHotspot: DWORD,
    pub hbmMask: HBITMAP,
    pub hbmColor: HBITMAP,
}

pub type WNDPROC = Option<unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT>;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct WNDCLASSEXW {
    pub cbSize: u32,
    pub style: u32,
    pub lpfnWndProc: WNDPROC,
    pub cbClsExtra: i32,
    pub cbWndExtra: i32,
    pub hInstance: HINSTANCE,
    pub hIcon: HICON,
    pub hCursor: HCURSOR,
    pub hbrBackground: HBRUSH,
    pub lpszMenuName: *const u16,
    pub lpszClassName: *const u16,
    pub hIconSm: HICON,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct CREATESTRUCTW {
    pub lpCreateParams: *mut c_void,
    pub hInstance: HINSTANCE,
    pub hMenu: HMENU,
    pub hwndParent: HWND,
    pub cy: i32,
    pub cx: i32,
    pub y: i32,
    pub x: i32,
    pub style: i32,
    pub lpszName: *const u16,
    pub lpszClass: *const u16,
    pub dwExStyle: DWORD,
}

/// One range for `PrefetchVirtualMemory`.
#[repr(C)]
pub struct WIN32_MEMORY_RANGE_ENTRY {
    pub VirtualAddress: *mut c_void,
    pub NumberOfBytes: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NOTIFYICONDATAW {
    pub cbSize: u32,
    pub hWnd: HWND,
    pub uID: u32,
    pub uFlags: u32,
    pub uCallbackMessage: u32,
    pub hIcon: HICON,
    pub szTip: [u16; 128],
    pub dwState: u32,
    pub dwStateMask: u32,
    pub szInfo: [u16; 256],
    pub uTimeoutOrVersion: u32,
    pub szInfoTitle: [u16; 64],
    pub dwInfoFlags: u32,
    pub guidItem: GUID,
    pub hBalloonIcon: HICON,
}

impl Default for NOTIFYICONDATAW {
    fn default() -> Self {
        // SAFETY: plain-old-data struct
        unsafe { std::mem::zeroed() }
    }
}

/// UTF-8 → NUL-terminated UTF-16.
pub fn wide(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    v
}

/// Copy `s` into a fixed UTF-16 buffer, always NUL-terminated.
pub fn wide_into(dst: &mut [u16], s: &str) {
    let mut i = 0;
    for u in s.encode_utf16() {
        if i + 1 >= dst.len() {
            break;
        }
        dst[i] = u;
        i += 1;
    }
    dst[i] = 0;
}

pub fn from_wide_buf(w: &[u16]) -> String {
    let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end])
}

#[link(name = "kernel32")]
extern "system" {
    pub fn GetModuleHandleW(lpModuleName: *const u16) -> HMODULE;
    pub fn GetCurrentProcess() -> HANDLE;
    pub fn PrefetchVirtualMemory(
        hProcess: HANDLE,
        NumberOfEntries: usize,
        VirtualAddresses: *const WIN32_MEMORY_RANGE_ENTRY,
        Flags: u32,
    ) -> BOOL;
    pub fn GetLastError() -> DWORD;
    pub fn GetTickCount64() -> u64;
    pub fn GetCurrentThread() -> HANDLE;
    pub fn SetThreadPriority(hThread: HANDLE, nPriority: i32) -> BOOL;
    pub fn DeviceIoControl(
        hDevice: HANDLE,
        dwIoControlCode: u32,
        lpInBuffer: *const c_void,
        nInBufferSize: u32,
        lpOutBuffer: *mut c_void,
        nOutBufferSize: u32,
        lpBytesReturned: *mut u32,
        lpOverlapped: *mut c_void,
    ) -> BOOL;
    pub fn OpenFileById(
        hVolumeHint: HANDLE,
        lpFileId: *const c_void,
        dwDesiredAccess: u32,
        dwShareMode: u32,
        lpSecurityAttributes: *const c_void,
        dwFlagsAndAttributes: u32,
    ) -> HANDLE;
    pub fn GetFileInformationByHandleEx(
        hFile: HANDLE,
        FileInformationClass: u32,
        lpFileInformation: *mut c_void,
        dwBufferSize: u32,
    ) -> BOOL;
    pub fn CreateMutexW(
        lpMutexAttributes: *const c_void,
        bInitialOwner: BOOL,
        lpName: *const u16,
    ) -> HANDLE;
    pub fn CloseHandle(hObject: HANDLE) -> BOOL;
    pub fn GlobalAlloc(uFlags: u32, dwBytes: usize) -> HANDLE;
    pub fn GlobalLock(hMem: HANDLE) -> *mut c_void;
    pub fn GlobalUnlock(hMem: HANDLE) -> BOOL;
    pub fn GlobalFree(hMem: HANDLE) -> HANDLE;
    pub fn AttachConsole(dwProcessId: u32) -> BOOL;
    pub fn GetStdHandle(nStdHandle: u32) -> HANDLE;
    pub fn SetStdHandle(nStdHandle: u32, hHandle: HANDLE) -> BOOL;
    pub fn CreateFileW(
        lpFileName: *const u16,
        dwDesiredAccess: u32,
        dwShareMode: u32,
        lpSecurityAttributes: *const c_void,
        dwCreationDisposition: u32,
        dwFlagsAndAttributes: u32,
        hTemplateFile: HANDLE,
    ) -> HANDLE;
}

/// Give CLI modes a console when the release binary is built as a GUI app.
///
/// Three launch shapes have to work:
///
/// * **a real terminal** — the GUI subsystem means the child got no console of its
///   own, so attach to the parent's and reopen `CONOUT$`/`CONIN$` for the three
///   standard handles, which are NULL in that case.
/// * **redirected to a pipe or file** (`prism --search x > out.txt`, a CI step) —
///   the standard handles are *valid pipe handles* and must be left alone. This is
///   the case the first version got wrong: `AttachConsole` can succeed even with a
///   redirected pipe, and then `CONOUT$` replaced a working pipe with a console
///   nobody reads, so every `println!` vanished and `--search` looked broken.
/// * **double-clicked with no arguments** — no console wanted, nothing happens.
pub fn ensure_cli_console() {
    unsafe {
        // A handle that is already set (a pipe, a file, NUL) is the caller's; only
        // NULL means "GUI child of a console, please wire me up".
        let valid = |h: HANDLE| !h.is_null() && h != INVALID_HANDLE_VALUE;
        if valid(GetStdHandle(STD_OUTPUT_HANDLE))
            && valid(GetStdHandle(STD_ERROR_HANDLE))
            && valid(GetStdHandle(STD_INPUT_HANDLE))
        {
            return;
        }
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return;
        }
        if !valid(GetStdHandle(STD_OUTPUT_HANDLE)) || !valid(GetStdHandle(STD_ERROR_HANDLE)) {
            let name = wide("CONOUT$");
            let out = CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            );
            if valid(out) {
                if !valid(GetStdHandle(STD_OUTPUT_HANDLE)) {
                    SetStdHandle(STD_OUTPUT_HANDLE, out);
                }
                if !valid(GetStdHandle(STD_ERROR_HANDLE)) {
                    SetStdHandle(STD_ERROR_HANDLE, out);
                }
            }
        }
        if !valid(GetStdHandle(STD_INPUT_HANDLE)) {
            let name = wide("CONIN$");
            let inp = CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            );
            if valid(inp) {
                SetStdHandle(STD_INPUT_HANDLE, inp);
            }
        }
    }
}

#[link(name = "user32")]
extern "system" {
    pub fn RegisterClassExW(lpWndClass: *const WNDCLASSEXW) -> u16;
    pub fn CreateWindowExW(
        dwExStyle: u32,
        lpClassName: *const u16,
        lpWindowName: *const u16,
        dwStyle: u32,
        X: i32,
        Y: i32,
        nWidth: i32,
        nHeight: i32,
        hWndParent: HWND,
        hMenu: HMENU,
        hInstance: HINSTANCE,
        lpParam: *mut c_void,
    ) -> HWND;
    pub fn DefWindowProcW(hWnd: HWND, Msg: u32, wParam: WPARAM, lParam: LPARAM) -> LRESULT;
    pub fn DestroyWindow(hWnd: HWND) -> BOOL;
    pub fn ShowWindow(hWnd: HWND, nCmdShow: i32) -> BOOL;
    pub fn SetForegroundWindow(hWnd: HWND) -> BOOL;
    pub fn GetForegroundWindow() -> HWND;
    pub fn SetFocus(hWnd: HWND) -> HWND;
    pub fn GetMessageW(lpMsg: *mut MSG, hWnd: HWND, wMsgFilterMin: u32, wMsgFilterMax: u32)
        -> BOOL;
    pub fn TranslateMessage(lpMsg: *const MSG) -> BOOL;
    pub fn DispatchMessageW(lpMsg: *const MSG) -> LRESULT;
    pub fn PostQuitMessage(nExitCode: i32);
    pub fn PostMessageW(hWnd: HWND, Msg: u32, wParam: WPARAM, lParam: LPARAM) -> BOOL;
    pub fn SendMessageW(hWnd: HWND, Msg: u32, wParam: WPARAM, lParam: LPARAM) -> LRESULT;
    pub fn GetWindowLongPtrW(hWnd: HWND, nIndex: i32) -> isize;
    pub fn SetWindowLongPtrW(hWnd: HWND, nIndex: i32, dwNewLong: isize) -> isize;
    pub fn FindWindowW(lpClassName: *const u16, lpWindowName: *const u16) -> HWND;
    pub fn RegisterHotKey(hWnd: HWND, id: i32, fsModifiers: u32, vk: u32) -> BOOL;
    pub fn UnregisterHotKey(hWnd: HWND, id: i32) -> BOOL;
    pub fn GetKeyState(nVirtKey: i32) -> i16;
    pub fn GetCursorPos(lpPoint: *mut POINT) -> BOOL;
    pub fn SetCapture(hWnd: HWND) -> HWND;
    pub fn ReleaseCapture() -> BOOL;
    pub fn TrackMouseEvent(lpEventTrack: *mut TRACKMOUSEEVENT) -> BOOL;
    pub fn GetDC(hWnd: HWND) -> HDC;
    pub fn ReleaseDC(hWnd: HWND, hDC: HDC) -> i32;
    pub fn UpdateLayeredWindow(
        hWnd: HWND,
        hdcDst: HDC,
        pptDst: *const POINT,
        psize: *const SIZE,
        hdcSrc: HDC,
        pptSrc: *const POINT,
        crKey: u32,
        pblend: *const BLENDFUNCTION,
        dwFlags: u32,
    ) -> BOOL;
    pub fn BeginPaint(hWnd: HWND, lpPaint: *mut PAINTSTRUCT) -> HDC;
    pub fn EndPaint(hWnd: HWND, lpPaint: *const PAINTSTRUCT) -> BOOL;
    pub fn SetTimer(hWnd: HWND, nIDEvent: usize, uElapse: u32, lpTimerFunc: *mut c_void) -> usize;
    pub fn KillTimer(hWnd: HWND, uIDEvent: usize) -> BOOL;
    pub fn LoadCursorW(hInstance: HINSTANCE, lpCursorName: *const u16) -> HCURSOR;
    pub fn LoadIconW(hInstance: HINSTANCE, lpIconName: *const u16) -> HICON;
    pub fn CreateIconIndirect(piconinfo: *const ICONINFO) -> HICON;
    pub fn CreatePopupMenu() -> HMENU;
    pub fn AppendMenuW(hMenu: HMENU, uFlags: u32, uIDNewItem: usize, lpNewItem: *const u16)
        -> BOOL;
    pub fn TrackPopupMenu(
        hMenu: HMENU,
        uFlags: u32,
        x: i32,
        y: i32,
        nReserved: i32,
        hWnd: HWND,
        prcRect: *const RECT,
    ) -> u32;
    pub fn DestroyMenu(hMenu: HMENU) -> BOOL;
    pub fn MonitorFromPoint(pt: POINT, dwFlags: u32) -> HMONITOR;
    pub fn GetMonitorInfoW(hMonitor: HMONITOR, lpmi: *mut MONITORINFO) -> BOOL;
    pub fn GetSystemMetrics(nIndex: i32) -> i32;
    pub fn SetProcessDpiAwarenessContext(value: isize) -> BOOL;
    pub fn GetDpiForSystem() -> u32;
    pub fn MessageBoxW(hWnd: HWND, lpText: *const u16, lpCaption: *const u16, uType: u32) -> i32;
    pub fn OpenClipboard(hWndNewOwner: HWND) -> BOOL;
    pub fn EmptyClipboard() -> BOOL;
    pub fn SetClipboardData(uFormat: u32, hMem: HANDLE) -> HANDLE;
    pub fn GetClipboardData(uFormat: u32) -> HANDLE;
    pub fn CloseClipboard() -> BOOL;
    pub fn IsClipboardFormatAvailable(format: u32) -> BOOL;
    pub fn SetWindowPos(
        hWnd: HWND,
        hWndInsertAfter: HWND,
        X: i32,
        Y: i32,
        cx: i32,
        cy: i32,
        uFlags: u32,
    ) -> BOOL;
    pub fn GetWindowRect(hWnd: HWND, lpRect: *mut RECT) -> BOOL;
    pub fn InvalidateRect(hWnd: HWND, lpRect: *const RECT, bErase: BOOL) -> BOOL;
    pub fn SetCursor(hCursor: HCURSOR) -> HCURSOR;
    pub fn ScreenToClient(hWnd: HWND, lpPoint: *mut POINT) -> BOOL;
}

#[link(name = "gdi32")]
extern "system" {
    pub fn CreateCompatibleDC(hdc: HDC) -> HDC;
    pub fn CreateCompatibleBitmap(hdc: HDC, cx: i32, cy: i32) -> HBITMAP;
    pub fn CreateDIBSection(
        hdc: HDC,
        pbmi: *const BITMAPINFO,
        usage: u32,
        ppvBits: *mut *mut c_void,
        hSection: HANDLE,
        offset: u32,
    ) -> HBITMAP;
    pub fn SelectObject(hdc: HDC, h: HGDIOBJ) -> HGDIOBJ;
    pub fn DeleteObject(ho: HGDIOBJ) -> BOOL;
    pub fn DeleteDC(hdc: HDC) -> BOOL;
    pub fn BitBlt(
        hdcDest: HDC,
        x: i32,
        y: i32,
        cx: i32,
        cy: i32,
        hdcSrc: HDC,
        x1: i32,
        y1: i32,
        rop: u32,
    ) -> BOOL;
    pub fn CreateBitmap(
        nWidth: i32,
        nHeight: i32,
        nPlanes: u32,
        nBitCount: u32,
        lpBits: *const c_void,
    ) -> HBITMAP;
}

#[link(name = "shell32")]
extern "system" {
    pub fn ShellExecuteW(
        hwnd: HWND,
        lpOperation: *const u16,
        lpFile: *const u16,
        lpParameters: *const u16,
        lpDirectory: *const u16,
        nShowCmd: i32,
    ) -> HINSTANCE;
    pub fn Shell_NotifyIconW(dwMessage: u32, lpData: *mut NOTIFYICONDATAW) -> BOOL;
}
