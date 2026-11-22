#![windows_subsystem = "windows"]

mod copy;
mod disks;
mod theme;
mod win32;

use copy::{Entry, Operation};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    io,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    sync::mpsc::{self, Receiver, TryRecvError},
};
use theme::{Theme, FILES_TOP, FOOTER_HEIGHT, MARGIN};
use win32::*;

const SOURCE: i32 = 101;
const DESTINATION: i32 = 102;
const AUTO: i32 = 103;
const SHOW: i32 = 104;
const OPEN: i32 = 105;
const UP: i32 = 106;
const SELECT_ALL: i32 = 107;
const CLEAR: i32 = 108;
const FILES: i32 = 109;
const COPY: i32 = 110;
const MOVE: i32 = 111;
const HELP: i32 = 112;
const ADDRESS: i32 = 113;
const STATUS: i32 = 114;
const SELECTION: i32 = 115;
const SOURCE_LABEL: i32 = 116;
const DESTINATION_LABEL: i32 = 117;
const APP_ICON_RESOURCE: u16 = 101;
const MIN_WIDTH: i32 = 700;
const MIN_HEIGHT: i32 = 600;
const JOB_TIMER: usize = 1;
const DEVICE_TIMER: usize = 2;

#[derive(Default)]
struct Browser {
    root: PathBuf,
    directory: PathBuf,
    entries: Vec<Entry>,
    selected: BTreeSet<PathBuf>,
}

impl Browser {
    fn covered(&self, path: &Path) -> bool {
        path.ancestors().any(|part| self.selected.contains(part))
    }

    fn inside_selection(&self) -> bool {
        self.directory != self.root && self.covered(&self.directory)
    }

    fn check(&mut self, path: PathBuf) {
        self.selected.retain(|item| !item.starts_with(&path));
        self.selected.insert(path);
    }
}

struct Listing {
    root: PathBuf,
    directory: PathBuf,
    entries: Vec<Entry>,
}

impl Listing {
    fn read(root: PathBuf, directory: PathBuf) -> io::Result<Self> {
        let entries = copy::list(&root, &directory)?;
        Ok(Self {
            root,
            directory,
            entries,
        })
    }
}

enum TaskResult {
    Browse(io::Result<Listing>),
    Drives {
        result: io::Result<Vec<disks::Disk>>,
        choose: bool,
    },
    Transfer {
        message: String,
        refreshed: Option<Browser>,
    },
}

thread_local! {
    static UI: RefCell<Browser> = RefCell::new(Browser::default());
    static THEME: RefCell<Option<Theme>> = const { RefCell::new(None) };
    static JOB: RefCell<Option<Receiver<TaskResult>>> = const { RefCell::new(None) };
    static REDRAW_LIST: Cell<bool> = const { Cell::new(false) };
    static SETTING_PATHS: Cell<bool> = const { Cell::new(false) };
    static DISKS_WITHOUT_DRIVE_LETTERS: RefCell<Vec<disks::Disk>> = const { RefCell::new(Vec::new()) };
}

unsafe fn window_text(control: Handle) -> Vec<u16> {
    let mut buffer = vec![0; GetWindowTextLengthW(control).max(0) as usize + 1];
    let length = GetWindowTextW(control, buffer.as_mut_ptr(), buffer.len() as i32);
    buffer.truncate(length.max(0) as usize);
    buffer
}

unsafe fn field(window: Handle, id: i32) -> PathBuf {
    PathBuf::from(OsString::from_wide(&window_text(GetDlgItem(window, id))))
}

unsafe fn text(window: Handle, id: i32, value: &str) {
    SetWindowTextW(GetDlgItem(window, id), wide(value).as_ptr());
}

unsafe fn status(window: Handle, message: &str) {
    text(window, STATUS, message);
}

unsafe fn control(
    window: Handle,
    class: &str,
    label: &str,
    id: i32,
    style: u32,
    ex: u32,
) -> Handle {
    let control = CreateWindowExW(
        ex,
        wide(class).as_ptr(),
        wide(label).as_ptr(),
        0x5000_0000 | style,
        0,
        0,
        150,
        30,
        window,
        id as usize as Handle,
        GetModuleHandleW(null()),
        null_mut(),
    );
    let font = THEME.with_borrow(|theme| theme.as_ref().unwrap().body);
    SendMessageW(control, WM_SETFONT, font as usize, 1);
    control
}

