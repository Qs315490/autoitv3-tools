//! The dialogs and feedback windows the Win32 backend shows.
//!
//! AutoIt's dialogs block: `MsgBox` waits for a button, `InputBox` for a name,
//! `FileOpenDialog` for a file. On Windows with a user in front of it this does
//! the same, and the semantics layer only falls back to its scripted answer when
//! a backend says it cannot show one — which is what the headless and offscreen
//! backends say, so an analysis run never waits for somebody who is not there.
//!
//! `InputBox` builds its window by hand and runs its own message loop rather than
//! going through a dialog template: the template layout rules are fiddly, and a
//! window whose procedure is ours keeps the result where we can read it.
//!
//! The splash, progress and tooltip windows are *not* modal: a script opens one,
//! keeps working, and closes it later, so they are created once and updated in
//! place.

use std::cell::RefCell;
use std::ffi::c_void;

use autoitv3_gui_model::{Progress, Splash};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, GetSaveFileNameW, OPENFILENAMEW,
};
use windows_sys::Win32::UI::Controls::{InitCommonControlsEx, INITCOMMONCONTROLSEX};
use windows_sys::Win32::UI::Shell::{SHBrowseForFolderW, SHGetPathFromIDListW, BROWSEINFOW};
use windows_sys::Win32::Graphics::Gdi::UpdateWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetDlgItem, GetMessageW,
    IsDialogMessageW, MessageBoxW, RegisterClassExW, SendMessageW, SetWindowTextW, ShowWindow,
    TranslateMessage, MSG, WNDCLASSEXW,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const WS_POPUP: u32 = 0x8000_0000;
const WS_CAPTION: u32 = 0x00C0_0000;
const WS_SYSMENU: u32 = 0x0008_0000;
const WS_BORDER: u32 = 0x0080_0000;
const WS_CHILD: u32 = 0x4000_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const WS_TABSTOP: u32 = 0x0001_0000;
const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
const SS_LEFT: u32 = 0x0000_0000;
const SS_CENTER: u32 = 0x0000_0001;
const BS_DEFPUSHBUTTON: u32 = 0x0000_0001;
const ES_AUTOHSCROLL: u32 = 0x0000_0080;
const SW_SHOW: i32 = 5;
const SW_HIDE: i32 = 0;
const EM_SETPASSWORDCHAR: u32 = 0x00CC;
const PBM_SETPOS: u32 = 0x0402;
const PBM_SETRANGE: u32 = 0x0401;
const WM_COMMAND: u32 = 0x0111;
const WM_CLOSE: u32 = 0x0010;

/// The identifiers the input box's own controls answer with.
const ID_PROMPT: i32 = 1000;
const ID_EDIT: i32 = 1001;
const ID_OK: i32 = 1;
const ID_CANCEL: i32 = 2;

/// `OPENFILENAMEW` flags.
const OFN_OVERWRITEPROMPT: u32 = 0x0000_0002;
const OFN_NOCHANGEDIR: u32 = 0x0000_0008;
const OFN_PATHMUSTEXIST: u32 = 0x0000_0800;
const OFN_FILEMUSTEXIST: u32 = 0x0000_1000;
const OFN_EXPLORER: u32 = 0x0008_0000;

/// `SHBrowseForFolderW` flags.
const BIF_RETURNONLYFSDIRS: u32 = 0x0000_0001;
const BIF_NEWDIALOGSTYLE: u32 = 0x0000_0040;
const BIF_EDITBOX: u32 = 0x0000_0010;

/// `InitCommonControlsEx` bits the progress bar needs.
const ICC_PROGRESS_CLASS: u32 = 0x0000_0020;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A NUL-terminated UTF-16 copy of `text`.
fn to_wide(text: &str) -> Vec<u16> {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    wide.push(0);
    wide
}

