#![windows_subsystem = "windows"]

mod copy;
mod dialog;
mod disks;
mod linux;
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
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
        Arc, Mutex,
    },
    thread::JoinHandle,
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
const EJECT: i32 = 118;
const PROGRESS: i32 = 119;
const KEEP_BOTH: i32 = 120;
const DESTINATION_SHOW: i32 = 121;
const DESTINATION_OPEN: i32 = 122;
const DESTINATION_UP: i32 = 123;
const DESTINATION_ADDRESS: i32 = 124;
const DESTINATION_FILES: i32 = 125;
const SOURCE_ACTIONS: i32 = 126;
const DESTINATION_ACTIONS: i32 = 130;
const APP_ICON_RESOURCE: u16 = 101;
const MIN_WIDTH: i32 = 920;
const MIN_HEIGHT: i32 = 600;
const JOB_TIMER: usize = 1;
const DEVICE_TIMER: usize = 2;
const SELECTION_SIZE_TIMER: usize = 3;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pane {
    Source,
    Destination,
}

struct PaneControls {
    label: i32,
    path: i32,
    show: i32,
    open: i32,
    up: i32,
    address: i32,
    files: i32,
    actions: i32,
}

impl Pane {
    fn controls(self) -> PaneControls {
        match self {
            Self::Source => PaneControls {
                label: SOURCE_LABEL,
                path: SOURCE,
                show: SHOW,
                open: OPEN,
                up: UP,
                address: ADDRESS,
                files: FILES,
                actions: SOURCE_ACTIONS,
            },
            Self::Destination => PaneControls {
                label: DESTINATION_LABEL,
                path: DESTINATION,
                show: DESTINATION_SHOW,
                open: DESTINATION_OPEN,
                up: DESTINATION_UP,
                address: DESTINATION_ADDRESS,
                files: DESTINATION_FILES,
                actions: DESTINATION_ACTIONS,
            },
        }
    }

    fn with_listing<T>(self, read: impl FnOnce(&Listing) -> T) -> T {
        match self {
            Self::Source => UI.with_borrow(|ui| read(&ui.listing)),
            Self::Destination => DESTINATION_VIEW.with_borrow(read),
        }
    }

    fn opposite(self) -> Self {
        match self {
            Self::Source => Self::Destination,
            Self::Destination => Self::Source,
        }
    }
}

#[derive(Clone, Copy)]
enum FileAction {
    NewFile,
    NewFolder,
    Move,
    Delete,
}

const FILE_ACTIONS: [(FileAction, &str, i32); 4] = [
    (FileAction::NewFile, "Nuovo file…", 88),
    (FileAction::NewFolder, "Nuova cartella…", 111),
    (FileAction::Move, "Sposta…", 74),
    (FileAction::Delete, "Elimina…", 74),
];

enum FileChange {
    Create {
        name: OsString,
        folder: bool,
    },
    Move {
        item: PathBuf,
        folder: bool,
        destination: PathBuf,
    },
    Delete {
        item: PathBuf,
        folder: bool,
    },
}

#[derive(Default)]
enum SelectionSize {
    #[default]
    Pending,
    Measured(copy::SizeEstimate),
    Unavailable,
}

#[derive(Default)]
struct Browser {
    listing: Listing,
    selected: BTreeSet<PathBuf>,
    selection_size: SelectionSize,
}

impl Browser {
    fn covered(&self, path: &Path) -> bool {
        path.ancestors().any(|part| self.selected.contains(part))
    }

    fn inside_selection(&self) -> bool {
        self.listing.can_go_up() && self.covered(&self.listing.directory)
    }

    fn check(&mut self, path: PathBuf) {
        self.selected.retain(|item| !item.starts_with(&path));
        self.selected.insert(path);
    }

    fn selection_summary(&self) -> String {
        let visible = self.listing.entries.len();
        let count = self.selected.len();
        if count == 0 {
            return format!("{visible} visibili  /  0 con spunta");
        }
        let size = match &self.selection_size {
            SelectionSize::Pending => "Calcolo dimensione…".into(),
            SelectionSize::Measured(size) if !size.incomplete => {
                display_selection_bytes(size.bytes)
            }
            SelectionSize::Measured(size) if size.bytes > 0 => {
                format!("{} (parziale)", display_selection_bytes(size.bytes))
            }
            _ => "Dimensione non disponibile".into(),
        };
        if self.inside_selection() {
            format!("{count} con spunta · {size} · Cartella già inclusa. Risali per modificare le spunte.")
        } else {
            format!("{visible} visibili  /  {count} con spunta · {size}")
        }
    }
}

#[derive(Default)]
struct Listing {
    root: PathBuf,
    directory: PathBuf,
    entries: Vec<Entry>,
}

impl Listing {
    fn read(pane: Pane, root: PathBuf, directory: PathBuf) -> io::Result<Self> {
        let entries = copy::list(&root, &directory, pane == Pane::Source)?;
        Ok(Self {
            root,
            directory,
            entries,
        })
    }

    fn can_go_up(&self) -> bool {
        self.directory != self.root
    }

    fn selected_folder(&self, index: Option<usize>) -> Option<&Path> {
        let entry = self.entries.get(index?)?;
        entry.directory.then_some(entry.path.as_path())
    }

    fn current_folder<'a>(&'a self, input: &'a Path) -> &'a Path {
        if self.root == input {
            &self.directory
        } else {
            input
        }
    }

    fn is_folder(&self) -> bool {
        self.directory.is_absolute()
            && !self
                .entries
                .iter()
                .any(|entry| entry.path == self.directory)
    }

    fn refresh(pane: Pane, root: PathBuf, directory: PathBuf) -> Self {
        if root.as_os_str().is_empty() {
            return Self::default();
        }
        for directory in directory
            .ancestors()
            .take_while(|path| path.starts_with(&root))
        {
            if let Ok(entries) = copy::list(&root, directory, pane == Pane::Source) {
                return Self {
                    directory: directory.into(),
                    root,
                    entries,
                };
            }
        }
        Self::default()
    }
}

enum TaskResult {
    Browse(Pane, io::Result<Listing>),
    Drives {
        result: io::Result<disks::Inventory>,
        choose: bool,
    },
    Mount {
        result: io::Result<linux::MountedDisk>,
        pane: Pane,
    },
    Eject {
        remaining: Vec<linux::MountedDisk>,
        error: Option<String>,
    },
    Transfer {
        message: String,
        details: Option<String>,
        finished: bool,
        source: Option<Browser>,
        destination: Option<Listing>,
    },
    FileChange {
        result: io::Result<String>,
        source: Browser,
        destination: Listing,
    },
}

type TransferProgress = Arc<Mutex<Option<copy::Progress>>>;
type SizeTask = (Arc<AtomicBool>, JoinHandle<copy::SizeEstimate>);