unsafe fn setup(window: Handle) {
    THEME.with(|theme| theme.replace(Some(Theme::new())));
    const TAB: u32 = 0x10000;
    const DROPDOWN: u32 = TAB | 0x200_000 | 0x42;
    const BUTTON: u32 = TAB | 0xb;
    control(window, "BUTTON", "&Help · F1", HELP, BUTTON, 0);
    control(window, "STATIC", "&Origine", SOURCE_LABEL, 0x200, 0);
    let source = control(window, "COMBOBOX", "", SOURCE, DROPDOWN, 0);
    control(window, "BUTTON", "&Auto disco", AUTO, BUTTON, 0);
    control(
        window,
        "STATIC",
        "&Destinazione",
        DESTINATION_LABEL,
        0x200,
        0,
    );
    control(window, "COMBOBOX", "", DESTINATION, DROPDOWN, 0);
    control(window, "BUTTON", "&Mostra file", SHOW, BUTTON, 0);
    control(window, "BUTTON", "Apri", OPEN, BUTTON, 0);
    control(window, "BUTTON", "Su", UP, BUTTON, 0);
    control(window, "BUTTON", "Spunta tutti", SELECT_ALL, BUTTON, 0);
    control(window, "BUTTON", "Azzera spunte", CLEAR, BUTTON, 0);
    control(window, "STATIC", "", ADDRESS, 0x40c0, 0x200);
    let list = control(
        window,
        "SysListView32",
        "",
        FILES,
        TAB | 0x0040 | 0x0008 | 0x0004 | 1,
        0x200,
    );
    SendMessageW(list, LVM_SETEXTENDEDLISTVIEWSTYLE, 0, 0x0001_0024);
    let icons = THEME.with_borrow(|theme| theme.as_ref().unwrap().icons);
    if !icons.is_null() {
        SendMessageW(list, LVM_SETIMAGELIST, 1, icons as isize);
    }
    for (index, (label, width)) in [("Nome", 340), ("Tipo", 96), ("Dimensione", 121)]
        .into_iter()
        .enumerate()
    {
        let label = wide(label);
        let column = ListColumn {
            mask: 7,
            format: i32::from(index == 2),
            width,
            text: label.as_ptr(),
            capacity: 0,
            subitem: 0,
            image: 0,
            order: 0,
        };
        SendMessageW(list, LVM_INSERTCOLUMNW, index, &column as *const _ as isize);
    }
    control(window, "STATIC", "", SELECTION, 0x80, 0);
    control(window, "BUTTON", "&Copia tutto", COPY, BUTTON, 0);
    control(window, "BUTTON", "&Sposta selezione", MOVE, BUTTON, 0);
    control(
        window,
        "STATIC",
        "Scegli una cartella o un'unità di origine.",
        STATUS,
        0x80,
        0,
    );
    update_view(window);
    SetFocus(source);
    scan_drives(window, true);
}

unsafe fn layout(window: Handle) {
    let mut area = Rect::default();
    GetClientRect(window, &mut area);
    let width = area.right;
    let footer = area.bottom - FOOTER_HEIGHT;
    if width < 500 || footer < FILES_TOP + 126 {
        return;
    }
    let place = |id, x, y, w, h| MoveWindow(GetDlgItem(window, id), x, y, w, h, 1);
    place(HELP, width - 119, 36, 98, 34);
    place(SOURCE_LABEL, 30, 167, 90, 25);
    place(SOURCE, 123, 168, width - 281, 190);
    place(AUTO, width - 147, 165, 118, 29);
    place(DESTINATION_LABEL, 30, 201, 90, 25);
    place(DESTINATION, 123, 201, width - 152, 190);
    place(SHOW, 151, FILES_TOP + 8, 109, 29);
    place(OPEN, 268, FILES_TOP + 8, 66, 29);
    place(UP, 342, FILES_TOP + 8, 51, 29);
    place(SELECT_ALL, 401, FILES_TOP + 8, 101, 29);
    place(CLEAR, 510, FILES_TOP + 8, 112, 29);
    place(ADDRESS, 30, FILES_TOP + 46, width - 60, 23);
    place(
        FILES,
        30,
        FILES_TOP + 77,
        width - 60,
        footer - FILES_TOP - 115,
    );
    SendMessageW(
        GetDlgItem(window, FILES),
        LVM_SETCOLUMNWIDTH,
        0,
        (width - 302) as isize,
    );
    place(SELECTION, 30, footer - 31, width - 60, 18);
    place(MOVE, width - 174, footer + 11, 156, 33);
    place(COPY, width - 334, footer + 11, 151, 33);
    place(
        STATUS,
        MARGIN + 22,
        footer + 59,
        width - MARGIN * 2 - 32,
        35,
    );
    InvalidateRect(window, null(), 0);
}