/// The text in a NUL-terminated UTF-16 buffer.
fn from_wide(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|unit| *unit == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

/// `(x, y, width, height)` for a control.
fn rect(x: i32, y: i32, width: i32, height: i32) -> (i32, i32, i32, i32) {
    (x, y, width, height)
}

/// Register `class` once, with `proc` as its window procedure.
fn register_class(class: &str, proc: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT) -> bool {
    static REGISTERED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *REGISTERED.get_or_init(|| unsafe {
        let name = to_wide(class);
        let mut wc: WNDCLASSEXW = std::mem::zeroed();
        wc.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
        wc.lpfnWndProc = Some(proc);
        wc.hInstance = GetModuleHandleW(std::ptr::null());
        wc.lpszClassName = name.as_ptr();
        RegisterClassExW(&wc) != 0
    })
}

// ---------------------------------------------------------------------------
// MsgBox
// ---------------------------------------------------------------------------

/// `MsgBox`, answered by the user. The flags are AutoIt's, which are the Win32
/// `MB_*` values the same way the message box's own are.
pub(crate) fn message_box(flags: i64, title: &str, text: &str) -> i64 {
    let title = to_wide(title);
    let text = to_wide(text);
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            flags as u32,
        ) as i64
    }
}

// ---------------------------------------------------------------------------
// InputBox
// ---------------------------------------------------------------------------

/// What the input box that is running knows.
struct InputState {
    /// `Some` once the user accepted.
    answer: Option<String>,
}

thread_local! {
    /// The input box on this thread, if one is running. It is thread-local
    /// because a window procedure has no other way back to the call that made
    /// the window.
    static INPUT: RefCell<Option<InputState>> = const { RefCell::new(None) };
}

/// The window procedure of the input box: OK reads the edit, Cancel and the
/// close box answer "cancelled".
unsafe extern "system" fn input_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_COMMAND => {
            let id = (wparam & 0xFFFF) as i32;
            if id == ID_OK {
                let edit = GetDlgItem(hwnd, ID_EDIT);
                let mut buffer = vec![0u16; 2048];
                let length = windows_sys::Win32::UI::WindowsAndMessaging::GetWindowTextW(
                    edit,
                    buffer.as_mut_ptr(),
                    buffer.len() as i32,
                );
                let answer = String::from_utf16_lossy(&buffer[..length.max(0) as usize]);
                INPUT.with(|state| {
                    if let Some(state) = state.borrow_mut().as_mut() {
                        state.answer = Some(answer);
                    }
                });
                DestroyWindow(hwnd);
                return 0;
            }
            if id == ID_CANCEL {
                DestroyWindow(hwnd);
                return 0;
            }
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
            return 0;
        }
        _ => {}
    }
    DefWindowProcW(hwnd, message, wparam, lparam)
}

