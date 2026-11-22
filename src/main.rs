#![windows_subsystem = "windows"]

mod copy;
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
const APP_ICON_RESOURCE: u16 = 101;
const MIN_WIDTH: i32 = 700;
const MIN_HEIGHT: i32 = 600;
const JOB_TIMER: usize = 1;
const DEVICE_TIMER: usize = 2;
const SELECTION_SIZE_TIMER: usize = 3;

#[derive(Default)]
enum SelectionSize {
    #[default]
    Pending,
    Measured(copy::SizeEstimate),
    Unavailable,
}

#[derive(Default)]
struct Browser {
    root: PathBuf,
    directory: PathBuf,
    entries: Vec<Entry>,
    selected: BTreeSet<PathBuf>,
    selection_size: SelectionSize,
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

    fn selection_summary(&self) -> String {
        let visible = self.entries.len();
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
        result: io::Result<disks::Inventory>,
        choose: bool,
    },
    Mount {
        result: io::Result<linux::MountedDisk>,
        field: i32,
    },
    Eject {
        remaining: Vec<linux::MountedDisk>,
        error: Option<String>,
        exit: bool,
    },
    Transfer {
        message: String,
        details: Option<String>,
        finished: bool,
        refreshed: Option<Browser>,
    },
}

type TransferProgress = Arc<Mutex<Option<copy::Progress>>>;
type SizeTask = (Arc<AtomicBool>, JoinHandle<copy::SizeEstimate>);