unsafe fn scan_drives(window: Handle, choose: bool) {
    launch(
        window,
        choose.then_some("Rilevamento dei dischi in corso…"),
        move || TaskResult::Drives {
            result: disks::without_drive_letters(),
            choose,
        },
    );
}

fn disk_notice(source: &Path) -> Option<String> {
    DISKS_WITHOUT_DRIVE_LETTERS.with_borrow(|disks| {
        disks
            .iter()
            .find(|disk| source.as_os_str() == OsStr::new(&disk.label))
            .map(|disk| disk.notice.clone())
    })
}

unsafe fn selected_disk_notice(combo: Handle) -> Option<String> {
    let selection = SendMessageW(combo, CB_GETCURSEL, 0, 0);
    let data = SendMessageW(combo, CB_GETITEMDATA, usize::try_from(selection).ok()?, 0);
    let index = usize::try_from(data).ok()?.checked_sub(1)?;
    DISKS_WITHOUT_DRIVE_LETTERS.with_borrow(|disks| disks.get(index).map(|disk| disk.notice.clone()))
}

unsafe fn refresh_drives(
    window: Handle,
    choose: bool,
    result: io::Result<Vec<disks::Disk>>,
) {
    let source = field(window, SOURCE);
    let destination = field(window, DESTINATION);
    let failed = result.is_err();
    let disks = result.unwrap_or_default();
    let removed = disk_notice(&source).is_some()
        && !disks.iter().any(|disk| source.as_os_str() == OsStr::new(&disk.label));
    let mut drives = Vec::new();
    let mask = GetLogicalDrives();
    let system = std::env::var("SystemDrive").unwrap_or_else(|_| String::from("C:"));
    for index in 0..26 {
        if mask & (1 << index) != 0 {
            let root = format!("{}:\\", char::from(b'A' + index));
            let kind = GetDriveTypeW(wide(&root).as_ptr());
            drives.push((root, kind));
        }
    }
    let mut automatic = if choose {
        let destination_drive = destination.to_string_lossy();
        let mut candidates: Vec<_> = drives
            .iter()
            .filter(|(_, kind)| matches!(*kind, 2 | 3))
            .collect();
        candidates.sort_by_key(|(drive, kind)| {
            (
                *kind,
                drive.trim_end_matches('\\').eq_ignore_ascii_case(&system),
            )
        });
        candidates
            .into_iter()
            .map(|(drive, _)| drive)
            .find(|drive| {
                !destination_drive
                    .get(..2)
                    .is_some_and(|prefix| drive[..2].eq_ignore_ascii_case(prefix))
                    && std::fs::metadata(drive).is_ok_and(|metadata| metadata.is_dir())
            })
            .map(String::as_str)
    } else {
        None
    };
    if choose && automatic.is_none_or(|drive| drive.trim_end_matches('\\').eq_ignore_ascii_case(&system)) {
        if let Some(disk) = disks.iter().find(|disk| disk.usb) {
            automatic = Some(&disk.label);
        }
    }
    let new_source = match automatic {
        Some(drive) => PathBuf::from(drive),
        None if removed => PathBuf::new(),
        None => source.clone(),
    };
    SETTING_PATHS.set(true);
    for (id, path) in [(SOURCE, &new_source), (DESTINATION, &destination)] {
        let combo = GetDlgItem(window, id);
        SendMessageW(combo, CB_RESETCONTENT, 0, 0);
        for (drive, _) in &drives {
            SendMessageW(combo, CB_ADDSTRING, 0, wide(drive).as_ptr() as isize);
        }
        if id == SOURCE {
            for (index, disk) in disks.iter().enumerate() {
                let item = SendMessageW(combo, CB_ADDSTRING, 0, wide(&disk.label).as_ptr() as isize);
                if item >= 0 {
                    SendMessageW(combo, CB_SETITEMDATA, item as usize, (index + 1) as isize);
                }
            }
        }
        let value: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        SetWindowTextW(combo, value.as_ptr());
    }
    SETTING_PATHS.set(false);
    let automatic = automatic.is_some();
    DISKS_WITHOUT_DRIVE_LETTERS.with(|items| items.replace(disks));
    if source != new_source {
        UI.with(|ui| ui.replace(Browser::default()));
        update_view(window);
    }
    if failed && (choose || removed) {
        status(window, "Inventario dei dischi fisici non disponibile. Puoi usare le unità con lettera o digitare un percorso.");
    } else if let Some(notice) = disk_notice(&new_source) {
        status(window, &notice);
    } else if automatic {
        status(
            window,
            &format!(
                "Unità {} selezionata. Scegli la destinazione o premi Mostra file.",
                new_source.display()
            ),
        );
    } else if removed {
        status(window, "Il disco di origine è cambiato. Scegli di nuovo l'origine.");
    } else if choose {
        status(
            window,
            "Nessun disco idoneo trovato. Scegli un'unità dal menu o digita un percorso.",
        );
    }
}

