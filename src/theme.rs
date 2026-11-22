use crate::{win32::*, AUTO, COPY, HELP, MOVE, SHOW};
use std::ptr::null_mut;

pub const MARGIN: i32 = 18;
pub const PATHS_TOP: i32 = 123;
pub const FILES_TOP: i32 = 249;
pub const FOOTER_HEIGHT: i32 = 132;

pub const INK: u32 = rgb(20, 52, 83);
pub const MUTED: u32 = rgb(56, 80, 105);
pub const PANEL: u32 = rgb(218, 236, 248);
const CANVAS: u32 = rgb(192, 218, 240);
const LINE: u32 = rgb(123, 160, 191);
const WHITE: u32 = rgb(249, 253, 255);

pub const fn rgb(red: u32, green: u32, blue: u32) -> u32 {
    red | (green << 8) | (blue << 16)
}

pub fn rect(x: i32, y: i32, width: i32, height: i32) -> Rect {
    Rect {
        left: x,
        top: y,
        right: x + width,
        bottom: y + height,
    }
}

impl Rect {
    fn inset(self, amount: i32) -> Self {
        rect(
            self.left + amount,
            self.top + amount,
            self.right - self.left - 2 * amount,
            self.bottom - self.top - 2 * amount,
        )
    }
}

pub struct Theme {
    pub body: Handle,
    bold: Handle,
    title: Handle,
    pub panel: Handle,
    pub icons: Handle,
}

unsafe fn font(size: i32, weight: i32) -> Handle {
    CreateFontW(
        -size,
        0,
        0,
        0,
        weight,
        0,
        0,
        0,
        1,
        0,
        0,
        0,
        0,
        wide("Tahoma").as_ptr(),
    )
}

impl Theme {
    pub unsafe fn new() -> Self {
        Self {
            body: font(14, 400),
            bold: font(14, 700),
            title: font(34, 700),
            panel: CreateSolidBrush(PANEL),
            icons: file_icons(),
        }
    }

    pub unsafe fn paint(&self, dc: Handle, area: Rect) {
        let width = area.right;
        let footer = area.bottom - FOOTER_HEIGHT;
        fill(dc, area, CANVAS);
        for row in 0..108 {
            let tint = (row * 31 / 108) as u32;
            fill(
                dc,
                rect(0, row, width, 1),
                rgb(13 + tint / 2, 48 + tint, 91 + tint),
            );
        }
        fill(dc, rect(0, 108, width, 3), rgb(109, 190, 243));
        fill(dc, rect(0, 111, width, 1), WHITE);
        drive(dc, 23, 23);
        label(
            dc,
            "Disk Copy 98",
            rect(98, 19, width - 224, 42),
            self.title,
            WHITE,
            0x24,
        );
        label(
            dc,
            "Trasferisci file e cartelle tra le unità",
            rect(100, 65, width - 225, 20),
            self.body,
            rgb(201, 231, 253),
            0x24,
        );

        panel(dc, rect(MARGIN, PATHS_TOP, width - 2 * MARGIN, 112));
        label(
            dc,
            "Unità e percorsi",
            rect(30, PATHS_TOP + 9, 220, 18),
            self.bold,
            INK,
            0x24,
        );
        fill(dc, rect(30, PATHS_TOP + 33, width - 60, 1), LINE);

        panel(
            dc,
            rect(
                MARGIN,
                FILES_TOP,
                width - 2 * MARGIN,
                footer - FILES_TOP - 10,
            ),
        );
        label(
            dc,
            "File e cartelle",
            rect(30, FILES_TOP + 7, 118, 30),
            self.bold,
            INK,
            0x24,
        );

        fill(dc, rect(0, footer, width, FOOTER_HEIGHT), PANEL);
        fill(dc, rect(0, footer, width, 1), LINE);
        fill(dc, rect(0, footer + 1, width, 1), WHITE);
        fill(
            dc,
            rect(MARGIN, footer + 72, width - 2 * MARGIN, FOOTER_HEIGHT - 81),
            LINE,
        );
        fill(
            dc,
            rect(
                MARGIN + 1,
                footer + 73,
                width - 2 * MARGIN - 2,
                FOOTER_HEIGHT - 83,
            ),
            WHITE,
        );
        fill(dc, rect(MARGIN + 9, footer + 84, 7, 7), rgb(54, 137, 191));
        SelectObject(dc, GetStockObject(17));
    }

    pub unsafe fn button(&self, draw: &DrawItem) {
        let pressed = draw.state & 1 != 0;
        let disabled = draw.state & 4 != 0;
        let (face, shade, text, font) = match (draw.id as i32, disabled) {
            (_, true) => (
                rgb(219, 231, 241),
                rgb(195, 212, 224),
                rgb(99, 118, 134),
                self.body,
            ),
            (COPY, _) => (rgb(31, 97, 168), rgb(20, 76, 143), WHITE, self.bold),
            (MOVE, _) => (rgb(255, 223, 141), rgb(238, 193, 95), INK, self.bold),
            (AUTO | SHOW | HELP, _) => (rgb(181, 222, 249), rgb(148, 197, 233), INK, self.bold),
            _ => (rgb(228, 242, 250), rgb(201, 222, 239), INK, self.body),
        };
        let area = draw.rect;
        let inner = area.inset(1);
        fill(draw.dc, area, if disabled { LINE } else { INK });
        fill(draw.dc, inner, if pressed { shade } else { WHITE });
        fill(draw.dc, inner.inset(1), if pressed { shade } else { face });
        if !pressed {
            fill(
                draw.dc,
                rect(
                    inner.left + 1,
                    inner.bottom - 7,
                    inner.right - inner.left - 2,
                    6,
                ),
                shade,
            );
        }
        let shift = i32::from(pressed);
        let name = crate::window_text(draw.window);
        let mut text_area = area.inset(3);
        text_area.left += shift;
        text_area.top += shift;
        label_wide(draw.dc, &name, text_area, font, text, 0x25);
        if draw.state & (0x10 | 0x200) == 0x10 {
            DrawFocusRect(draw.dc, &area.inset(4));
        }
        SelectObject(draw.dc, GetStockObject(17));
    }
}