thread_local! {
    static UI: RefCell<Browser> = RefCell::new(Browser::default());
    static DESTINATION_VIEW: RefCell<Listing> = RefCell::new(Listing::default());
    static THEME: RefCell<Option<Theme>> = const { RefCell::new(None) };
    static JOB: RefCell<Option<Receiver<TaskResult>>> = const { RefCell::new(None) };
    static EJECT_EXIT: Cell<Option<bool>> = const { Cell::new(None) };
    static SIZE_TASK: RefCell<Option<SizeTask>> = const { RefCell::new(None) };
    static ACTIVE_PROGRESS: RefCell<Option<(Operation, TransferProgress)>> = const { RefCell::new(None) };
    static REDRAW_LIST: Cell<bool> = const { Cell::new(false) };
    static FILE_PROMPT_OPEN: Cell<bool> = const { Cell::new(false) };
    static SETTING_PATHS: Cell<bool> = const { Cell::new(false) };
    static DISKS_WITHOUT_DRIVE_LETTERS: RefCell<Vec<disks::Disk>> = const { RefCell::new(Vec::new()) };
    static MOUNTED_DISKS: RefCell<Vec<linux::MountedDisk>> = const { RefCell::new(Vec::new()) };
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
    for (pane, heading, tool, tool_label, browse_label) in [
        (Pane::Source, "&Origine", AUTO, "&Auto disco", "&Sfoglia"),
        (
            Pane::Destination,
            "&Destinazione",
            EJECT,
            "Espelli WSL",
            "S&foglia",
        ),
    ] {
        let ids = pane.controls();
        let label = control(window, "STATIC", heading, ids.label, 0x200, 0);
        let (bold, icons) = THEME.with_borrow(|theme| {
            let theme = theme.as_ref().unwrap();
            (theme.bold, theme.icons)
        });
        SendMessageW(label, WM_SETFONT, bold as usize, 1);
        control(window, "COMBOBOX", "", ids.path, DROPDOWN, 0);
        control(window, "BUTTON", tool_label, tool, BUTTON, 0);
        control(window, "BUTTON", browse_label, ids.show, BUTTON, 0);
        control(window, "BUTTON", "Apri", ids.open, BUTTON, 0);
        control(window, "BUTTON", "Su", ids.up, BUTTON, 0);
        if pane == Pane::Source {
            control(window, "BUTTON", "Spunta tutti", SELECT_ALL, BUTTON, 0);
            control(window, "BUTTON", "Azzera spunte", CLEAR, BUTTON, 0);
        }
        for (action, label, _) in FILE_ACTIONS {
            control(
                window,
                "BUTTON",
                label,
                ids.actions + action as i32,
                BUTTON,
                0,
            );
        }
        control(window, "STATIC", "", ids.address, 0x80c0, 0x200);
        let list = control(
            window,
            "SysListView32",
            "",
            ids.files,
            TAB | 0x0040 | 0x0008 | 0x0004 | 1,
            0x200,
        );
        let checks = if pane == Pane::Source { 4 } else { 0 };
        SendMessageW(list, LVM_SETEXTENDEDLISTVIEWSTYLE, 0, 0x0001_0020 | checks);
        if !icons.is_null() {
            SendMessageW(list, LVM_SETIMAGELIST, 1, icons as isize);
        }
        for (index, (label, width)) in [("Nome", 210), ("Tipo", 75), ("Dimensione", 104)]
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
    }
    control(window, "STATIC", "", SELECTION, 0x80, 0);
    control(window, "BUTTON", "&Copia tutto", COPY, BUTTON, 0);
    control(window, "BUTTON", "&Sposta selezione", MOVE, BUTTON, 0);
    control(
        window,
        "BUTTON",
        "&Mantieni entrambi se il nome esiste già",
        KEEP_BOTH,
        TAB | 3,
        0,
    );
    let progress = control(window, "msctls_progress32", "", PROGRESS, 1, 0);
    SendMessageW(
        progress,
        PBM_SETBARCOLOR,
        0,
        theme::rgb(38, 119, 184) as isize,
    );
    ShowWindow(progress, 0);
    control(
        window,
        "STATIC",
        "Scegli una cartella o un'unità di origine.",
        STATUS,
        0x80,
        0,
    );
    update_view(window, Pane::Source);
    update_view(window, Pane::Destination);
    SetFocus(GetDlgItem(window, SOURCE));
    scan_drives(window, true);
}

unsafe fn layout(window: Handle) {
    let mut area = Rect::default();
    GetClientRect(window, &mut area);
    let width = area.right;
    let footer = area.bottom - FOOTER_HEIGHT;
    if width < 870 || footer < FILES_TOP + 200 {
        return;
    }
    let place = |id, x, y, w, h| MoveWindow(GetDlgItem(window, id), x, y, w, h, 1);
    place(HELP, width - 119, 36, 98, 34);
    let [left, right] = theme::file_panels(width, footer);
    for (pane, bounds, tool) in [
        (Pane::Source, left, AUTO),
        (Pane::Destination, right, EJECT),
    ] {
        let ids = pane.controls();
        let x = bounds.left + 12;
        let end = bounds.right - 12;
        let inner = end - x;
        place(ids.label, x, FILES_TOP + 9, inner - 112, 25);
        place(tool, end - 106, FILES_TOP + 8, 106, 26);
        place(ids.path, x, FILES_TOP + 40, inner, 200);
        place(ids.show, x, FILES_TOP + 71, 78, 27);
        place(ids.open, x + 85, FILES_TOP + 71, 48, 27);
        place(ids.up, x + 140, FILES_TOP + 71, 40, 27);
        let mut action_x = x;
        for (action, _, width) in FILE_ACTIONS {
            place(
                ids.actions + action as i32,
                action_x,
                FILES_TOP + 105,
                width,
                27,
            );
            action_x += width + 7;
        }
        place(ids.address, x, FILES_TOP + 140, inner, 23);
        let list_bottom = if pane == Pane::Source {
            footer - 59
        } else {
            footer - 22
        };
        place(
            ids.files,
            x,
            FILES_TOP + 171,
            inner,
            list_bottom - FILES_TOP - 171,
        );
        SendMessageW(
            GetDlgItem(window, ids.files),
            LVM_SETCOLUMNWIDTH,
            0,
            (inner - 201) as isize,
        );
        if pane == Pane::Source {
            place(SELECT_ALL, x + 187, FILES_TOP + 71, 90, 27);
            place(CLEAR, x + 284, FILES_TOP + 71, 102, 27);
            place(SELECTION, x, footer - 52, inner, 34);
        }
    }
    place(MOVE, width - 174, footer + 11, 156, 33);
    place(COPY, width - 334, footer + 11, 151, 33);
    place(KEEP_BOTH, MARGIN + 2, footer + 11, width - 365, 33);
    place(PROGRESS, MARGIN, footer + 50, width - 2 * MARGIN, 15);
    place(
        STATUS,
        MARGIN + 22,
        footer + 78,
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
            result: disks::scan(),
            choose,
        },
    );
}

fn physical_disk(path: &Path) -> Option<disks::Disk> {
    DISKS_WITHOUT_DRIVE_LETTERS.with_borrow(|disks| {
        disks
            .iter()
            .find(|disk| path.as_os_str() == OsStr::new(&disk.label))
            .cloned()
    })
}

unsafe fn add_disk_item(combo: Handle, label: &str, data: usize) {
    let item = SendMessageW(combo, CB_ADDSTRING, 0, wide(label).as_ptr() as isize);
    if item >= 0 {
        SendMessageW(combo, CB_SETITEMDATA, item as usize, data as isize);
    }
}