unsafe fn busy() -> bool {
    JOB.with_borrow(Option::is_some)
}

unsafe fn launch(
    window: Handle,
    progress: Option<&str>,
    work: impl FnOnce() -> TaskResult + Send + 'static,
) {
    if busy() {
        return;
    }
    if SetTimer(window, JOB_TIMER, 70, null_mut()) == 0 {
        status(window, "Impossibile avviare l'operazione.");
        return;
    }
    let (sender, receiver) = mpsc::channel();
    if std::thread::Builder::new()
        .spawn(move || {
            let _ = sender.send(work());
        })
        .is_err()
    {
        KillTimer(window, JOB_TIMER);
        status(window, "Impossibile avviare l'operazione.");
        return;
    }
    JOB.with(|job| job.replace(Some(receiver)));
    update_controls(window);
    if let Some(progress) = progress {
        status(window, progress);
    }
}

unsafe fn show_files(window: Handle) {
    let root = field(window, SOURCE);
    if let Some(notice) = disk_notice(&root) {
        status(window, &notice);
        return;
    }
    if !root.is_absolute() {
        status(
            window,
            "Scegli un'unità o inserisci un percorso di origine assoluto.",
        );
        return;
    }
    let directory = UI.with_borrow(|ui| {
        if ui.root == root {
            ui.directory.clone()
        } else {
            root.clone()
        }
    });
    browse(window, root, directory);
}

unsafe fn browse(window: Handle, root: PathBuf, directory: PathBuf) {
    launch(window, Some("Lettura dei file in corso…"), move || {
        TaskResult::Browse(Listing::read(root, directory))
    });
}

unsafe fn highlighted(window: Handle) -> Option<usize> {
    let index = SendMessageW(GetDlgItem(window, FILES), LVM_GETNEXTITEM, usize::MAX, 2);
    usize::try_from(index).ok()
}

unsafe fn navigate(window: Handle, up: bool) {
    let index = highlighted(window);
    let target = UI.with_borrow(|ui| {
        let directory = if up && ui.directory != ui.root {
            ui.directory.parent().map(Path::to_path_buf)
        } else if !up {
            index
                .and_then(|index| ui.entries.get(index))
                .filter(|entry| entry.directory)
                .map(|entry| entry.path.clone())
        } else {
            None
        }?;
        Some((ui.root.clone(), directory))
    });
    if let Some((root, directory)) = target {
        browse(window, root, directory);
    }
}