/// `InputBox`, answered by the user.
///
/// The prompt is one static, the value one edit, and OK/Cancel a pair of
/// buttons; `IsDialogMessageW` in the loop is what makes Tab, Return and Escape
/// do what a user expects.
pub(crate) fn input_box(
    title: &str,
    prompt: &str,
    default: &str,
    password: bool,
) -> Option<Option<String>> {
    if !register_class("Au3EmulatedInputBox", input_proc) {
        return None;
    }
    let class = to_wide("Au3EmulatedInputBox");
    let title = to_wide(title);
    let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let (x, y, width, height) = (0, 0, 400, 190);
    let window = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_VISIBLE,
            x,
            y,
            width,
            height,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        )
    };
    if window.is_null() {
        return None;
    }
    let static_class = to_wide("STATIC");
    let edit_class = to_wide("EDIT");
    let button_class = to_wide("BUTTON");
    let prompt_text = to_wide(prompt);
    let default_text = to_wide(default);
    let ok_text = to_wide("OK");
    let cancel_text = to_wide("Cancel");
    let (px, py, pw, ph) = rect(10, 10, 370, 70);
    let (ex, ey, ew, eh) = rect(10, 85, 370, 24);
    let (ox, oy, ow, oh) = rect(230, 120, 75, 26);
    let (cx, cy, cw, ch) = rect(310, 120, 75, 26);
    unsafe {
        let prompt_control = CreateWindowExW(
            0,
            static_class.as_ptr(),
            prompt_text.as_ptr(),
            WS_CHILD | WS_VISIBLE | SS_LEFT,
            px,
            py,
            pw,
            ph,
            window,
            ID_PROMPT as _,
            instance,
            std::ptr::null(),
        );
        let _ = prompt_control;
        let edit = CreateWindowExW(
            0x0000_0200, // WS_EX_CLIENTEDGE
            edit_class.as_ptr(),
            default_text.as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | ES_AUTOHSCROLL,
            ex,
            ey,
            ew,
            eh,
            window,
            ID_EDIT as _,
            instance,
            std::ptr::null(),
        );
        let ok = CreateWindowExW(
            0,
            button_class.as_ptr(),
            ok_text.as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_DEFPUSHBUTTON,
            ox,
            oy,
            ow,
            oh,
            window,
            ID_OK as _,
            instance,
            std::ptr::null(),
        );
        let cancel = CreateWindowExW(
            0,
            button_class.as_ptr(),
            cancel_text.as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            cx,
            cy,
            cw,
            ch,
            window,
            ID_CANCEL as _,
            instance,
            std::ptr::null(),
        );
        let _ = (ok, cancel);
        if password {
            SendMessageW(edit, EM_SETPASSWORDCHAR, '*' as usize, 0);
        }
        SendMessageW(window, 0x0028, 0, edit as isize); // WM_NEXTDLGCTL
    }
    with_input(|state| {
        *state = Some(InputState { answer: None });
    });
    unsafe {
        ShowWindow(window, SW_SHOW);
        UpdateWindow(window);
    }
    // The modal loop: `GetMessageW` blocks until something arrives, and
    // `IsDialogMessageW` turns Tab, Return and Escape into dialog behaviour.
    // It ends when the window procedure destroys the window.
    let mut message: MSG = unsafe { std::mem::zeroed() };
    while unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) } > 0 {
        let handled = unsafe { IsDialogMessageW(window, &message) } != 0;
        if handled {
            continue;
        }
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    // The box ran, so its answer is the answer: `None` means the user
    // cancelled, which is a different thing from "this backend cannot ask".
    Some(with_input(|state| state.take()).and_then(|state| state.answer))
}

/// Run `f` against this thread's input box.
fn with_input<R>(f: impl FnOnce(&mut Option<InputState>) -> R) -> R {
    INPUT.with(|state| f(&mut state.borrow_mut()))
}

// ---------------------------------------------------------------------------
// File dialogs
// ---------------------------------------------------------------------------

/// The filter string `GetOpenFileNameW` wants: one description and one pattern
/// per entry, each NUL-terminated, and a final empty entry.
///
/// AutoIt's own filter is a description that usually ends in the patterns in
/// parentheses (`"Text files (*.txt;*.log)"`), which is where the patterns come
/// from; without any, everything is offered.
fn build_filter(filter: &str) -> Vec<u16> {
    let mut wide = Vec::new();
    let (description, patterns) = match (filter.find('('), filter.rfind(')')) {
        (Some(open), Some(close)) if close > open => (
            filter[..open].trim().to_string(),
            filter[open + 1..close].replace(';', "\u{0}"),
        ),
        _ => (filter.trim().to_string(), String::new()),
    };
    let description = if description.is_empty() {
        "All files (*.*)".to_string()
    } else {
        description
    };
    let patterns = if patterns.is_empty() {
        "*.*".to_string()
    } else {
        patterns
    };
    wide.extend(to_wide(&description));
    wide.pop();
    wide.push(0);
    wide.extend(patterns.encode_utf16());
    wide.push(0);
    wide.push(0);
    wide
}

/// `FileOpenDialog`/`FileSaveDialog`, answered by the user.
pub(crate) fn file_dialog(
    save: bool,
    title: &str,
    initial: &str,
    filter: &str,
    default_name: &str,
    multi: bool,
) -> Option<Option<String>> {
    let title = to_wide(title);
    let initial = to_wide(initial);
    let filter = build_filter(filter);
    let mut buffer = vec![0u16; 32_768];
    {
        let default = to_wide(default_name);
        let length = default.len().min(buffer.len());
        buffer[..length].copy_from_slice(&default[..length]);
    }
    let mut options: OPENFILENAMEW = unsafe { std::mem::zeroed() };
    options.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
    options.hwndOwner = std::ptr::null_mut();
    options.lpstrFilter = filter.as_ptr();
    options.lpstrFile = buffer.as_mut_ptr();
    options.nMaxFile = buffer.len() as u32;
    options.lpstrInitialDir = initial.as_ptr();
    options.lpstrTitle = title.as_ptr();
    options.Flags = OFN_EXPLORER
        | OFN_NOCHANGEDIR
        | if save {
            OFN_OVERWRITEPROMPT
        } else {
            OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST
        }
        | if multi { 0x0000_0200 } else { 0 }; // OFN_ALLOWMULTISELECT
    let accepted = unsafe {
        if save {
            GetSaveFileNameW(&mut options)
        } else {
            GetOpenFileNameW(&mut options)
        }
    };
    if accepted == 0 {
        // The dialog ran and the user cancelled.
        return Some(None);
    }
    Some(Some(from_wide(&buffer)))
}