unsafe fn add_volumes(window: Handle, first: usize) {
    let physical = DISKS_WITHOUT_DRIVE_LETTERS.with_borrow(Vec::len);
    MOUNTED_DISKS.with_borrow(|disks| {
        for (index, volume) in disks
            .iter()
            .flat_map(|disk| &disk.volumes)
            .enumerate()
            .skip(first)
        {
            add_disk_item(
                GetDlgItem(window, SOURCE),
                &volume.label,
                physical + index + 1,
            );
            if !volume.readonly {
                add_disk_item(
                    GetDlgItem(window, DESTINATION),
                    &volume.label,
                    physical + index + 1,
                );
            }
        }
    });
}

unsafe fn set_path(window: Handle, pane: Pane, path: &Path) {
    write_path(window, pane, path);
    clear_pane(window, pane);
}

unsafe fn write_path(window: Handle, pane: Pane, path: &Path) {
    SETTING_PATHS.set(true);
    let value: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    SetWindowTextW(GetDlgItem(window, pane.controls().path), value.as_ptr());
    SETTING_PATHS.set(false);
}

unsafe fn clear_pane(window: Handle, pane: Pane) {
    match pane {
        Pane::Source => replace_browser(window, Browser::default()),
        Pane::Destination => {
            DESTINATION_VIEW.with(|view| view.replace(Listing::default()));
            update_view(window, pane);
        }
    }
}