unsafe fn start_transfer(window: Handle, operation: Operation) {
    if busy() {
        return;
    }
    let source = field(window, SOURCE);
    if let Some(notice) = disk_notice(&source) {
        status(window, &notice);
        return;
    }
    let destination = field(window, DESTINATION);
    if !source.is_absolute() || !destination.is_absolute() {
        status(
            window,
            "Indica due percorsi assoluti. La destinazione deve essere una cartella esistente.",
        );
        return;
    }
    let (directory, selected) = UI.with_borrow(|ui| {
        if ui.root == source {
            (
                ui.directory.clone(),
                ui.selected.iter().cloned().collect::<Vec<_>>(),
            )
        } else {
            (source.clone(), Vec::new())
        }
    });
    let moving = operation == Operation::Move;
    if moving {
        if selected.is_empty() {
            return;
        }
        let prompt = format!(
            "Spostare {} elementi selezionati? I file trasferiti verranno rimossi dall'origine.\r\n\r\nOrigine  {}\r\nDestinazione  {}",
            selected.len(), source.display(), destination.display()
        );
        if MessageBoxW(
            window,
            wide(&prompt).as_ptr(),
            wide("Conferma spostamento").as_ptr(),
            0x124,
        ) != 6
        {
            return;
        }
    }
    let progress = if moving {
        "Spostamento in corso…"
    } else {
        "Copia in corso…"
    };
    launch(window, Some(progress), move || {
        let result = if selected.is_empty() {
            copy::copy(&source, &destination)
        } else {
            copy::transfer_selection(&source, &destination, &selected, operation)
        };
        let message = match result {
            Ok(report) => format!(
                "{} completata. {} file ({} byte), {} {}. {} saltati.",
                if moving { "Operazione" } else { "Copia" },
                report.files,
                report.bytes,
                report.folders,
                if report.folders == 1 {
                    "cartella"
                } else {
                    "cartelle"
                },
                report.skipped,
            ),
            Err(error) => format!(
                "{} interrotta. {error}",
                if moving { "Operazione" } else { "Copia" }
            ),
        };
        let refreshed = moving.then(|| refresh_after_move(source, directory, selected));
        TaskResult::Transfer { message, refreshed }
    });
}

fn refresh_after_move(root: PathBuf, directory: PathBuf, selected: Vec<PathBuf>) -> Browser {
    let mut selected: BTreeSet<_> = selected.into_iter().collect();
    selected.retain(|path| {
        std::fs::symlink_metadata(path)
            .err()
            .is_none_or(|error| error.kind() != io::ErrorKind::NotFound)
    });
    for directory in directory
        .ancestors()
        .take_while(|path| path.starts_with(&root))
    {
        if let Ok(entries) = copy::list(&root, directory) {
            return Browser {
                directory: directory.into(),
                root,
                entries,
                selected,
            };
        }
    }
    Browser::default()
}

unsafe fn collect_task(window: Handle) {
    let result = JOB.with_borrow(|job| job.as_ref().map(Receiver::try_recv));
    match result {
        Some(Err(TryRecvError::Empty)) | None => return,
        _ => {}
    }
    JOB.with(|job| job.replace(None));
    KillTimer(window, JOB_TIMER);
    match result {
        Some(Ok(TaskResult::Drives { result, choose })) => {
            refresh_drives(window, choose, result);
        }
        Some(Ok(TaskResult::Browse(Ok(listing)))) => {
            let count = listing.entries.len();
            UI.with_borrow_mut(|ui| {
                if ui.root != listing.root {
                    ui.selected.clear();
                }
                ui.root = listing.root;
                ui.directory = listing.directory;
                ui.entries = listing.entries;
                let current = &ui.directory;
                let visible: BTreeSet<_> = ui.entries.iter().map(|entry| &entry.path).collect();
                ui.selected
                    .retain(|path| path.parent() != Some(current) || visible.contains(&path));
            });
            update_view(window);
            status(
                window,
                if count == 0 {
                    "La cartella è vuota."
                } else {
                    "Spunta i file o le cartelle da trasferire. Apri le cartelle con un doppio clic."
                },
            );
        }
        Some(Ok(TaskResult::Browse(Err(error)))) => {
            status(window, &format!("Impossibile aprire il percorso. {error}"))
        }
        Some(Ok(TaskResult::Transfer { message, refreshed })) => {
            if let Some(browser) = refreshed {
                UI.with(|ui| ui.replace(browser));
                update_view(window);
            }
            status(window, &message);
        }
        _ => status(window, "Operazione interrotta."),
    }
    update_controls(window);
}