/// `FileSelectFolder`, answered by the user.
pub(crate) fn select_folder(title: &str, initial: &str) -> Option<Option<String>> {
    let title = to_wide(title);
    let mut display = vec![0u16; 260];
    let mut browse: BROWSEINFOW = unsafe { std::mem::zeroed() };
    browse.hwndOwner = std::ptr::null_mut();
    browse.pszDisplayName = display.as_mut_ptr();
    browse.lpszTitle = title.as_ptr();
    browse.ulFlags = BIF_RETURNONLYFSDIRS | BIF_EDITBOX | BIF_NEWDIALOGSTYLE;
    let _ = initial;
    let item = unsafe { SHBrowseForFolderW(&browse) };
    if item.is_null() {
        return Some(None);
    }
    let mut path = vec![0u16; 32_768];
    let ok = unsafe { SHGetPathFromIDListW(item, path.as_mut_ptr()) };
    unsafe { CoTaskMemFree(item as *const c_void) };
    if ok == 0 {
        return Some(None);
    }
    Some(Some(from_wide(&path)))
}

// ---------------------------------------------------------------------------
// Splash, progress and tooltip windows
// ---------------------------------------------------------------------------

/// The procedure of the feedback windows: they are only ever closed.
unsafe extern "system" fn feedback_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_CLOSE {
        ShowWindow(hwnd, SW_HIDE);
        return 0;
    }
    DefWindowProcW(hwnd, message, wparam, lparam)
}

/// The splash, progress and tooltip windows one backend has open.
///
/// Each is a plain popup with static children; nothing is owner-drawn, so a
/// repaint is Windows' own business.
#[derive(Default)]
pub(crate) struct Feedback {
    splash: Option<HWND>,
    progress: Option<HWND>,
    progress_bar: Option<HWND>,
    progress_main: Option<HWND>,
    progress_sub: Option<HWND>,
    tooltip: Option<HWND>,
}

