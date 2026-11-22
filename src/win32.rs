use std::ffi::c_void;

pub type Handle = *mut c_void;
type WindowProc = unsafe extern "system" fn(Handle, u32, usize, isize) -> isize;

#[repr(C)]
pub struct WindowClass {
    pub style: u32,
    pub procedure: WindowProc,
    pub class_extra: i32,
    pub window_extra: i32,
    pub instance: Handle,
    pub icon: Handle,
    pub cursor: Handle,
    pub background: Handle,
    pub menu: *const u16,
    pub name: *const u16,
}

#[repr(C)]
pub struct Message {
    pub window: Handle,
    pub message: u32,
    pub wparam: usize,
    pub lparam: isize,
    pub time: u32,
    pub point: [i32; 2],
    pub private: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[repr(C)]
pub struct Paint {
    pub dc: Handle,
    pub erase: i32,
    pub rect: Rect,
    pub restore: i32,
    pub inc_update: i32,
    pub reserved: [u8; 32],
}

#[repr(C)]
pub struct DrawItem {
    pub kind: u32,
    pub id: u32,
    pub item: u32,
    pub action: u32,
    pub state: u32,
    pub window: Handle,
    pub dc: Handle,
    pub rect: Rect,
    pub data: usize,
}

#[repr(C)]
pub struct MinMaxInfo {
    pub reserved: [i32; 2],
    pub max_size: [i32; 2],
    pub max_position: [i32; 2],
    pub min_track: [i32; 2],
    pub max_track: [i32; 2],
}

#[repr(C)]
pub struct Notification {
    pub window: Handle,
    pub id: usize,
    pub code: i32,
}

#[repr(C)]
pub struct ListNotification {
    pub header: Notification,
    pub item: i32,
    pub subitem: i32,
    pub new_state: u32,
    pub old_state: u32,
    pub changed: u32,
    pub point: [i32; 2],
    pub data: isize,
}

#[repr(C)]
pub struct ListItem {
    pub mask: u32,
    pub item: i32,
    pub subitem: i32,
    pub state: u32,
    pub state_mask: u32,
    pub text: *const u16,
    pub capacity: i32,
    pub image: i32,
    pub data: isize,
    pub indent: i32,
}

#[repr(C)]
pub struct ListColumn {
    pub mask: u32,
    pub format: i32,
    pub width: i32,
    pub text: *const u16,
    pub capacity: i32,
    pub subitem: i32,
    pub image: i32,
    pub order: i32,
}

#[link(name = "user32")]
extern "system" {
    pub fn RegisterClassW(class: *const WindowClass) -> u16;
    pub fn CreateWindowExW(
        ex: u32,
        class: *const u16,
        name: *const u16,
        style: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        parent: Handle,
        menu: Handle,
        instance: Handle,
        param: Handle,
    ) -> Handle;
    pub fn DefWindowProcW(window: Handle, message: u32, wparam: usize, lparam: isize) -> isize;
    pub fn LoadCursorW(instance: Handle, name: *const u16) -> Handle;
    pub fn LoadIconW(instance: Handle, name: *const u16) -> Handle;
    pub fn GetMessageW(message: *mut Message, window: Handle, min: u32, max: u32) -> i32;
    pub fn IsDialogMessageW(window: Handle, message: *mut Message) -> i32;
    pub fn TranslateMessage(message: *const Message) -> i32;
    pub fn DispatchMessageW(message: *const Message) -> isize;
    pub fn PostQuitMessage(code: i32);
    pub fn GetDlgItem(window: Handle, id: i32) -> Handle;
    pub fn GetWindowTextLengthW(window: Handle) -> i32;
    pub fn GetWindowTextW(window: Handle, text: *mut u16, capacity: i32) -> i32;
    pub fn SetWindowTextW(window: Handle, text: *const u16) -> i32;
    pub fn EnableWindow(window: Handle, enabled: i32) -> i32;
    pub fn SetFocus(window: Handle) -> Handle;
    pub fn SendMessageW(window: Handle, message: u32, wparam: usize, lparam: isize) -> isize;
    pub fn SetTimer(window: Handle, id: usize, interval: u32, callback: Handle) -> usize;
    pub fn KillTimer(window: Handle, id: usize) -> i32;
    pub fn MessageBoxW(window: Handle, text: *const u16, caption: *const u16, kind: u32) -> i32;
    pub fn MoveWindow(window: Handle, x: i32, y: i32, cx: i32, cy: i32, repaint: i32) -> i32;
    pub fn GetClientRect(window: Handle, rect: *mut Rect) -> i32;
    pub fn InvalidateRect(window: Handle, rect: *const Rect, erase: i32) -> i32;
    pub fn BeginPaint(window: Handle, paint: *mut Paint) -> Handle;
    pub fn EndPaint(window: Handle, paint: *const Paint) -> i32;
    pub fn FillRect(dc: Handle, rect: *const Rect, brush: Handle) -> i32;
    pub fn DrawTextW(
        dc: Handle,
        text: *const u16,
        length: i32,
        rect: *mut Rect,
        format: u32,
    ) -> i32;
    pub fn DrawFocusRect(dc: Handle, rect: *const Rect) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    pub fn GetModuleHandleW(name: *const u16) -> Handle;
    pub fn GetLogicalDrives() -> u32;
    pub fn GetDriveTypeW(root: *const u16) -> u32;
}

#[link(name = "comctl32")]
extern "system" {
    pub fn InitCommonControls();
    pub fn ImageList_Create(width: i32, height: i32, flags: u32, initial: i32, grow: i32)
        -> Handle;
    pub fn ImageList_AddMasked(list: Handle, bitmap: Handle, mask_color: u32) -> i32;
    pub fn ImageList_Destroy(list: Handle) -> i32;
}

#[link(name = "gdi32")]
extern "system" {
    pub fn GetStockObject(index: i32) -> Handle;
    pub fn CreateSolidBrush(color: u32) -> Handle;
    pub fn SetDCBrushColor(dc: Handle, color: u32) -> u32;
    pub fn SetTextColor(dc: Handle, color: u32) -> u32;
    pub fn SetBkMode(dc: Handle, mode: i32) -> i32;
    pub fn SelectObject(dc: Handle, object: Handle) -> Handle;
    pub fn DeleteObject(object: Handle) -> i32;
    pub fn DeleteDC(dc: Handle) -> i32;
    pub fn CreateCompatibleDC(dc: Handle) -> Handle;
    pub fn CreateBitmap(
        width: i32,
        height: i32,
        planes: u32,
        bits_per_pixel: u32,
        data: Handle,
    ) -> Handle;
    pub fn CreateFontW(
        height: i32,
        width: i32,
        escapement: i32,
        orientation: i32,
        weight: i32,
        italic: u32,
        underline: u32,
        strike: u32,
        charset: u32,
        output: u32,
        clip: u32,
        quality: u32,
        family: u32,
        face: *const u16,
    ) -> Handle;
}

pub const WM_SETFONT: u32 = 0x0030;
pub const CB_ADDSTRING: u32 = 0x0143;
pub const CB_GETCURSEL: u32 = 0x0147;
pub const CB_RESETCONTENT: u32 = 0x014b;
pub const CB_GETITEMDATA: u32 = 0x0150;
pub const CB_SETITEMDATA: u32 = 0x0151;
pub const LVM_FIRST: u32 = 0x1000;
pub const LVM_GETNEXTITEM: u32 = LVM_FIRST + 12;
pub const LVM_DELETEALLITEMS: u32 = LVM_FIRST + 9;
pub const LVM_SETIMAGELIST: u32 = LVM_FIRST + 3;
pub const LVM_SETCOLUMNWIDTH: u32 = LVM_FIRST + 30;
pub const LVM_SETITEMSTATE: u32 = LVM_FIRST + 43;
pub const LVM_SETEXTENDEDLISTVIEWSTYLE: u32 = LVM_FIRST + 54;
pub const LVM_SETITEMW: u32 = LVM_FIRST + 76;
pub const LVM_INSERTITEMW: u32 = LVM_FIRST + 77;
pub const LVM_INSERTCOLUMNW: u32 = LVM_FIRST + 97;
pub const LVIS_STATEIMAGEMASK: u32 = 0xf000;

pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}