unsafe fn update_view(window: Handle) {
    let list = GetDlgItem(window, FILES);
    let icons = THEME.with_borrow(|theme| !theme.as_ref().unwrap().icons.is_null());
    REDRAW_LIST.set(true);
    SendMessageW(list, 0x000b, 0, 0); // WM_SETREDRAW
    SendMessageW(list, LVM_DELETEALLITEMS, 0, 0);
    UI.with_borrow(|ui| {
        for (index, entry) in ui.entries.iter().enumerate() {
            let name: Vec<_> = entry
                .path
                .file_name()
                .unwrap_or_default()
                .encode_wide()
                .chain([0])
                .collect();
            let check = if ui.covered(&entry.path) { 2 } else { 1 };
            let mut item = ListItem {
                mask: if icons { 0xb } else { 9 },
                item: index as i32,
                subitem: 0,
                state: check << 12,
                state_mask: LVIS_STATEIMAGEMASK,
                text: name.as_ptr(),
                capacity: 0,
                image: i32::from(!entry.directory),
                data: 0,
                indent: 0,
            };
            if SendMessageW(list, LVM_INSERTITEMW, 0, &item as *const _ as isize) < 0 {
                break;
            }
            item.mask = 1;
            item.subitem = 1;
            let kind = wide(if entry.directory { "Cartella" } else { "File" });
            item.text = kind.as_ptr();
            SendMessageW(list, LVM_SETITEMW, 0, &item as *const _ as isize);
            if !entry.directory {
                item.subitem = 2;
                let size = wide(&format!("{} byte", entry.length));
                item.text = size.as_ptr();
                SendMessageW(list, LVM_SETITEMW, 0, &item as *const _ as isize);
            }
        }
    });
    update_checks(window);
    SendMessageW(list, 0x000b, 1, 0);
    REDRAW_LIST.set(false);
    InvalidateRect(list, null(), 1);
    let address = UI.with_borrow(|ui| {
        if ui.directory.as_os_str().is_empty() {
            String::from("Premi Mostra file per esplorare l'origine.")
        } else {
            ui.directory.display().to_string()
        }
    });
    text(window, ADDRESS, &address);
    update_controls(window);
}

unsafe fn update_checks(window: Handle) {
    let restoring = REDRAW_LIST.replace(true);
    let list = GetDlgItem(window, FILES);
    UI.with_borrow(|ui| {
        for (index, entry) in ui.entries.iter().enumerate() {
            let item = ListItem {
                mask: 8,
                item: index as i32,
                subitem: 0,
                state: (if ui.covered(&entry.path) { 2 } else { 1 }) << 12,
                state_mask: LVIS_STATEIMAGEMASK,
                text: null(),
                capacity: 0,
                image: 0,
                data: 0,
                indent: 0,
            };
            SendMessageW(list, LVM_SETITEMSTATE, index, &item as *const _ as isize);
        }
    });
    REDRAW_LIST.set(restoring);
}

unsafe fn update_controls(window: Handle) {
    let busy = busy();
    let highlighted = highlighted(window);
    let (count, visible, can_open, can_up, covered) = UI.with_borrow(|ui| {
        (
            ui.selected.len(),
            ui.entries.len(),
            highlighted
                .and_then(|i| ui.entries.get(i))
                .is_some_and(|entry| entry.directory),
            ui.directory != ui.root,
            ui.inside_selection(),
        )
    });
    for (id, enabled) in [
        (SOURCE, true),
        (DESTINATION, true),
        (AUTO, true),
        (SHOW, true),
        (FILES, true),
        (OPEN, can_open),
        (UP, can_up),
        (SELECT_ALL, visible > 0 && !covered),
        (CLEAR, count > 0),
        (COPY, true),
        (MOVE, count > 0),
    ] {
        EnableWindow(GetDlgItem(window, id), i32::from(enabled && !busy));
    }
    let summary = if covered {
        format!("{count} con spunta · Cartella già inclusa. Risali e togli la spunta per scegliere singoli file.")
    } else {
        format!("{visible} visibili  /  {count} con spunta")
    };
    text(window, SELECTION, &summary);
    text(
        window,
        COPY,
        if count == 0 {
            "&Copia tutto"
        } else {
            "&Copia selezione"
        },
    );
}