impl Drop for Theme {
    fn drop(&mut self) {
        unsafe {
            for object in [self.body, self.bold, self.title, self.panel] {
                DeleteObject(object);
            }
            if !self.icons.is_null() {
                ImageList_Destroy(self.icons);
            }
        }
    }
}

unsafe fn fill(dc: Handle, area: Rect, color: u32) {
    SetDCBrushColor(dc, color);
    FillRect(dc, &area, GetStockObject(18));
}

unsafe fn panel(dc: Handle, area: Rect) {
    fill(dc, area, LINE);
    fill(dc, area.inset(1), WHITE);
    fill(dc, area.inset(2), PANEL);
}

unsafe fn label(dc: Handle, text: &str, area: Rect, font: Handle, color: u32, format: u32) {
    let mut text = wide(text);
    text.pop();
    label_wide(dc, &text, area, font, color, format);
}

unsafe fn label_wide(
    dc: Handle,
    text: &[u16],
    mut area: Rect,
    font: Handle,
    color: u32,
    format: u32,
) {
    SelectObject(dc, font);
    SetTextColor(dc, color);
    SetBkMode(dc, 1);
    DrawTextW(dc, text.as_ptr(), text.len() as i32, &mut area, format);
}

unsafe fn drive(dc: Handle, x: i32, y: i32) {
    fill(dc, rect(x, y, 59, 59), rgb(9, 39, 75));
    fill(dc, rect(x + 1, y + 1, 56, 56), rgb(151, 218, 255));
    fill(dc, rect(x + 3, y + 3, 52, 52), rgb(58, 120, 177));
    fill(dc, rect(x + 11, y + 13, 37, 33), rgb(14, 48, 77));
    fill(dc, rect(x + 12, y + 14, 34, 29), rgb(232, 243, 255));
    fill(dc, rect(x + 14, y + 16, 30, 14), rgb(165, 194, 218));
    fill(dc, rect(x + 14, y + 32, 30, 9), rgb(123, 155, 183));
    fill(dc, rect(x + 18, y + 34, 13, 4), rgb(26, 57, 86));
    fill(dc, rect(x + 37, y + 34, 4, 4), rgb(160, 247, 166));
}

unsafe fn file_icons() -> Handle {
    let list = ImageList_Create(16, 16, 0x19, 2, 0);
    if list.is_null() {
        return list;
    }
    let dc = CreateCompatibleDC(null_mut());
    let bitmap = CreateBitmap(16, 16, 1, 32, null_mut());
    if dc.is_null() || bitmap.is_null() {
        if !dc.is_null() {
            DeleteDC(dc);
        }
        if !bitmap.is_null() {
            DeleteObject(bitmap);
        }
        ImageList_Destroy(list);
        return null_mut();
    }
    let old = SelectObject(dc, bitmap);
    let mask = rgb(255, 0, 255);
    fill(dc, rect(0, 0, 16, 16), mask);
    fill(dc, rect(1, 3, 7, 4), rgb(145, 102, 30));
    fill(dc, rect(2, 4, 5, 3), rgb(255, 204, 103));
    fill(dc, rect(1, 6, 14, 8), rgb(145, 102, 30));
    fill(dc, rect(2, 7, 12, 6), rgb(255, 220, 134));
    fill(dc, rect(2, 7, 12, 1), rgb(255, 241, 193));
    SelectObject(dc, old);
    ImageList_AddMasked(list, bitmap, mask);

    SelectObject(dc, bitmap);
    fill(dc, rect(0, 0, 16, 16), mask);
    fill(dc, rect(3, 1, 8, 14), rgb(64, 109, 146));
    fill(dc, rect(3, 4, 10, 11), rgb(64, 109, 146));
    fill(dc, rect(4, 2, 6, 12), WHITE);
    fill(dc, rect(4, 5, 8, 9), WHITE);
    fill(dc, rect(6, 7, 4, 1), rgb(110, 165, 207));
    fill(dc, rect(6, 10, 4, 1), rgb(110, 165, 207));
    SelectObject(dc, old);
    ImageList_AddMasked(list, bitmap, mask);
    DeleteObject(bitmap);
    DeleteDC(dc);
    if ImageList_Count(list) == 2 {
        list
    } else {
        ImageList_Destroy(list);
        null_mut()
    }
}

#[link(name = "comctl32")]
extern "system" {
    #[link_name = "ImageList_GetImageCount"]
    fn ImageList_Count(list: Handle) -> i32;
}