unsafe fn refresh_drives(window: Handle, choose: bool, result: io::Result<disks::Inventory>) {
    let source = field(window, SOURCE);
    let destination = field(window, DESTINATION);
    let failed = result.is_err();
    let disks::Inventory {
        without_drive_letters: disks,
        usb_drive_letters,
    } = result.unwrap_or_else(|_| disks::Inventory {
        without_drive_letters: DISKS_WITHOUT_DRIVE_LETTERS.with_borrow(Clone::clone),
        usb_drive_letters: String::new(),
    });
    let removed = !failed
        && physical_disk(&source).is_some()
        && !disks
            .iter()
            .any(|disk| source.as_os_str() == OsStr::new(&disk.label));
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
    let external = |drive: &str, kind| kind == 2 || usb_drive_letters.contains(&drive[..1]);
    let mounted = if choose {
        let destination_drive = destination.to_string_lossy();
        let mut candidates: Vec<_> = drives
            .iter()
            .filter(|(_, kind)| matches!(*kind, 2 | 3))
            .collect();
        candidates.sort_by_key(|(drive, kind)| {
            (
                !external(drive, *kind),
                *kind,
                drive.trim_end_matches('\\').eq_ignore_ascii_case(&system),
            )
        });
        candidates.into_iter().find(|(drive, _)| {
            !destination_drive
                .get(..2)
                .is_some_and(|prefix| drive[..2].eq_ignore_ascii_case(prefix))
                && std::fs::metadata(drive).is_ok_and(|metadata| metadata.is_dir())
        })
    } else {
        None
    };
    let detected_usb = if choose && mounted.is_none_or(|(drive, kind)| !external(drive, *kind)) {
        disks.iter().find(|disk| disk.usb && disk.mountable)
    } else {
        None
    };
    let linux_path = MOUNTED_DISKS.with_borrow(|disks| {
        disks
            .iter()
            .flat_map(|disk| &disk.volumes)
            .find(|volume| !destination.starts_with(&volume.path))
            .map(|volume| volume.path.clone())
    });
    let automatic = detected_usb
        .map(|disk| PathBuf::from(&disk.label))
        .or_else(|| {
            mounted
                .filter(|(drive, kind)| external(drive, *kind))
                .map(|(drive, _)| PathBuf::from(drive))
        })
        .or_else(|| choose.then_some(linux_path).flatten())
        .or_else(|| mounted.map(|(drive, _)| PathBuf::from(drive)));
    let new_source = match automatic {
        Some(ref drive) => drive.clone(),
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
        for (index, disk) in disks.iter().enumerate() {
            if id == SOURCE || disk.mountable {
                add_disk_item(combo, &disk.label, index + 1);
            }
        }
        let value: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        SetWindowTextW(combo, value.as_ptr());
    }
    let automatic = automatic.is_some();
    DISKS_WITHOUT_DRIVE_LETTERS.with(|items| items.replace(disks));
    add_volumes(window, 0);
    SETTING_PATHS.set(false);
    if source != new_source {
        clear_pane(window, Pane::Source);
    }
    if failed && (choose || removed) {
        status(window, "Inventario dei dischi fisici non disponibile. Puoi usare le unità con lettera o digitare un percorso.");
    } else if let Some(disk) = physical_disk(&new_source) {
        if choose || source != new_source {
            status(window, &disk.notice);
        }
    } else if automatic {
        show_files(window, Pane::Source);
    } else if removed {
        status(
            window,
            "Il disco di origine è cambiato. Scegli di nuovo l'origine.",
        );
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

unsafe fn show_files(window: Handle, pane: Pane) {
    let root = field(window, pane.controls().path);
    if let Some(disk) = physical_disk(&root) {
        mount_disk(window, pane, disk);
        return;
    }
    if !root.is_absolute() {
        status(
            window,
            match pane {
                Pane::Source => "Scegli un'unità di origine o inserisci un percorso assoluto.",
                Pane::Destination => {
                    "Scegli un'unità di destinazione o inserisci un percorso assoluto."
                }
            },
        );
        return;
    }
    let directory = pane.with_listing(|view| view.current_folder(&root).to_path_buf());
    browse(window, pane, root, directory);
}

unsafe fn mount_disk(window: Handle, pane: Pane, disk: disks::Disk) {
    if !disk.mountable {
        status(window, &disk.notice);
        return;
    }
    launch(
        window,
        Some("Collegamento del disco a WSL. Conferma eventuali richieste di Windows…"),
        move || TaskResult::Mount {
            result: linux::MountedDisk::open(disk),
            pane,
        },
    );
}

unsafe fn choose_disk_item(window: Handle, pane: Pane, combo: Handle) {
    let Ok(index) = usize::try_from(SendMessageW(combo, CB_GETCURSEL, 0, 0)) else {
        return;
    };
    let Ok(item) = usize::try_from(SendMessageW(combo, CB_GETITEMDATA, index, 0)) else {
        return;
    };
    if item == 0 {
        let length = SendMessageW(combo, CB_GETLBTEXTLEN, index, 0);
        if length > 0 {
            let mut value = vec![0u16; length as usize + 1];
            let length = SendMessageW(combo, CB_GETLBTEXT, index, value.as_mut_ptr() as isize);
            if length > 0 {
                let path = PathBuf::from(OsString::from_wide(&value[..length as usize]));
                open_volume(window, pane, path);
            }
        }
        return;
    }
    let physical = DISKS_WITHOUT_DRIVE_LETTERS.with_borrow(Vec::len);
    if item <= physical {
        let disk = DISKS_WITHOUT_DRIVE_LETTERS.with_borrow(|disks| disks[item - 1].clone());
        mount_disk(window, pane, disk);
    } else {
        let path = MOUNTED_DISKS.with_borrow(|disks| {
            disks
                .iter()
                .flat_map(|disk| &disk.volumes)
                .nth(item - physical - 1)
                .map(|volume| volume.path.clone())
        });
        if let Some(path) = path {
            open_volume(window, pane, path);
        }
    }
}

unsafe fn open_volume(window: Handle, pane: Pane, path: PathBuf) {
    set_path(window, pane, &path);
    browse(window, pane, path.clone(), path);
}

unsafe fn eject_linux(window: Handle, exit: bool) {
    let disks = MOUNTED_DISKS.with(|disks| disks.take());
    if UI.with_borrow(|ui| {
        disks
            .iter()
            .flat_map(|disk| &disk.volumes)
            .any(|volume| ui.listing.root.starts_with(&volume.path))
    }) {
        queue_selection_size(window);
    }
    launch(
        window,
        Some("Smontaggio sicuro dei dischi Linux in corso…"),
        move || {
            let mut remaining = Vec::new();
            let mut error = None;
            for mut disk in disks {
                if let Err(problem) = disk.close() {
                    error = Some(format!("{} · {problem}", disk.label));
                    remaining.push(disk);
                }
            }
            TaskResult::Eject { remaining, error }
        },
    );
    if busy() {
        EJECT_EXIT.set(Some(exit));
    } else if exit {
        DestroyWindow(window);
    }
}

unsafe fn browse(window: Handle, pane: Pane, root: PathBuf, directory: PathBuf) {
    let progress = match pane {
        Pane::Source => "Lettura dell'origine…",
        Pane::Destination => "Lettura della destinazione…",
    };
    launch(window, Some(progress), move || {
        TaskResult::Browse(pane, Listing::read(pane, root, directory))
    });
}

unsafe fn highlighted(window: Handle, pane: Pane) -> Option<usize> {
    let index = SendMessageW(
        GetDlgItem(window, pane.controls().files),
        LVM_GETNEXTITEM,
        usize::MAX,
        2,
    );
    usize::try_from(index).ok()
}

unsafe fn navigate(window: Handle, pane: Pane, up: bool) {
    let index = highlighted(window, pane);
    let target = pane.with_listing(|view| {
        let directory = if up {
            view.can_go_up().then(|| view.directory.parent()).flatten()
        } else {
            view.selected_folder(index)
        }?;
        Some((view.root.clone(), directory.to_path_buf()))
    });
    if let Some((root, directory)) = target {
        browse(window, pane, root, directory);
    }
}

unsafe fn request_file_change(
    window: Handle,
    pane: Pane,
    action: FileAction,
    directory: &Path,
    item: Option<(PathBuf, bool)>,
) -> Option<FileChange> {
    match action {
        FileAction::NewFile | FileAction::NewFolder => {
            let folder = matches!(action, FileAction::NewFolder);
            let mut name = OsString::new();
            let title = if folder {
                "Nuova cartella"
            } else {
                "Nuovo file"
            };
            loop {
                name = dialog::text_prompt(
                    window,
                    &dialog::TextPrompt {
                        title,
                        context_label: "Crea in questa cartella",
                        context: directory.as_os_str(),
                        input_label: "Nome",
                        initial: &name,
                        submit: "Crea",
                    },
                )?;
                match copy::validate_name(&name) {
                    Ok(()) => return Some(FileChange::Create { name, folder }),
                    Err(error) => file_notice(window, title, &error.to_string()),
                }
            }
        }
        FileAction::Move => {
            let (item, folder) = item?;
            let mut destination = pane.opposite().with_listing(|view| {
                if view.is_folder() {
                    view.directory.clone()
                } else {
                    PathBuf::new()
                }
            });
            loop {
                destination = dialog::text_prompt(
                    window,
                    &dialog::TextPrompt {
                        title: "Conferma spostamento",
                        context_label: if folder {
                            "Cartella da spostare"
                        } else {
                            "File da spostare"
                        },
                        context: item.as_os_str(),
                        input_label: "Percorso della cartella di destinazione",
                        initial: destination.as_os_str(),
                        submit: "Sposta",
                    },
                )?
                .into();
                let error = if !destination.is_absolute() {
                    "Indica il percorso assoluto di una cartella esistente."
                } else if !can_write(&destination) {
                    "La cartella di destinazione è in sola lettura."
                } else {
                    return Some(FileChange::Move {
                        item,
                        folder,
                        destination,
                    });
                };
                file_notice(window, "Sposta", error);
            }
        }
        FileAction::Delete => {
            let (item, folder) = item?;
            let prompt = format!(
                "Eliminare definitivamente {}?\r\n\r\n{}\r\n\r\nL'operazione salta il Cestino.",
                if folder {
                    "la cartella e tutto il suo contenuto"
                } else {
                    "il file"
                },
                item.display(),
            );
            (MessageBoxW(
                window,
                wide(&prompt).as_ptr(),
                wide("Conferma eliminazione").as_ptr(),
                0x124,
            ) == 6)
                .then_some(FileChange::Delete { item, folder })
        }
    }
}

unsafe fn file_notice(window: Handle, title: &str, message: &str) {
    MessageBoxW(window, wide(message).as_ptr(), wide(title).as_ptr(), 0x30);
}

unsafe fn manage_files(window: Handle, pane: Pane, action: FileAction) {
    let index = highlighted(window, pane);
    let (root, directory, is_folder, item) = pane.with_listing(|view| {
        (
            view.root.clone(),
            view.directory.clone(),
            view.is_folder(),
            index
                .and_then(|index| view.entries.get(index))
                .map(|entry| (entry.path.clone(), entry.directory)),
        )
    });
    let creating = matches!(action, FileAction::NewFile | FileAction::NewFolder);
    let path = if creating && is_folder {
        directory.as_path()
    } else if !creating {
        let Some((item, _)) = &item else {
            return;
        };
        item.as_path()
    } else {
        return;
    };
    if !can_write(path) {
        return;
    }
    let source_access = linux_write_access(path);
    FILE_PROMPT_OPEN.set(true);
    let change = request_file_change(window, pane, action, &directory, item);
    FILE_PROMPT_OPEN.set(false);
    let Some(change) = change else {
        return;
    };

    let destination_access = match &change {
        FileChange::Move { destination, .. } => {
            linux_write_access(destination).filter(|access| Some(access) != source_access.as_ref())
        }
        _ => None,
    };
    let (source_root, source_directory, selected) = UI.with_borrow(|ui| {
        (
            ui.listing.root.clone(),
            ui.listing.directory.clone(),
            ui.selected.iter().cloned().collect(),
        )
    });
    let (destination_root, destination_directory) =
        Pane::Destination.with_listing(|view| (view.root.clone(), view.directory.clone()));
    let moving = matches!(action, FileAction::Move);
    let progress: TransferProgress = Arc::new(Mutex::new(None));
    let worker_progress = Arc::clone(&progress);
    queue_selection_size(window);
    launch(
        window,
        Some(match action {
            FileAction::NewFile => "Creazione del file…",
            FileAction::NewFolder => "Creazione della cartella…",
            FileAction::Move => "Preparazione dello spostamento…",
            FileAction::Delete => "Eliminazione in corso…",
        }),
        move || {
            let apply = || match change {
                FileChange::Create { name, folder } => {
                    copy::create_item(&root, &directory, &name, folder)?;
                    Ok(format!(
                        "{}  {}",
                        if folder {
                            "Cartella creata"
                        } else {
                            "File creato"
                        },
                        directory.join(name).display()
                    ))
                }
                FileChange::Move {
                    item,
                    folder,
                    destination,
                } => {
                    copy::move_item(&root, &item, folder, &destination, &mut |update| {
                        *worker_progress.lock().unwrap() = Some(update);
                    })?;
                    Ok(format!(
                        "{} in {}",
                        if folder {
                            "Cartella spostata"
                        } else {
                            "File spostato"
                        },
                        destination.display()
                    ))
                }
                FileChange::Delete { item, folder } => {
                    copy::delete_item(&root, &item, folder)?;
                    Ok(format!(
                        "{}  {}",
                        if folder {
                            "Cartella eliminata"
                        } else {
                            "File eliminato"
                        },
                        item.display()
                    ))
                }
            };
            let apply = || match destination_access {
                Some(access) => access.run(apply),
                None => apply(),
            };
            let result = match source_access {
                Some(access) => access.run(apply),
                None => apply(),
            };
            TaskResult::FileChange {
                result,
                source: refresh_source(source_root, source_directory, selected),
                destination: Listing::refresh(
                    Pane::Destination,
                    destination_root,
                    destination_directory,
                ),
            }
        },
    );
    if moving && busy() {
        show_progress(window, Operation::Move, progress);
    } else {
        ShowWindow(GetDlgItem(window, PROGRESS), 0);
    }
}

unsafe fn start_transfer(window: Handle, operation: Operation) {
    if busy() {
        return;
    }
    let source = field(window, SOURCE);
    if let Some(disk) = physical_disk(&source) {
        mount_disk(window, Pane::Source, disk);
        return;
    }
    let destination_root = field(window, DESTINATION);
    if let Some(disk) = physical_disk(&destination_root) {
        mount_disk(window, Pane::Destination, disk);
        return;
    }
    let destination =
        DESTINATION_VIEW.with_borrow(|view| view.current_folder(&destination_root).to_path_buf());
    let moving = operation == Operation::Move;
    let move_access = moving.then(|| linux_write_access(&source)).flatten();
    if readonly_volume(&destination)
        || (moving && move_access.is_none() && readonly_volume(&source))
    {
        status(
            window,
            "Il volume Linux è in sola lettura. Puoi usarlo come origine per una copia.",
        );
        return;
    }
    if !source.is_absolute() || !destination.is_absolute() {
        status(
            window,
            "Indica due percorsi assoluti. La destinazione deve essere una cartella esistente.",
        );
        return;
    }
    let (directory, selected) = UI.with_borrow(|ui| {
        if ui.listing.root == source {
            (
                ui.listing.directory.clone(),
                ui.selected.iter().cloned().collect::<Vec<_>>(),
            )
        } else {
            (source.clone(), Vec::new())
        }
    });
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
    let keep_both = SendMessageW(GetDlgItem(window, KEEP_BOTH), BM_GETCHECK, 0, 0) == 1;
    let progress: TransferProgress = Arc::new(Mutex::new(None));
    let worker_progress = Arc::clone(&progress);
    if moving {
        queue_selection_size(window);
    }
    launch(
        window,
        Some(if moving {
            "Preparazione dello spostamento…"
        } else {
            "Preparazione della copia…"
        }),
        move || {
            let transfer = || {
                copy::transfer(
                    &source,
                    &destination,
                    &selected,
                    operation,
                    keep_both,
                    &mut |update| *worker_progress.lock().unwrap() = Some(update),
                )
            };
            let result = match move_access {
                Some(access) => access.run(transfer),
                None => transfer(),
            };
            let finished = result.is_ok();
            let (message, details) = match result {
                Ok(report) => {
                    let mut message = format!(
                        "{} file {} · {} · {} cartelle · {} saltati.",
                        report.files,
                        if moving { "spostati" } else { "copiati" },
                        display_bytes(report.bytes),
                        report.folders,
                        report.skipped,
                    );
                    if report.renamed > 0 {
                        message.push_str(&format!(" {} rinominati.", report.renamed));
                    }
                    let details = (report.skipped > 0).then(|| {
                        let mut details =
                            format!("{message}\r\n\r\n{}", report.skip_details.join("\r\n\r\n"));
                        if report.skipped > report.skip_details.len() as u64 {
                            details.push_str(&format!(
                                "\r\n\r\nAltri elementi saltati: {}.",
                                report.skipped - report.skip_details.len() as u64
                            ));
                        }
                        details
                    });
                    (message, details)
                }
                Err(error) => {
                    let message = format!(
                        "{} interrotta. {error}",
                        if moving { "Operazione" } else { "Copia" }
                    );
                    (message.clone(), Some(message))
                }
            };
            let source = moving.then(|| refresh_source(source, directory, selected));
            let destination = Listing::read(Pane::Destination, destination_root, destination).ok();
            TaskResult::Transfer {
                message,
                details,
                finished,
                source,
                destination,
            }
        },
    );
    if busy() {
        show_progress(window, operation, progress);
    }
}

unsafe fn show_progress(window: Handle, operation: Operation, progress: TransferProgress) {
    ACTIVE_PROGRESS.with(|active| active.replace(Some((operation, progress))));
    let bar = GetDlgItem(window, PROGRESS);
    SendMessageW(bar, PBM_SETPOS, 0, 0);
    ShowWindow(bar, 5);
}

fn display_bytes(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * MIB;
    match bytes {
        bytes if bytes >= GIB => format!("{:.1} GiB", bytes as f64 / GIB as f64),
        bytes if bytes >= MIB => format!("{:.1} MiB", bytes as f64 / MIB as f64),
        bytes => format!("{bytes} byte"),
    }
}

fn display_selection_bytes(bytes: u64) -> String {
    const MB: u64 = 1_000_000;
    const GB: u64 = 1_000 * MB;
    match bytes {
        0 => "0 MB".into(),
        1..=9_999 => "< 0,01 MB".into(),
        bytes if bytes >= GB => format!("{:.2} GB", bytes as f64 / GB as f64).replace('.', ","),
        bytes => format!("{:.2} MB", bytes as f64 / MB as f64).replace('.', ","),
    }
}

unsafe fn queue_selection_size(window: Handle) {
    let selected = UI.with_borrow_mut(|ui| {
        ui.selection_size = SelectionSize::Pending;
        !ui.selected.is_empty()
    });
    let running = SIZE_TASK.with_borrow(|task| {
        if let Some((cancelled, _)) = task {
            cancelled.store(true, Ordering::Relaxed);
        }
        task.is_some()
    });
    if selected || running {
        if SetTimer(window, SELECTION_SIZE_TIMER, 70, null_mut()) == 0 {
            UI.with_borrow_mut(|ui| ui.selection_size = SelectionSize::Unavailable);
        }
    } else {
        KillTimer(window, SELECTION_SIZE_TIMER);
    }
}

unsafe fn poll_selection_size(window: Handle) {
    let finished = SIZE_TASK.with_borrow_mut(|task| task.take_if(|(_, task)| task.is_finished()));
    if let Some((cancelled, task)) = finished {
        if !cancelled.load(Ordering::Relaxed) {
            let size = task
                .join()
                .map_or(SelectionSize::Unavailable, SelectionSize::Measured);
            UI.with_borrow_mut(|ui| ui.selection_size = size);
            KillTimer(window, SELECTION_SIZE_TIMER);
            update_controls(window);
            return;
        }
    }
    if SIZE_TASK.with_borrow(Option::is_some) || busy() {
        return;
    }
    let request = UI.with_borrow(|ui| {
        (!ui.selected.is_empty() && matches!(ui.selection_size, SelectionSize::Pending)).then(
            || {
                (
                    ui.listing.root.clone(),
                    ui.selected.iter().cloned().collect::<Vec<_>>(),
                )
            },
        )
    });
    let Some((root, selected)) = request else {
        KillTimer(window, SELECTION_SIZE_TIMER);
        return;
    };
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancelled = Arc::clone(&cancelled);
    match std::thread::Builder::new()
        .spawn(move || copy::selection_size(&root, &selected, &worker_cancelled))
    {
        Ok(task) => SIZE_TASK.with_borrow_mut(|current| *current = Some((cancelled, task))),
        Err(_) => {
            UI.with_borrow_mut(|ui| ui.selection_size = SelectionSize::Unavailable);
            KillTimer(window, SELECTION_SIZE_TIMER);
            update_controls(window);
        }
    }
}

fn readonly_volume(path: &Path) -> bool {
    MOUNTED_DISKS.with_borrow(|disks| {
        disks
            .iter()
            .flat_map(|disk| &disk.volumes)
            .any(|volume| volume.readonly && volume.contains(path))
    })
}

fn linux_write_access(path: &Path) -> Option<linux::WriteAccess> {
    MOUNTED_DISKS.with_borrow(|disks| {
        disks
            .iter()
            .flat_map(|disk| &disk.volumes)
            .find(|volume| volume.contains(path))
            .and_then(|volume| volume.write_access.clone())
    })
}

fn can_write(path: &Path) -> bool {
    !readonly_volume(path) || linux_write_access(path).is_some()
}

fn refresh_source(root: PathBuf, directory: PathBuf, selected: Vec<PathBuf>) -> Browser {
    let listing = Listing::refresh(Pane::Source, root, directory);
    if listing.root.as_os_str().is_empty() {
        return Browser::default();
    }
    let mut selected: BTreeSet<_> = selected.into_iter().collect();
    selected.retain(|path| {
        std::fs::symlink_metadata(path)
            .err()
            .is_none_or(|error| error.kind() != io::ErrorKind::NotFound)
    });
    Browser {
        listing,
        selected,
        selection_size: SelectionSize::Pending,
    }
}

unsafe fn collect_task(window: Handle) {
    let progress = ACTIVE_PROGRESS.with_borrow(|active| {
        let (operation, latest) = active.as_ref()?;
        Some((*operation, latest.lock().unwrap().take()?))
    });
    if let Some((operation, progress)) = progress {
        let percent = (u128::from(progress.processed) * 100 / u128::from(progress.total.max(1)))
            .min(99) as usize;
        SendMessageW(GetDlgItem(window, PROGRESS), PBM_SETPOS, percent, 0);
        status(
            window,
            &format!(
                "{} · {percent}% · {} scritti",
                if operation == Operation::Move {
                    "Spostamento"
                } else {
                    "Copia"
                },
                display_bytes(progress.written)
            ),
        );
    }
    let result = JOB.with_borrow(|job| job.as_ref().map(Receiver::try_recv));
    match result {
        Some(Err(TryRecvError::Empty)) | None => return,
        _ => {}
    }
    JOB.with(|job| job.replace(None));
    ACTIVE_PROGRESS.with(|active| active.replace(None));
    KillTimer(window, JOB_TIMER);
    if EJECT_EXIT.take() == Some(true) {
        DestroyWindow(window);
        return;
    }
    match result {
        Some(Ok(TaskResult::Drives { result, choose })) => {
            refresh_drives(window, choose, result);
        }
        Some(Ok(TaskResult::Mount {
            result: Ok(disk),
            pane,
        })) => {
            let mut choices = disk
                .volumes
                .iter()
                .filter(|volume| pane == Pane::Source || !volume.readonly);
            let path = choices.next().map(|volume| volume.path.clone());
            let multiple = choices.next().is_some();
            let first = MOUNTED_DISKS
                .with_borrow(|disks| disks.iter().map(|disk| disk.volumes.len()).sum());
            MOUNTED_DISKS.with_borrow_mut(|disks| disks.push(disk));
            add_volumes(window, first);
            if multiple {
                set_path(window, pane, Path::new(""));
                status(window, "Disco aperto in WSL. Scegli il volume dal menu.");
            } else if let Some(path) = path {
                open_volume(window, pane, path);
            } else {
                set_path(window, pane, Path::new(""));
                status(
                    window,
                    "I volumi del disco sono in sola lettura. Puoi sceglierli dal menu Origine.",
                );
            }
        }
        Some(Ok(TaskResult::Mount {
            result: Err(error), ..
        })) => {
            let message = error.to_string();
            status(window, &message);
            MessageBoxW(
                window,
                wide(&message).as_ptr(),
                wide("Disco Linux · Disk Copy 98").as_ptr(),
                0x30,
            );
        }
        Some(Ok(TaskResult::Eject { remaining, error })) => {
            MOUNTED_DISKS.with(|disks| disks.replace(remaining));
            for pane in [Pane::Source, Pane::Destination] {
                if field(window, pane.controls().path)
                    .to_string_lossy()
                    .contains(r"\mnt\wsl\disk-copy-98.")
                {
                    set_path(window, pane, Path::new(""));
                }
            }
            if let Some(error) = error {
                status(window, &error);
                MessageBoxW(
                    window,
                    wide(&error).as_ptr(),
                    wide("Smontaggio · Disk Copy 98").as_ptr(),
                    0x30,
                );
            } else {
                status(window, "Dischi Linux smontati.");
            }
            scan_drives(window, false);
        }
        Some(Ok(TaskResult::Browse(pane, Ok(listing)))) => {
            if field(window, pane.controls().path) != listing.root {
                write_path(window, pane, &listing.root);
            }
            let message = match pane {
                Pane::Source => {
                    let message = if listing.entries.is_empty() {
                        "La cartella di origine è vuota."
                    } else if readonly_volume(&listing.root)
                        && linux_write_access(&listing.root).is_none()
                    {
                        "Origine Linux in sola lettura. Spunta gli elementi da copiare."
                    } else {
                        "Spunta a sinistra i file e le cartelle da trasferire."
                    };
                    let remeasure = UI.with_borrow_mut(|ui| {
                        let count = ui.selected.len();
                        let same_directory = ui.listing.directory == listing.directory;
                        if ui.listing.root != listing.root {
                            ui.selected.clear();
                        }
                        let visible: BTreeSet<_> =
                            listing.entries.iter().map(|entry| &entry.path).collect();
                        ui.selected.retain(|path| {
                            path.parent() != Some(&listing.directory) || visible.contains(&path)
                        });
                        ui.listing = listing;
                        same_directory || count != ui.selected.len()
                    });
                    if remeasure {
                        queue_selection_size(window);
                    }
                    message
                }
                Pane::Destination => {
                    let readonly = readonly_volume(&listing.directory);
                    DESTINATION_VIEW.with(|view| view.replace(listing));
                    if readonly {
                        "La cartella di destinazione è in sola lettura."
                    } else {
                        "I file verranno trasferiti nella cartella aperta a destra."
                    }
                }
            };
            update_view(window, pane);
            status(window, message);
        }
        Some(Ok(TaskResult::Browse(pane, Err(error)))) => {
            let location = if pane == Pane::Source {
                "l'origine"
            } else {
                "la destinazione"
            };
            status(window, &format!("Impossibile aprire {location}. {error}"))
        }
        Some(Ok(TaskResult::Transfer {
            message,
            details,
            finished,
            source,
            destination,
        })) => {
            if let Some(browser) = source {
                replace_browser(window, browser);
            }
            if let Some(listing) = destination {
                DESTINATION_VIEW.with(|view| view.replace(listing));
                update_view(window, Pane::Destination);
            }
            status(window, &message);
            if finished {
                SendMessageW(GetDlgItem(window, PROGRESS), PBM_SETPOS, 100, 0);
            }
            if let Some(details) = details {
                MessageBoxW(
                    window,
                    wide(&details).as_ptr(),
                    wide("Esito del trasferimento").as_ptr(),
                    0x40,
                );
            }
        }
        Some(Ok(TaskResult::FileChange {
            result,
            source,
            destination,
        })) => {
            replace_browser(window, source);
            DESTINATION_VIEW.with(|view| view.replace(destination));
            update_view(window, Pane::Destination);
            ShowWindow(GetDlgItem(window, PROGRESS), 0);
            match result {
                Ok(message) => status(window, &message),
                Err(error) => {
                    let message = format!("Operazione interrotta. {error}");
                    status(window, &message);
                    file_notice(window, "Gestione file · Disk Copy 98", &message);
                }
            }
        }
        _ => status(window, "Operazione interrotta."),
    }
    update_controls(window);
}

unsafe fn replace_browser(window: Handle, browser: Browser) {
    UI.with(|ui| ui.replace(browser));
    queue_selection_size(window);
    update_view(window, Pane::Source);
}

unsafe fn update_view(window: Handle, pane: Pane) {
    let ids = pane.controls();
    let list = GetDlgItem(window, ids.files);
    let icons = THEME.with_borrow(|theme| !theme.as_ref().unwrap().icons.is_null());
    REDRAW_LIST.set(true);
    SendMessageW(list, 0x000b, 0, 0); // WM_SETREDRAW
    SendMessageW(list, LVM_DELETEALLITEMS, 0, 0);
    pane.with_listing(|view| {
        for (index, entry) in view.entries.iter().enumerate() {
            let name: Vec<_> = entry
                .path
                .file_name()
                .unwrap_or_default()
                .encode_wide()
                .chain([0])
                .collect();
            let mut item = ListItem {
                mask: if icons { 3 } else { 1 },
                item: index as i32,
                subitem: 0,
                state: 0,
                state_mask: 0,
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
    if pane == Pane::Source {
        update_checks(window);
    }
    SendMessageW(list, 0x000b, 1, 0);
    REDRAW_LIST.set(false);
    InvalidateRect(list, null(), 1);
    let address = pane.with_listing(|view| {
        if view.directory.as_os_str().is_empty() {
            match pane {
                Pane::Source => "Scegli l'unità o il percorso da sfogliare.".into(),
                Pane::Destination => "Apri la cartella in cui trasferire i file.".into(),
            }
        } else {
            view.directory.display().to_string()
        }
    });
    text(window, ids.address, &address);
    update_controls(window);
}

unsafe fn update_checks(window: Handle) {
    let restoring = REDRAW_LIST.replace(true);
    let list = GetDlgItem(window, FILES);
    UI.with_borrow(|ui| {
        for (index, entry) in ui.listing.entries.iter().enumerate() {
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
    let source = field(window, SOURCE);
    for pane in [Pane::Source, Pane::Destination] {
        let ids = pane.controls();
        let selected = highlighted(window, pane);
        let (can_open, can_up, can_create, can_change) = pane.with_listing(|view| {
            let item = selected.and_then(|index| view.entries.get(index));
            (
                view.selected_folder(selected).is_some(),
                view.can_go_up(),
                view.is_folder() && can_write(&view.directory),
                item.is_some_and(|entry| can_write(&entry.path)),
            )
        });
        for (id, enabled) in [
            (ids.path, true),
            (ids.show, true),
            (ids.files, true),
            (ids.open, can_open),
            (ids.up, can_up),
        ] {
            EnableWindow(GetDlgItem(window, id), i32::from(enabled && !busy));
        }
        for (action, _, _) in FILE_ACTIONS {
            let enabled = match action {
                FileAction::NewFile | FileAction::NewFolder => can_create,
                FileAction::Move | FileAction::Delete => can_change,
            };
            EnableWindow(
                GetDlgItem(window, ids.actions + action as i32),
                i32::from(enabled && !busy),
            );
        }
    }
    let (count, visible, covered, summary) = UI.with_borrow(|ui| {
        (
            ui.selected.len(),
            ui.listing.entries.len(),
            ui.inside_selection(),
            ui.selection_summary(),
        )
    });
    for (id, enabled) in [
        (AUTO, true),
        (EJECT, MOUNTED_DISKS.with_borrow(|disks| !disks.is_empty())),
        (SELECT_ALL, visible > 0 && !covered),
        (CLEAR, count > 0),
        (COPY, true),
        (KEEP_BOTH, true),
        (MOVE, count > 0 && can_write(&source)),
    ] {
        EnableWindow(GetDlgItem(window, id), i32::from(enabled && !busy));
    }
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
            let mut visible: BTreeSet<_> = ui
                .listing
                .entries
                .iter()
                .map(|entry| entry.path.clone())
                .collect();
            let directory = &ui.listing.directory;
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
    queue_selection_size(window);
    update_checks(window);
    update_controls(window);
}

unsafe fn list_notification(window: Handle, pane: Pane, notification: &Notification) -> isize {
    if REDRAW_LIST.get() || busy() {
        return 0;
    }
    match notification.code {
        -3 | -4 => navigate(window, pane, false), // NM_DBLCLK | NM_RETURN
        -100 | -101 if pane == Pane::Source => {
            let change = &*(notification as *const Notification as *const ListNotification);
            let old = change.old_state & LVIS_STATEIMAGEMASK;
            let new = change.new_state & LVIS_STATEIMAGEMASK;
            if old != new && new != 0 {
                if change.header.code == -100 {
                    return UI.with_borrow(Browser::inside_selection) as isize;
                }
                let changed = UI.with_borrow_mut(|ui| {
                    let Some(entry) = ui.listing.entries.get(change.item as usize) else {
                        return false;
                    };
                    let path = entry.path.clone();
                    if new == 0x2000 {
                        ui.check(path);
                        true
                    } else {
                        ui.selected.remove(&path)
                    }
                });
                if changed {
                    queue_selection_size(window);
                }
            }
            if change.header.code == -101 {
                update_controls(window);
            }
        }
        -101 => update_controls(window),
        _ => {}
    }
    0
}

unsafe fn help(window: Handle) {
    MessageBoxW(
        window,
        wide(concat!(
            "1. Scegli l'unità di origine a sinistra e quella di destinazione a destra. ",
            "Per aprire un percorso digitato o incollato, usa Sfoglia nel relativo pannello. ",
            "Auto disco dà priorità alle unità USB e rimovibili. ",
            "Scegli un disco senza lettera per montarlo tramite WSL. ",
            "Se lo ha trovato Auto disco, premi Sfoglia. Usa Espelli WSL e controlla l'esito prima di scollegare il disco.\r\n\r\n",
            "2. Apri una cartella con un doppio clic o con Apri e usa Su per risalire. ",
            "A destra apri la cartella in cui vuoi trasferire i file. A sinistra spunta gli elementi da trasferire. ",
            "Le spunte restano attive quando cambi cartella. Spuntare una cartella include tutto il suo contenuto. ",
            "Sotto l'elenco trovi la dimensione totale degli elementi spuntati in MB o GB. ",
            "Azzera spunte deseleziona tutti gli elementi.\r\n\r\n",
            "3. Copia selezione lascia gli originali al loro posto. Sposta selezione li rimuove dopo il trasferimento. ",
            "Senza spunte, Copia tutto copia l'intera origine. La barra mostra l'avanzamento.\r\n\r\n",
            "4. Nei due pannelli usa Nuovo file o Nuova cartella per creare un elemento nella cartella aperta. ",
            "Evidenzia una riga per usare Sposta o Elimina. Sposta chiede la cartella di arrivo e mantiene il nome. ",
            "Elimina chiede conferma e rimuove la cartella con tutto il suo contenuto, saltando il Cestino.\r\n\r\n",
            "Se il nome di un file esiste già, attiva Mantieni entrambi per aggiungere un suffisso in destinazione. ",
            "Il riepilogo indica il motivo dei salti.\r\n\r\n",
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
            if [ADDRESS, DESTINATION_ADDRESS, STATUS]
                .iter()
                .any(|&id| control == GetDlgItem(window, id))
            {
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
                    (SOURCE | DESTINATION, 1 | 5) if !SETTING_PATHS.get() => {
                        let pane = if id == SOURCE {
                            Pane::Source
                        } else {
                            Pane::Destination
                        };
                        clear_pane(window, pane);
                        let notice = physical_disk(&field(window, id)).map(|disk| disk.notice);
                        status(
                            window,
                            notice
                                .as_deref()
                                .unwrap_or("Premi Sfoglia nel pannello per aprire il percorso."),
                        );
                    }
                    (SOURCE, 9) if !SETTING_PATHS.get() => {
                        choose_disk_item(window, Pane::Source, lparam as Handle)
                    }
                    (DESTINATION, 9) if !SETTING_PATHS.get() => {
                        choose_disk_item(window, Pane::Destination, lparam as Handle)
                    }
                    (AUTO, 0) => scan_drives(window, true),
                    (SHOW, 0) => show_files(window, Pane::Source),
                    (DESTINATION_SHOW, 0) => show_files(window, Pane::Destination),
                    (EJECT, 0) => eject_linux(window, false),
                    (OPEN, 0) => navigate(window, Pane::Source, false),
                    (UP, 0) => navigate(window, Pane::Source, true),
                    (DESTINATION_OPEN, 0) => navigate(window, Pane::Destination, false),
                    (DESTINATION_UP, 0) => navigate(window, Pane::Destination, true),
                    (SELECT_ALL, 0) => bulk_selection(window, true),
                    (CLEAR, 0) => bulk_selection(window, false),
                    (COPY, 0) => start_transfer(window, Operation::Copy),
                    (MOVE, 0) => start_transfer(window, Operation::Move),
                    (SOURCE_ACTIONS..=129, 0) => manage_files(
                        window,
                        Pane::Source,
                        FILE_ACTIONS[(id - SOURCE_ACTIONS) as usize].0,
                    ),
                    (DESTINATION_ACTIONS..=133, 0) => manage_files(
                        window,
                        Pane::Destination,
                        FILE_ACTIONS[(id - DESTINATION_ACTIONS) as usize].0,
                    ),
                    _ => {}
                }
            }
        }
        0x4e if wparam == FILES as usize && lparam != 0 => {
            return list_notification(window, Pane::Source, &*(lparam as *const Notification));
        }
        0x4e if wparam == DESTINATION_FILES as usize && lparam != 0 => {
            return list_notification(window, Pane::Destination, &*(lparam as *const Notification));
        }
        0x113 if wparam == JOB_TIMER => collect_task(window),
        0x113 if wparam == SELECTION_SIZE_TIMER => poll_selection_size(window),
        0x113 if wparam == DEVICE_TIMER => {
            if !busy() && !FILE_PROMPT_OPEN.get() {
                KillTimer(window, DEVICE_TIMER);
                scan_drives(window, field(window, SOURCE).as_os_str().is_empty());
            }
        }
        0x219
            if wparam == 7
                || (matches!(wparam, 0x8000 | 0x8004)
                    && lparam != 0
                    && *(lparam as *const u32).add(1) == 2) =>
        {
            SetTimer(window, DEVICE_TIMER, 400, null_mut());
            return 1;
        }
        0x10 if EJECT_EXIT.get().is_some() => EJECT_EXIT.set(Some(true)),
        0x10 if busy() => {
            MessageBoxW(
                window,
                wide("Attendi il termine dell'operazione.").as_ptr(),
                wide("Disk Copy 98").as_ptr(),
                0x40,
            );
        }
        0x10 if MOUNTED_DISKS.with_borrow(|disks| !disks.is_empty()) => eject_linux(window, true),
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
            1100,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_summary_shows_the_total_in_mb_or_gb() {
        let mut browser = Browser::default();
        browser.check(PathBuf::from("file.bin"));
        browser.selection_size = SelectionSize::Measured(copy::SizeEstimate {
            bytes: 25_000_000,
            incomplete: false,
        });
        assert!(browser.selection_summary().ends_with(" · 25,00 MB"));

        browser.selection_size = SelectionSize::Measured(copy::SizeEstimate {
            bytes: 1_250_000_000,
            incomplete: false,
        });
        assert!(browser.selection_summary().ends_with(" · 1,25 GB"));

        browser.selected.clear();
        assert!(browser.selection_summary().ends_with("0 con spunta"));
    }

    #[test]
    fn destination_uses_the_open_folder_until_the_input_changes() -> io::Result<()> {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let folder = root.join("src");
        let typed_folder = root.join("scripts");
        let file = root.join("README.md");

        let listing = Listing::read(Pane::Destination, root.clone(), root.clone())?;
        assert!(!listing.can_go_up());
        let folder_index = listing
            .entries
            .iter()
            .position(|entry| entry.path == folder);
        assert_eq!(
            listing.selected_folder(folder_index),
            Some(folder.as_path())
        );
        let file_index = listing.entries.iter().position(|entry| entry.path == file);
        assert!(file_index.is_some());
        assert_eq!(listing.selected_folder(file_index), None);

        let listing = Listing::read(Pane::Destination, root.clone(), folder.clone())?;
        assert!(listing.can_go_up());
        assert_eq!(listing.current_folder(&root), folder);
        assert_eq!(listing.current_folder(&typed_folder), typed_folder);
        let result = Listing::read(Pane::Destination, file.clone(), file);
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::NotADirectory);
        Ok(())
    }
}