unsafe fn bulk_selection(window: Handle, select: bool) {
    UI.with_borrow_mut(|ui| {
        if !select {
            ui.selected.clear();
        } else if !ui.inside_selection() {
            let mut visible: BTreeSet<_> =
                ui.entries.iter().map(|entry| entry.path.clone()).collect();
            let directory = &ui.directory;
            ui.selected.retain(|item| {
                let Ok(relative) = item.strip_prefix(directory) else {
                    return true;
                };
                relative
                    .components()
                    .next()
                    .is_none_or(|part| !visible.contains(&directory.join(part.as_os_str())))
            });
            ui.selected.append(&mut visible);
        }
    });
    update_checks(window);
    update_controls(window);
}

unsafe fn list_notification(window: Handle, notification: &Notification) -> isize {
    if REDRAW_LIST.get() || busy() {
        return 0;
    }
    match notification.code {
        -3 => navigate(window, false), // NM_DBLCLK
        -100 | -101 => {
            let change = &*(notification as *const Notification as *const ListNotification);
            let old = change.old_state & LVIS_STATEIMAGEMASK;
            let new = change.new_state & LVIS_STATEIMAGEMASK;
            if old != new && new != 0 {
                if change.header.code == -100 {
                    return UI.with_borrow(Browser::inside_selection) as isize;
                }
                UI.with_borrow_mut(|ui| {
                    if let Some(entry) = ui.entries.get(change.item as usize) {
                        let path = entry.path.clone();
                        if new == 0x2000 {
                            ui.check(path);
                        } else {
                            ui.selected.remove(&path);
                        }
                    }
                });
            }
            if change.header.code == -101 {
                update_controls(window);
            }
        }
        _ => {}
    }
    0
}

unsafe fn help(window: Handle) {
    MessageBoxW(
        window,
        wide(concat!(
            "1. Scegli origine e destinazione dai menu oppure digita i percorsi. ",
            "Auto disco preferisce le unità rimovibili e lascia per ultima quella di sistema. ",
            "Il menu Origine mostra anche i dischi senza lettera e il loro stato.\r\n\r\n",
            "2. Premi Mostra file. Apri una cartella con un doppio clic o con Apri. Usa Su per risalire. ",
            "Le spunte restano attive quando cambi cartella. Spuntare una cartella include tutto il suo contenuto. ",
            "Azzera spunte deseleziona tutti gli elementi.\r\n\r\n",
            "3. Usa Copia selezione o Sposta selezione. Senza spunte, Copia tutto copia l'intera origine. ",
            "Le sottocartelle mantengono la struttura originale e i file già presenti vengono saltati.\r\n\r\n",
            "Esempi di percorso: D:\\Foto, E:\\Backup, \\\\wsl.localhost\\Ubuntu\\home\\utente",
        )).as_ptr(),
        wide("Guida · Disk Copy 98").as_ptr(),
        0x40,
    );
}