impl Feedback {
    /// Show, move or hide the splash window.
    pub(crate) fn splash(&mut self, splash: &Splash, off: bool) -> bool {
        if off || !splash.visible {
            if let Some(window) = self.splash.take() {
                unsafe { DestroyWindow(window) };
            }
            return true;
        }
        if self.splash.is_none() {
            self.splash = create_popup("Au3EmulatedSplash", &splash.text, 0, 0, 320, 120);
        }
        if let Some(window) = self.splash {
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::MoveWindow(
                    window,
                    splash.x,
                    splash.y,
                    splash.width.max(60),
                    splash.height.max(40),
                    1,
                );
                ShowWindow(window, SW_SHOW);
            }
        }
        true
    }

    /// Show or update the progress window.
    pub(crate) fn progress(&mut self, progress: &Progress, off: bool) -> bool {
        if off || !progress.on {
            if let Some(window) = self.progress.take() {
                unsafe { DestroyWindow(window) };
            }
            self.progress_bar = None;
            self.progress_main = None;
            self.progress_sub = None;
            return true;
        }
        if self.progress.is_none() {
            // The progress bar comes from the common controls, which have to be
            // told about it before the class exists.
            unsafe {
                let mut common: INITCOMMONCONTROLSEX = std::mem::zeroed();
                common.dwSize = std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32;
                common.dwICC = ICC_PROGRESS_CLASS;
                InitCommonControlsEx(&common);
            }
            let Some(window) = create_popup("Au3EmulatedProgress", progress.text.as_str(), 0, 0, 300, 100)
            else {
                return false;
            };
            let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
            let static_class = to_wide("STATIC");
            let bar_class = to_wide("msctls_progress32");
            let empty = to_wide("");
            unsafe {
                let main = CreateWindowExW(
                    0,
                    static_class.as_ptr(),
                    empty.as_ptr(),
                    WS_CHILD | WS_VISIBLE | SS_LEFT,
                    10,
                    8,
                    280,
                    20,
                    window,
                    std::ptr::null_mut(),
                    instance,
                    std::ptr::null(),
                );
                let sub = CreateWindowExW(
                    0,
                    static_class.as_ptr(),
                    empty.as_ptr(),
                    WS_CHILD | WS_VISIBLE | SS_LEFT,
                    10,
                    30,
                    280,
                    18,
                    window,
                    std::ptr::null_mut(),
                    instance,
                    std::ptr::null(),
                );
                let bar = CreateWindowExW(
                    0x0000_0200,
                    bar_class.as_ptr(),
                    empty.as_ptr(),
                    WS_CHILD | WS_VISIBLE,
                    10,
                    56,
                    280,
                    22,
                    window,
                    std::ptr::null_mut(),
                    instance,
                    std::ptr::null(),
                );
                // `MAKELPARAM(0, 100)`: the bar runs from 0 to 100.
                SendMessageW(bar, PBM_SETRANGE, 0, (100u32 << 16) as isize);
                self.progress_main = Some(main);
                self.progress_sub = Some(sub);
                self.progress_bar = Some(bar);
            }
            self.progress = Some(window);
        }
        if let Some(bar) = self.progress_bar {
            unsafe { SendMessageW(bar, PBM_SETPOS, progress.percent.clamp(0, 100) as usize, 0) };
        }
        let main = progress.text.clone();
        let sub = progress.sub.clone();
        if let Some(control) = self.progress_main {
            let text = to_wide(&main);
            unsafe { SetWindowTextW(control, text.as_ptr()) };
        }
        if let Some(control) = self.progress_sub {
            let text = to_wide(&sub);
            unsafe { SetWindowTextW(control, text.as_ptr()) };
        }
        if let Some(window) = self.progress {
            unsafe { ShowWindow(window, SW_SHOW) };
        }
        true
    }

    /// Show the tooltip window, or hide it when the text is empty.
    pub(crate) fn tooltip(&mut self, text: &str, x: i32, y: i32) -> bool {
        if text.is_empty() {
            if let Some(window) = self.tooltip.take() {
                unsafe { DestroyWindow(window) };
            }
            return true;
        }
        let width = (text.chars().count() as i32 * 7 + 20).clamp(40, 600);
        if self.tooltip.is_none() {
            self.tooltip = create_popup("Au3EmulatedTooltip", text, x, y, width, 24);
        }
        if let Some(window) = self.tooltip {
            let wide = to_wide(text);
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::MoveWindow(window, x, y, width, 24, 1);
                SetWindowTextW(window, wide.as_ptr());
                ShowWindow(window, SW_SHOW);
            }
        }
        true
    }
}

impl Drop for Feedback {
    fn drop(&mut self) {
        for window in [self.splash, self.progress, self.tooltip].into_iter().flatten() {
            unsafe { DestroyWindow(window) };
        }
    }
}

/// A borderless popup with `text` as its caption, shown at `(x, y)`.
fn create_popup(class: &str, text: &str, x: i32, y: i32, width: i32, height: i32) -> Option<HWND> {
    if !register_class(class, feedback_proc) {
        return None;
    }
    let class = to_wide(class);
    let text = to_wide(text);
    let window = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class.as_ptr(),
            text.as_ptr(),
            WS_POPUP | WS_BORDER,
            x,
            y,
            width,
            height,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null(),
        )
    };
    // The caption of a borderless popup is not painted, so the text goes into a
    // static that fills it.
    let static_class = to_wide("STATIC");
    unsafe {
        CreateWindowExW(
            0,
            static_class.as_ptr(),
            text.as_ptr(),
            WS_CHILD | WS_VISIBLE | SS_CENTER,
            0,
            0,
            width,
            height,
            window,
            std::ptr::null_mut(),
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null(),
        );
    }
    Some(window)
}
