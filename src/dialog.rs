use crate::{control, win32::*, window_text};
use std::{
    ffi::{OsStr, OsString},
    os::windows::ffi::{OsStrExt, OsStringExt},
    ptr::null,
};

pub struct TextPrompt<'a> {
    pub title: &'a str,
    pub context_label: &'a str,
    pub context: &'a OsStr,
    pub input_label: &'a str,
    pub initial: &'a OsStr,
    pub submit: &'a str,
}

const OK: i32 = 1;
const CANCEL: i32 = 2;
const INPUT: i32 = 3;

pub unsafe fn text_prompt(owner: Handle, prompt: &TextPrompt) -> Option<OsString> {
    let template = EmptyDialogTemplate {
        style: 0x80c8_0800, // WS_POPUP | WS_CAPTION | WS_SYSMENU | DS_CENTER
        extended_style: 1,  // WS_EX_DLGMODALFRAME
        controls: 0,
        x: 0,
        y: 0,
        width: 300,
        height: 96,
        menu: 0,
        class: 0,
        title: 0,
    };
    let result = DialogBoxIndirectParamW(
        GetModuleHandleW(null()),
        &template,
        owner,
        procedure,
        prompt as *const TextPrompt as isize,
    );
    match result {
        0 | 2 => None,
        -1 => {
            MessageBoxW(
                owner,
                wide("Impossibile aprire la finestra.").as_ptr(),
                wide(prompt.title).as_ptr(),
                0x30,
            );
            None
        }
        // The submit handler passes ownership of the entered text back to the caller.
        value => Some(*Box::from_raw(value as *mut OsString)),
    }
}

unsafe extern "system" fn procedure(
    window: Handle,
    message: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    match message {
        0x110 => {
            // WM_INITDIALOG
            let prompt = &*(lparam as *const TextPrompt);
            SetWindowTextW(window, wide(prompt.title).as_ptr());
            let mut area = Rect::default();
            GetClientRect(window, &mut area);
            let width = area.right - 28;
            let bottom = area.bottom - 38;
            let label = |value, y| {
                let child = control(window, "STATIC", value, -1, 0x80, 0);
                MoveWindow(child, 14, y, width, 18, 0);
            };
            let edit = |id, value: &OsStr, y, readonly| {
                let style = 0x10080 | if readonly { 0x800 } else { 0 };
                let child = control(window, "EDIT", "", id, style, 0x200);
                let value: Vec<_> = value.encode_wide().chain([0]).collect();
                SetWindowTextW(child, value.as_ptr());
                MoveWindow(child, 14, y, width, 24, 0);
                child
            };
            label(prompt.context_label, 12);
            edit(4, prompt.context, 31, true);
            label(prompt.input_label, 65);
            let input = edit(INPUT, prompt.initial, 84, false);
            let submit = control(window, "BUTTON", prompt.submit, OK, 0x10001, 0);
            let cancel = control(window, "BUTTON", "Annulla", CANCEL, 0x10000, 0);
            MoveWindow(submit, area.right - 202, bottom, 90, 26, 0);
            MoveWindow(cancel, area.right - 104, bottom, 90, 26, 0);
            SendMessageW(window, 0x0401, OK as usize, 0); // DM_SETDEFID
            SendMessageW(input, 0x00b1, 0, -1); // EM_SETSEL
            SetFocus(input);
            0
        }
        0x111 if wparam == OK as usize => {
            let value = OsString::from_wide(&window_text(GetDlgItem(window, INPUT)));
            EndDialog(window, Box::into_raw(Box::new(value)) as isize);
            1
        }
        0x111 if wparam == CANCEL as usize => {
            EndDialog(window, 0);
            1
        }
        0x10 => {
            // WM_CLOSE
            EndDialog(window, 0);
            1
        }
        _ => 0,
    }
}