unsafe extern "system" fn procedure(
    window: Handle,
    message: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    match message {
        1 => setup(window),
        5 if wparam != 1 => layout(window),
        0x14 => return 1, // WM_ERASEBKGND
        0x24 => {
            let limits = &mut *(lparam as *mut MinMaxInfo);
            limits.min_track = [MIN_WIDTH, MIN_HEIGHT];
        }
        0x0f => {
            let mut paint: Paint = std::mem::zeroed();
            let dc = BeginPaint(window, &mut paint);
            let mut area = Rect::default();
            GetClientRect(window, &mut area);
            THEME.with_borrow(|theme| {
                if let Some(theme) = theme {
                    theme.paint(dc, area);
                }
            });
            EndPaint(window, &paint);
        }
        0x2b => {
            let draw = &*(lparam as *const DrawItem);
            THEME.with_borrow(|theme| theme.as_ref().unwrap().button(draw));
            return 1;
        }
        0x138 => {
            let control = lparam as Handle;
            let dc = wparam as Handle;
            SetTextColor(dc, theme::INK);
            SetBkMode(dc, 1);
            let brush = THEME.with_borrow(|theme| theme.as_ref().map(|theme| theme.panel));
            if control == GetDlgItem(window, ADDRESS) || control == GetDlgItem(window, STATUS) {
                SetTextColor(dc, theme::MUTED);
                SetDCBrushColor(dc, theme::rgb(249, 253, 255));
                return GetStockObject(18) as isize;
            }
            if let Some(brush) = brush {
                return brush as isize;
            }
        }
        0x111 => {
            let id = wparam as u16 as i32;
            let code = (wparam >> 16) as u16;
            if id == HELP && code == 0 {
                help(window);
            } else if !busy() {
                match (id, code) {
                    (SOURCE, 1 | 5) if !SETTING_PATHS.get() => {
                        UI.with(|ui| ui.replace(Browser::default()));
                        update_view(window);
                        let notice = if code == 1 {
                            selected_disk_notice(lparam as Handle)
                        } else {
                            disk_notice(&field(window, SOURCE))
                        };
                        status(window, notice.as_deref().unwrap_or(
                            "Origine aggiornata. Premi Mostra file per sfogliare.",
                        ));
                    }
                    (AUTO, 0) => scan_drives(window, true),
                    (SHOW, 0) => show_files(window),
                    (OPEN, 0) => navigate(window, false),
                    (UP, 0) => navigate(window, true),
                    (SELECT_ALL, 0) => bulk_selection(window, true),
                    (CLEAR, 0) => bulk_selection(window, false),
                    (COPY, 0) => start_transfer(window, Operation::Copy),
                    (MOVE, 0) => start_transfer(window, Operation::Move),
                    _ => {}
                }
            }
        }
        0x4e if wparam == FILES as usize && lparam != 0 => {
            return list_notification(window, &*(lparam as *const Notification));
        }
        0x113 if wparam == JOB_TIMER => collect_task(window),
        0x113 if wparam == DEVICE_TIMER => {
            if !busy() {
                KillTimer(window, DEVICE_TIMER);
                scan_drives(window, field(window, SOURCE).as_os_str().is_empty());
            }
        }
        0x219 if wparam == 7 || (matches!(wparam, 0x8000 | 0x8004) && lparam != 0 && *(lparam as *const u32).add(1) == 2) => {
            SetTimer(window, DEVICE_TIMER, 400, null_mut());
            return 1;
        }
        0x10 if busy() => {
            MessageBoxW(
                window,
                wide("Attendi il termine dell'operazione.").as_ptr(),
                wide("Disk Copy 98").as_ptr(),
                0x40,
            );
        }
        2 => PostQuitMessage(0),
        _ => return DefWindowProcW(window, message, wparam, lparam),
    }
    0
}

fn main() {
    unsafe {
        InitCommonControls();
        let instance = GetModuleHandleW(null());
        let title = wide("Disk Copy 98");
        let icon = LoadIconW(instance, APP_ICON_RESOURCE as usize as *const u16);
        let icon = if icon.is_null() {
            LoadIconW(null_mut(), 32512 as *const u16)
        } else {
            icon
        };
        let class = WindowClass {
            style: 0,
            procedure,
            class_extra: 0,
            window_extra: 0,
            instance,
            icon,
            cursor: LoadCursorW(null_mut(), 32512 as *const u16),
            background: null_mut(),
            menu: null(),
            name: title.as_ptr(),
        };
        if RegisterClassW(&class) == 0 {
            return;
        }
        let window = CreateWindowExW(
            0,
            title.as_ptr(),
            title.as_ptr(),
            0x12cf_0000,
            i32::MIN,
            i32::MIN,
            820,
            700,
            null_mut(),
            null_mut(),
            instance,
            null_mut(),
        );
        if window.is_null() {
            return;
        }
        let mut message: Message = std::mem::zeroed();
        while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
            if message.message == 0x100 && message.wparam == 0x70 {
                help(window);
            } else if IsDialogMessageW(window, &mut message) == 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
}