thread_local! {
    static UI: RefCell<Browser> = RefCell::new(Browser::default());
    static THEME: RefCell<Option<Theme>> = const { RefCell::new(None) };
    static JOB: RefCell<Option<Receiver<TaskResult>>> = const { RefCell::new(None) };
    static SIZE_TASK: RefCell<Option<SizeTask>> = const { RefCell::new(None) };
    static ACTIVE_PROGRESS: RefCell<Option<(Operation, TransferProgress)>> = const { RefCell::new(None) };
    static REDRAW_LIST: Cell<bool> = const { Cell::new(false) };
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
    control(window, "BUTTON", "Espelli WSL", EJECT, BUTTON, 0);
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
    place(EJECT, 30, FILES_TOP + 8, 113, 29);
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

unsafe fn selected_disk_item(combo: Handle) -> Option<usize> {
    let selection = SendMessageW(combo, CB_GETCURSEL, 0, 0);
    let data = SendMessageW(combo, CB_GETITEMDATA, usize::try_from(selection).ok()?, 0);
    usize::try_from(data).ok().filter(|&index| index > 0)
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

unsafe fn set_path(window: Handle, id: i32, path: &Path) {
    SETTING_PATHS.set(true);
    let value: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    SetWindowTextW(GetDlgItem(window, id), value.as_ptr());
    SETTING_PATHS.set(false);
    if id == SOURCE {
        replace_browser(window, Browser::default());
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
        replace_browser(window, Browser::default());
    }
    if failed && (choose || removed) {
        status(window, "Inventario dei dischi fisici non disponibile. Puoi usare le unità con lettera o digitare un percorso.");
    } else if let Some(disk) = physical_disk(&new_source) {
        if choose || source != new_source {
            status(window, &disk.notice);
        }
    } else if automatic {
        status(
            window,
            &format!(
                "Unità {} selezionata. Scegli la destinazione o premi Mostra file.",
                new_source.display()
            ),
        );
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

unsafe fn show_files(window: Handle) {
    let root = field(window, SOURCE);
    if let Some(disk) = physical_disk(&root) {
        mount_disk(window, SOURCE, disk);
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

unsafe fn mount_disk(window: Handle, field: i32, disk: disks::Disk) {
    if !disk.mountable {
        status(window, &disk.notice);
        return;
    }
    launch(
        window,
        Some("Collegamento del disco a WSL. Conferma eventuali richieste di Windows…"),
        move || TaskResult::Mount {
            result: linux::MountedDisk::open(disk),
            field,
        },
    );
}

unsafe fn choose_disk_item(window: Handle, id: i32, combo: Handle) {
    let Some(item) = selected_disk_item(combo) else {
        return;
    };
    let physical = DISKS_WITHOUT_DRIVE_LETTERS.with_borrow(Vec::len);
    if item <= physical {
        let disk = DISKS_WITHOUT_DRIVE_LETTERS.with_borrow(|disks| disks[item - 1].clone());
        mount_disk(window, id, disk);
    } else {
        let path = MOUNTED_DISKS.with_borrow(|disks| {
            disks
                .iter()
                .flat_map(|disk| &disk.volumes)
                .nth(item - physical - 1)
                .map(|volume| volume.path.clone())
        });
        if let Some(path) = path {
            set_path(window, id, &path);
            if id == SOURCE {
                browse(window, path.clone(), path);
            } else {
                status(window, "Volume Linux impostato come destinazione.");
            }
        }
    }
}

unsafe fn eject_linux(window: Handle, exit: bool) {
    let disks = MOUNTED_DISKS.with(|disks| disks.take());
    if UI.with_borrow(|ui| {
        disks
            .iter()
            .flat_map(|disk| &disk.volumes)
            .any(|volume| ui.root.starts_with(&volume.path))
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
            TaskResult::Eject {
                remaining,
                error,
                exit,
            }
        },
    );
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
    if let Some(disk) = physical_disk(&source) {
        mount_disk(window, SOURCE, disk);
        return;
    }
    let destination = field(window, DESTINATION);
    if let Some(disk) = physical_disk(&destination) {
        mount_disk(window, DESTINATION, disk);
        return;
    }
    let moving = operation == Operation::Move;
    let move_access = moving.then(|| linux_move_access(&source)).flatten();
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
        if ui.root == source {
            (
                ui.directory.clone(),
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
            let refreshed = moving.then(|| refresh_after_move(source, directory, selected));
            TaskResult::Transfer {
                message,
                details,
                finished,
                refreshed,
            }
        },
    );
    if busy() {
        ACTIVE_PROGRESS.with(|active| active.replace(Some((operation, progress))));
        let bar = GetDlgItem(window, PROGRESS);
        SendMessageW(bar, PBM_SETPOS, 0, 0);
        ShowWindow(bar, 5);
    }
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
                    ui.root.clone(),
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
            .any(|volume| volume.readonly && path.starts_with(&volume.path))
    })
}

fn linux_move_access(path: &Path) -> Option<linux::MoveAccess> {
    MOUNTED_DISKS.with_borrow(|disks| {
        disks
            .iter()
            .flat_map(|disk| &disk.volumes)
            .find(|volume| path.starts_with(&volume.path))
            .and_then(|volume| volume.move_access.clone())
    })
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
                selection_size: SelectionSize::Pending,
            };
        }
    }
    Browser::default()
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
    match result {
        Some(Ok(TaskResult::Drives { result, choose })) => {
            refresh_drives(window, choose, result);
        }
        Some(Ok(TaskResult::Mount {
            result: Ok(disk),
            field,
        })) => {
            let mut choices = disk
                .volumes
                .iter()
                .filter(|volume| field == SOURCE || !volume.readonly);
            let path = choices.next().map(|volume| volume.path.clone());
            let multiple = choices.next().is_some();
            let first = MOUNTED_DISKS
                .with_borrow(|disks| disks.iter().map(|disk| disk.volumes.len()).sum());
            MOUNTED_DISKS.with_borrow_mut(|disks| disks.push(disk));
            add_volumes(window, first);
            if multiple {
                set_path(window, field, Path::new(""));
                status(window, "Disco aperto in WSL. Scegli il volume dal menu.");
            } else if let Some(path) = path {
                set_path(window, field, &path);
                if field == SOURCE {
                    browse(window, path.clone(), path);
                } else {
                    status(
                        window,
                        "Volume Linux impostato. Premi Copia o Sposta per trasferire i file.",
                    );
                }
            } else {
                set_path(window, field, Path::new(""));
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
        Some(Ok(TaskResult::Eject {
            remaining,
            error,
            exit,
        })) => {
            MOUNTED_DISKS.with(|disks| disks.replace(remaining));
            for id in [SOURCE, DESTINATION] {
                if field(window, id)
                    .to_string_lossy()
                    .contains(r"\mnt\wsl\disk-copy-98.")
                {
                    set_path(window, id, Path::new(""));
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
                scan_drives(window, false);
            } else if exit {
                DestroyWindow(window);
                return;
            } else {
                status(window, "Dischi Linux smontati.");
                scan_drives(window, false);
            }
        }
        Some(Ok(TaskResult::Browse(Ok(listing)))) => {
            let count = listing.entries.len();
            let readonly = readonly_volume(&listing.root);
            let can_move = linux_move_access(&listing.root).is_some();
            let remeasure = UI.with_borrow_mut(|ui| {
                let count = ui.selected.len();
                let same_directory = ui.directory == listing.directory;
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
                same_directory || count != ui.selected.len()
            });
            if remeasure {
                queue_selection_size(window);
            }
            update_view(window);
            status(
                window,
                if count == 0 {
                    "La cartella è vuota."
                } else if can_move {
                    "Spunta i file e scegli Copia selezione o Sposta selezione."
                } else if readonly {
                    "Volume Linux in sola lettura. Spunta i file o le cartelle da copiare."
                } else {
                    "Spunta i file o le cartelle da trasferire. Apri le cartelle con un doppio clic."
                },
            );
        }
        Some(Ok(TaskResult::Browse(Err(error)))) => {
            status(window, &format!("Impossibile aprire il percorso. {error}"))
        }
        Some(Ok(TaskResult::Transfer {
            message,
            details,
            finished,
            refreshed,
        })) => {
            if let Some(browser) = refreshed {
                replace_browser(window, browser);
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
        _ => status(window, "Operazione interrotta."),
    }
    update_controls(window);
}

unsafe fn replace_browser(window: Handle, browser: Browser) {
    UI.with(|ui| ui.replace(browser));
    queue_selection_size(window);
    update_view(window);
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
    let source = field(window, SOURCE);
    let highlighted = highlighted(window);
    let (count, visible, can_open, can_up, covered, summary) = UI.with_borrow(|ui| {
        (
            ui.selected.len(),
            ui.entries.len(),
            highlighted
                .and_then(|i| ui.entries.get(i))
                .is_some_and(|entry| entry.directory),
            ui.directory != ui.root,
            ui.inside_selection(),
            ui.selection_summary(),
        )
    });
    for (id, enabled) in [
        (SOURCE, true),
        (DESTINATION, true),
        (AUTO, true),
        (SHOW, true),
        (EJECT, MOUNTED_DISKS.with_borrow(|disks| !disks.is_empty())),
        (FILES, true),
        (OPEN, can_open),
        (UP, can_up),
        (SELECT_ALL, visible > 0 && !covered),
        (CLEAR, count > 0),
        (COPY, true),
        (KEEP_BOTH, true),
        (
            MOVE,
            count > 0 && (!readonly_volume(&source) || linux_move_access(&source).is_some()),
        ),
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
    queue_selection_size(window);
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
                let changed = UI.with_borrow_mut(|ui| {
                    let Some(entry) = ui.entries.get(change.item as usize) else {
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
        _ => {}
    }
    0
}

unsafe fn help(window: Handle) {
    MessageBoxW(
        window,
        wide(concat!(
            "1. Scegli origine e destinazione dai menu oppure digita i percorsi. ",
            "Auto disco dà priorità alle unità USB e rimovibili. ",
            "Scegli un disco senza lettera per montarlo tramite WSL. ",
            "Se lo ha trovato Auto disco, premi Mostra file. Usa Espelli WSL prima di scollegarlo.\r\n\r\n",
            "2. Premi Mostra file. Apri una cartella con un doppio clic o con Apri. Usa Su per risalire. ",
            "Le spunte restano attive quando cambi cartella. Spuntare una cartella include tutto il suo contenuto. ",
            "Sotto l'elenco trovi la dimensione totale degli elementi spuntati in MB o GB. ",
            "Azzera spunte deseleziona tutti gli elementi.\r\n\r\n",
            "3. Copia selezione lascia gli originali al loro posto. Sposta selezione li rimuove dopo il trasferimento. ",
            "Senza spunte, Copia tutto copia l'intera origine. La barra mostra l'avanzamento.\r\n\r\n",
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
                        replace_browser(window, Browser::default());
                        let notice = physical_disk(&field(window, SOURCE)).map(|disk| disk.notice);
                        status(
                            window,
                            notice
                                .as_deref()
                                .unwrap_or("Origine aggiornata. Premi Mostra file per sfogliare."),
                        );
                    }
                    (SOURCE | DESTINATION, 9) if !SETTING_PATHS.get() => {
                        choose_disk_item(window, id, lparam as Handle);
                    }
                    (AUTO, 0) => scan_drives(window, true),
                    (SHOW, 0) => show_files(window),
                    (EJECT, 0) => eject_linux(window, false),
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
        0x113 if wparam == SELECTION_SIZE_TIMER => poll_selection_size(window),
        0x113 if wparam == DEVICE_TIMER => {
            if !busy() {
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
}
