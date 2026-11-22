use std::{
    fs::{self, File, Metadata, OpenOptions},
    io::{self, Read, Write},
    os::windows::{
        fs::{MetadataExt, OpenOptionsExt},
        io::{AsRawHandle, RawHandle},
    },
    path::{Component, Path, PathBuf, Prefix},
    sync::atomic::{AtomicBool, Ordering},
};

#[link(name = "kernel32")]
extern "system" {
    fn SetFileInformationByHandle(file: RawHandle, class: i32, info: *const u8, size: u32) -> i32;
    fn GetFileInformationByHandle(file: RawHandle, information: *mut FileInformation) -> i32;
}

const FILE_SHARE_READ: u32 = 1;
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x20_0000;

#[repr(C)]
#[derive(Default)]
struct FileInformation {
    attributes: u32,
    created: [u32; 2],
    accessed: [u32; 2],
    modified: [u32; 2],
    volume: u32,
    size: [u32; 2],
    links: u32,
    index: [u32; 2],
}

impl FileInformation {
    fn read(file: &File) -> io::Result<Self> {
        let mut information = Self::default();
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(information)
    }

    fn matches(&self, other: &Self) -> bool {
        self.index != [0; 2]
            && self.index == other.index
            && self.volume == other.volume
            && self.size == other.size
            && self.modified == other.modified
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Copy,
    Move,
}

pub struct Entry {
    pub path: PathBuf,
    pub directory: bool,
    pub length: u64,
}

#[derive(Default)]
pub struct Report {
    pub files: u64,
    pub folders: u64,
    pub skipped: u64,
    pub bytes: u64,
    pub renamed: u64,
    pub skip_details: Vec<String>,
}

#[derive(Clone, Copy, Default)]
pub struct Progress {
    pub total: u64,
    pub processed: u64,
    pub written: u64,
}

struct Transfer<'a> {
    operation: Operation,
    keep_both: bool,
    report: Report,
    progress: Progress,
    buffer: Vec<u8>,
    notify: &'a mut dyn FnMut(Progress),
}

impl Transfer<'_> {
    fn advance(&mut self, units: u64, written: u64) {
        self.progress.processed = self.progress.processed.saturating_add(units);
        self.progress.written = self.progress.written.saturating_add(written);
        (self.notify)(self.progress);
    }

    fn skip(&mut self, path: &Path, reason: &str, units: u64) {
        self.report.skipped += 1;
        if self.report.skip_details.len() < 6 {
            self.report
                .skip_details
                .push(format!("{reason}\r\n{}", path.display()));
        }
        self.advance(units, 0);
    }
}

fn is_link(metadata: &Metadata) -> bool {
    metadata.file_attributes() & 0x400 != 0
}

fn wsl_path(path: &Path) -> bool {
    let Some(Component::Prefix(prefix)) = path.components().next() else {
        return false;
    };
    match prefix.kind() {
        Prefix::UNC(host, _) | Prefix::VerbatimUNC(host, _) => {
            host.eq_ignore_ascii_case("wsl.localhost") || host.eq_ignore_ascii_case("wsl$")
        }
        _ => false,
    }
}

fn wsl_permission_denied(path: &Path, error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::PermissionDenied && wsl_path(path)
}

pub fn list(root: &Path, directory: &Path, allow_file: bool) -> io::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    let metadata = source_metadata(root, directory)?;
    if metadata.is_dir() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let metadata = match entry.metadata() {
                Err(error) if wsl_permission_denied(directory, &error) => continue,
                result => result?,
            };
            if !is_link(&metadata) {
                entries.push(Entry {
                    path: entry.path(),
                    directory: metadata.is_dir(),
                    length: metadata.len(),
                });
            }
        }
        entries.sort_by(|a, b| {
            b.directory.cmp(&a.directory).then_with(|| {
                a.path
                    .file_name()
                    .unwrap_or_default()
                    .cmp(b.path.file_name().unwrap_or_default())
            })
        });
    } else if !allow_file {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "La destinazione deve essere una cartella.",
        ));
    } else if metadata.is_file() {
        entries.push(Entry {
            path: directory.into(),
            directory: false,
            length: metadata.len(),
        });
    }
    Ok(entries)
}

fn source_metadata(root: &Path, path: &Path) -> io::Result<Metadata> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| io::ErrorKind::InvalidInput)?;
    let mut current = root.to_path_buf();
    let mut metadata = fs::symlink_metadata(&current)?;
    for component in relative.components() {
        if is_link(&metadata) || !matches!(component, Component::Normal(_)) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        current.push(component.as_os_str());
        metadata = fs::symlink_metadata(&current)?;
    }
    if is_link(&metadata) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "L'origine è un collegamento.",
        ));
    }
    Ok(metadata)
}

#[derive(Default)]
pub struct SizeEstimate {
    pub bytes: u64,
    pub incomplete: bool,
}

impl SizeEstimate {
    fn add(&mut self, path: &Path, metadata: io::Result<Metadata>, cancelled: &AtomicBool) {
        let Ok(metadata) = metadata else {
            self.incomplete = true;
            return;
        };
        if is_link(&metadata) {
            return;
        }
        if metadata.is_file() {
            self.bytes = self.bytes.saturating_add(metadata.len());
        } else if metadata.is_dir() {
            let Ok(entries) = fs::read_dir(path) else {
                self.incomplete = true;
                return;
            };
            for entry in entries {
                if cancelled.load(Ordering::Relaxed) {
                    return;
                }
                match entry {
                    Ok(entry) => self.add(&entry.path(), entry.metadata(), cancelled),
                    Err(_) => self.incomplete = true,
                }
            }
        }
    }
}

pub fn selection_size(root: &Path, selected: &[PathBuf], cancelled: &AtomicBool) -> SizeEstimate {
    let mut size = SizeEstimate::default();
    for path in selected {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        size.add(path, source_metadata(root, path), cancelled);
    }
    size
}

fn normalized(path: &Path) -> PathBuf {
    path.to_string_lossy()
        .to_lowercase()
        .replace(r"unc\wsl$\", r"unc\wsl.localhost\")
        .into()
}

fn validate(source: &Path, destination: &Path) -> io::Result<(Metadata, PathBuf, PathBuf)> {
    let metadata = fs::symlink_metadata(source)?;
    if is_link(&metadata) || !fs::metadata(destination)?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Usa un'origine reale e una cartella di destinazione esistente.",
        ));
    }
    let source_path = fs::canonicalize(source)?;
    let destination = fs::canonicalize(destination)?;
    if metadata.is_dir() && normalized(&destination).starts_with(normalized(&source_path)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "La destinazione deve essere fuori dalla cartella di origine.",
        ));
    }
    Ok((metadata, source_path, destination))
}

fn measure(path: &Path, metadata: &Metadata) -> u64 {
    if is_link(metadata) {
        return 1;
    }
    if metadata.is_file() {
        return metadata.len().saturating_add(1);
    }
    if !metadata.is_dir() {
        return 1;
    }
    fs::read_dir(path)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            entry
                .metadata()
                .ok()
                .map(|metadata| measure(&entry.path(), &metadata))
        })
        .fold(1u64, u64::saturating_add)
}

pub fn transfer(
    source: &Path,
    destination: &Path,
    selected: &[PathBuf],
    operation: Operation,
    keep_both: bool,
    notify: &mut dyn FnMut(Progress),
) -> io::Result<Report> {
    let mut transfer = Transfer {
        operation,
        keep_both,
        report: Report::default(),
        progress: Progress::default(),
        buffer: vec![0; 1024 * 1024],
        notify,
    };
    if selected.is_empty() && operation == Operation::Copy {
        copy_all(source, destination, &mut transfer)?;
    } else {
        transfer_selection(source, destination, selected, &mut transfer)?;
    }
    Ok(transfer.report)
}

fn copy_all(source: &Path, destination: &Path, transfer: &mut Transfer) -> io::Result<()> {
    let (metadata, source, destination) = validate(source, destination)?;
    transfer.progress.total = measure(&source, &metadata);
    transfer.advance(0, 0);
    if metadata.is_dir() {
        for entry in fs::read_dir(&source)? {
            let entry = entry?;
            let path = entry.path();
            let target = destination.join(entry.file_name());
            let metadata = match entry.metadata() {
                Err(error) if wsl_permission_denied(&source, &error) => {
                    transfer.skip(&path, "Accesso all'origine negato.", 1);
                    continue;
                }
                result => result?,
            };
            check_overlap(&path, &target, &metadata)?;
            transfer_entry(&path, &target, metadata, transfer)?;
        }
        transfer.advance(1, 0);
    } else {
        let name = source.file_name().ok_or(io::ErrorKind::InvalidInput)?;
        transfer_entry(&source, &destination.join(name), metadata, transfer)?;
    }
    Ok(())
}

fn transfer_selection(
    source: &Path,
    destination: &Path,
    selected: &[PathBuf],
    transfer: &mut Transfer,
) -> io::Result<()> {
    if selected.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Seleziona almeno un elemento.",
        ));
    }
    let (root_metadata, root, destination) = validate(source, destination)?;
    let mut entries = Vec::with_capacity(selected.len());
    for item in selected {
        let metadata = source_metadata(source, item).map_err(|error| at(item, error))?;
        let (input, relative) = if root_metadata.is_dir() && item != source {
            let relative = item
                .strip_prefix(source)
                .map_err(|_| io::ErrorKind::InvalidInput)?;
            (root.join(relative), relative)
        } else if !root_metadata.is_dir() && item == source {
            let name = root.file_name().ok_or(io::ErrorKind::InvalidInput)?;
            (root.clone(), Path::new(name))
        } else {
            return Err(io::ErrorKind::InvalidInput.into());
        };
        let target = destination.join(relative);
        if root_metadata.is_dir() && normalized(&target).starts_with(normalized(&root)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Un elemento finirebbe nella cartella di origine.",
            ));
        }
        check_overlap(&input, &target, &metadata)?;
        entries.push((relative, input, metadata));
    }
    entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
    if entries
        .windows(2)
        .any(|pair| pair[1].0.starts_with(pair[0].0))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "La selezione contiene elementi sovrapposti.",
        ));
    }

    transfer.progress.total = entries
        .iter()
        .map(|(_, path, metadata)| measure(path, metadata))
        .fold(0u64, u64::saturating_add);
    transfer.advance(0, 0);
    'entries: for (relative, input, metadata) in entries {
        let mut target = destination.clone();
        for part in relative.parent().into_iter().flat_map(Path::components) {
            target.push(part.as_os_str());
            if !ensure_directory(&target).map_err(|error| at(&target, error))? {
                transfer.skip(
                    &target,
                    "Il percorso di destinazione è occupato da un file o collegamento.",
                    measure(&input, &metadata),
                );
                continue 'entries;
            }
        }
        target.push(relative.file_name().ok_or(io::ErrorKind::InvalidInput)?);
        transfer_entry(&input, &target, metadata, transfer)?;
    }
    Ok(())
}

fn check_overlap(source: &Path, target: &Path, metadata: &Metadata) -> io::Result<()> {
    if metadata.is_dir() && !is_link(metadata) {
        let source = normalized(source);
        let target = normalized(target);
        if source.starts_with(&target) || target.starts_with(&source) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Origine e destinazione si sovrappongono.",
            ));
        }
    }
    Ok(())
}

fn at(path: &Path, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{}: {error}", path.display()))
}

fn ensure_directory(target: &Path) -> io::Result<bool> {
    match fs::create_dir(target) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let existing = fs::symlink_metadata(target)?;
            Ok(existing.is_dir() && !is_link(&existing))
        }
        Err(error) => Err(error),
    }
}

fn transfer_entry(
    source: &Path,
    target: &Path,
    metadata: Metadata,
    transfer: &mut Transfer,
) -> io::Result<()> {
    transfer_one(source, target, metadata, transfer).map_err(|error| at(source, error))
}

fn open_target(target: &Path, keep_both: bool) -> io::Result<Option<(File, PathBuf)>> {
    let create = |path: &Path| {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(FILE_SHARE_READ)
            .open(path)
    };
    let occupied = |path: &Path, error: &io::Error| {
        error.kind() == io::ErrorKind::AlreadyExists
            || (error.kind() == io::ErrorKind::PermissionDenied
                && fs::symlink_metadata(path).is_ok_and(|entry| entry.is_dir() || is_link(&entry)))
    };
    match create(target) {
        Ok(file) => return Ok(Some((file, target.to_owned()))),
        Err(error) if occupied(target, &error) => {
            if !keep_both {
                return Ok(None);
            }
        }
        Err(error) => return Err(error),
    }
    let stem = target.file_stem().ok_or(io::ErrorKind::InvalidInput)?;
    for index in 1..=10_000 {
        let mut name = stem.to_os_string();
        name.push(format!(" ({index})"));
        if let Some(extension) = target.extension() {
            name.push(".");
            name.push(extension);
        }
        let candidate = target.with_file_name(name);
        match create(&candidate) {
            Ok(file) => return Ok(Some((file, candidate))),
            Err(error) if occupied(&candidate, &error) => continue,
            Err(error) => return Err(at(&candidate, error)),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "Impossibile trovare un nome libero.",
    ))
}

fn remove_transferred_file(input: File, source: &Path) -> io::Result<()> {
    if !wsl_path(source) {
        const FILE_DISPOSITION_INFO: i32 = 4;
        if unsafe {
            SetFileInformationByHandle(input.as_raw_handle(), FILE_DISPOSITION_INFO, &1, 1)
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        return Ok(());
    }

    // Via WSL's redirector, delete-on-close can report success without unlinking a FUSE file.
    let original = FileInformation::read(&input)?;
    drop(input);
    let current = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(source)?;
    if is_link(&current.metadata()?) || !original.matches(&FileInformation::read(&current)?) {
        return Err(io::Error::other(
            "Il file di origine è cambiato durante il trasferimento.",
        ));
    }
    drop(current);
    fs::remove_file(source)?;
    match fs::symlink_metadata(source) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
        Ok(_) => Err(io::Error::other(
            "Il file è ancora presente nell'origine WSL.",
        )),
    }
}

fn transfer_one(
    source: &Path,
    target: &Path,
    metadata: Metadata,
    transfer: &mut Transfer,
) -> io::Result<()> {
    let operation = transfer.operation;
    if is_link(&metadata) {
        transfer.skip(source, "Collegamento all'origine.", 1);
    } else if metadata.is_dir() {
        if !ensure_directory(target).map_err(|error| at(target, error))? {
            transfer.skip(
                target,
                "La cartella di destinazione è occupata da un file o collegamento.",
                measure(source, &metadata),
            );
            return Ok(());
        }
        let entries = match fs::read_dir(source) {
            Err(error) if operation == Operation::Copy && wsl_permission_denied(source, &error) => {
                transfer.skip(source, "Accesso alla cartella di origine negato.", 1);
                return Ok(());
            }
            result => result?,
        };
        let skipped = transfer.report.skipped;
        for entry in entries.collect::<io::Result<Vec<_>>>()? {
            let metadata = match entry.metadata() {
                Err(error)
                    if operation == Operation::Copy && wsl_permission_denied(source, &error) =>
                {
                    transfer.skip(&entry.path(), "Accesso all'origine negato.", 1);
                    continue;
                }
                result => result?,
            };
            transfer_entry(
                &entry.path(),
                &target.join(entry.file_name()),
                metadata,
                transfer,
            )?;
        }
        if operation == Operation::Copy {
            transfer.report.folders += 1;
        } else if skipped == transfer.report.skipped {
            match fs::remove_dir(source) {
                Ok(()) => transfer.report.folders += 1,
                Err(error) if error.kind() == io::ErrorKind::DirectoryNotEmpty => {
                    transfer.skip(
                        source,
                        "La cartella di origine contiene ancora elementi.",
                        0,
                    );
                }
                Err(error) => return Err(error),
            }
        }
        transfer.advance(1, 0);
    } else if metadata.is_file() {
        let units = metadata.len().saturating_add(1);
        let mut input = if operation == Operation::Move {
            const DELETE: u32 = 0x1_0000;
            const GENERIC_READ: u32 = 0x8000_0000;
            OpenOptions::new()
                .access_mode(GENERIC_READ | DELETE)
                .share_mode(FILE_SHARE_READ)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                .open(source)?
        } else {
            match File::open(source) {
                Err(error) if wsl_permission_denied(source, &error) => {
                    transfer.skip(source, "Accesso al file di origine negato.", units);
                    return Ok(());
                }
                result => result?,
            }
        };
        if is_link(&input.metadata()?) {
            transfer.skip(source, "Collegamento all'origine.", units);
            return Ok(());
        }
        let Some((mut output, actual_target)) =
            open_target(target, transfer.keep_both).map_err(|error| at(target, error))?
        else {
            transfer.skip(
                target,
                "Esiste già un elemento con questo nome in destinazione.",
                units,
            );
            return Ok(());
        };
        let result = (|| -> io::Result<u64> {
            let mut bytes = 0;
            loop {
                let read = match input.read(&mut transfer.buffer) {
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    result => result?,
                };
                if read == 0 {
                    break;
                }
                output.write_all(&transfer.buffer[..read])?;
                bytes += read as u64;
                transfer.advance(read as u64, read as u64);
            }
            output.sync_all()?;
            Ok(bytes)
        })();
        match result {
            Ok(bytes) => {
                if operation == Operation::Move {
                    remove_transferred_file(input, source).map_err(|error| {
                        io::Error::other(format!(
                            "Copia completata, rimozione dall'origine non riuscita. {error}"
                        ))
                    })?;
                }
                transfer.report.files += 1;
                transfer.report.bytes += bytes;
                transfer.report.renamed += u64::from(actual_target != target);
                transfer.advance(1, 0);
            }
            Err(error) => {
                drop(output);
                let _ = fs::remove_file(&actual_target);
                return Err(at(&actual_target, error));
            }
        }
    } else {
        transfer.skip(source, "Tipo di elemento non trasferibile.", 1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(
        source: &Path,
        destination: &Path,
        selected: &[PathBuf],
        operation: Operation,
    ) -> io::Result<Report> {
        transfer(source, destination, selected, operation, false, &mut |_| {})
    }

    struct Fixture(PathBuf);

    impl Fixture {
        fn new(name: &str) -> io::Result<Self> {
            let path =
                std::env::temp_dir().join(format!("disk-copy-98-{}-{name}", std::process::id()));
            fs::create_dir(&path)?;
            Ok(Self(path))
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn selection_size_counts_nested_file_bytes() -> io::Result<()> {
        let fixture = Fixture::new("selection-size")?;
        let folder = fixture.0.join("folder");
        let nested = folder.join("nested");
        let file = fixture.0.join("selected.txt");
        fs::create_dir_all(&nested)?;
        fs::create_dir(folder.join("empty"))?;
        fs::write(nested.join("inside.txt"), b"12345")?;
        fs::write(&file, b"123")?;
        fs::write(fixture.0.join("ignored.txt"), b"ignored")?;

        let cancelled = AtomicBool::new(false);
        let size = selection_size(&fixture.0, &[folder, file.clone()], &cancelled);
        assert_eq!(size.bytes, 8);
        assert!(!size.incomplete);

        let size = selection_size(&file, std::slice::from_ref(&file), &cancelled);
        assert_eq!(size.bytes, 3);
        assert!(!size.incomplete);
        Ok(())
    }

    #[test]
    fn selection_size_reports_missing_items() -> io::Result<()> {
        let fixture = Fixture::new("incomplete-size")?;
        let file = fixture.0.join("selected.txt");
        fs::write(&file, b"123")?;

        let size = selection_size(
            &fixture.0,
            &[fixture.0.join("removed.txt"), file],
            &AtomicBool::new(false),
        );
        assert_eq!(size.bytes, 3);
        assert!(size.incomplete);
        Ok(())
    }

    #[test]
    fn copies_nested_files_and_preserves_existing_files() -> io::Result<()> {
        let fixture = Fixture::new("copy-all")?;
        let source = fixture.0.join("source");
        let destination = fixture.0.join("destination");
        let nested = source.join("nested");
        fs::create_dir_all(&nested)?;
        fs::create_dir(&destination)?;
        fs::write(nested.join("new.txt"), b"new")?;
        fs::write(source.join("existing.txt"), b"source")?;
        fs::write(destination.join("existing.txt"), b"keep")?;

        assert_eq!(
            run(&source, &nested, &[], Operation::Copy)
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        let report = run(&source, &destination, &[], Operation::Copy)?;
        assert_eq!((report.files, report.skipped, report.bytes), (1, 1, 3));
        assert_eq!(fs::read(destination.join("nested/new.txt"))?, b"new");
        assert_eq!(fs::read(destination.join("existing.txt"))?, b"keep");

        let report = run(&nested.join("new.txt"), &destination, &[], Operation::Copy)?;
        assert_eq!(report.files, 1);
        assert_eq!(fs::read(destination.join("new.txt"))?, b"new");
        Ok(())
    }

    #[test]
    fn moves_only_selected_items_and_leaves_conflicts_in_place() -> io::Result<()> {
        let fixture = Fixture::new("move-selection")?;
        let source = fixture.0.join("source");
        let destination = fixture.0.join("destination");
        let album = source.join("album");
        let drafts = source.join("drafts");
        let empty = source.join("empty");
        fs::create_dir_all(&album)?;
        fs::create_dir_all(&drafts)?;
        fs::create_dir_all(&empty)?;
        fs::create_dir_all(destination.join("album"))?;
        fs::write(album.join("a.txt"), b"a")?;
        fs::write(album.join("conflict.txt"), b"source")?;
        fs::write(destination.join("album/conflict.txt"), b"destination")?;
        fs::write(drafts.join("chosen.txt"), b"draft")?;
        fs::write(drafts.join("keep.txt"), b"keep")?;

        let selection = [album.clone(), drafts.join("chosen.txt"), empty.clone()];
        let report = run(&source, &destination, &selection, Operation::Move)?;
        assert_eq!((report.files, report.skipped, report.bytes), (2, 1, 6));
        assert!(report.skip_details[0].contains("Esiste già"));
        assert!(report.skip_details[0].ends_with("conflict.txt"));
        assert_eq!(report.folders, 1);
        assert!(!album.join("a.txt").exists());
        assert!(!drafts.join("chosen.txt").exists());
        assert!(!empty.exists());
        assert_eq!(fs::read(album.join("conflict.txt"))?, b"source");
        assert_eq!(fs::read(drafts.join("keep.txt"))?, b"keep");
        assert_eq!(fs::read(destination.join("album/a.txt"))?, b"a");
        assert_eq!(
            fs::read(destination.join("album/conflict.txt"))?,
            b"destination"
        );
        assert_eq!(fs::read(destination.join("drafts/chosen.txt"))?, b"draft");
        assert!(!destination.join("drafts/keep.txt").exists());
        assert!(destination.join("empty").is_dir());
        Ok(())
    }

    #[test]
    fn lists_files_and_copies_a_single_selection() -> io::Result<()> {
        let fixture = Fixture::new("copy-selection")?;
        let source = fixture.0.join("source");
        let destination = fixture.0.join("destination");
        fs::create_dir_all(source.join("folder"))?;
        fs::create_dir(&destination)?;
        let file = source.join("foto città.txt");
        fs::write(&file, b"photo")?;
        fs::write(source.join("other.txt"), b"other")?;
        let entries = list(&source, &source, true)?;
        assert_eq!(entries.len(), 3);
        assert!(entries[0].directory);
        let entries = list(&file, &file, true)?;
        assert_eq!(entries[0].path, file);
        assert_eq!(entries[0].length, 5);

        let report = run(
            &source,
            &destination,
            std::slice::from_ref(&file),
            Operation::Copy,
        )?;
        assert_eq!((report.files, report.bytes), (1, 5));
        assert_eq!(fs::read(&file)?, b"photo");
        assert_eq!(
            fs::read(destination.join(file.file_name().unwrap()))?,
            b"photo"
        );
        assert!(!destination.join("other.txt").exists());
        assert!(!destination.join("folder").exists());
        Ok(())
    }

    #[test]
    fn rejects_selection_that_would_move_items_inside_the_source() -> io::Result<()> {
        let fixture = Fixture::new("overlap")?;
        let source = fixture.0.join("source");
        let nested = source.join("source");
        fs::create_dir_all(&nested)?;
        let item = nested.join("stay.txt");
        fs::write(&item, b"stay")?;
        let error = run(&source, &fixture.0, &[item.clone()], Operation::Move)
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(fs::read(item)?, b"stay");
        assert!(!source.join("stay.txt").exists());
        Ok(())
    }

    #[test]
    fn reports_a_skipped_move_and_can_keep_both_with_byte_progress() -> io::Result<()> {
        let fixture = Fixture::new("conflict-progress")?;
        let source = fixture.0.join("source");
        let destination = fixture.0.join("destination");
        fs::create_dir(&source)?;
        fs::create_dir(&destination)?;
        let file = source.join("album.zip");
        let existing = destination.join("album.zip");
        let payload = vec![82u8; 3 * 1024 * 1024];
        fs::write(&file, &payload)?;
        fs::write(&existing, b"existing")?;

        let selection = std::slice::from_ref(&file);
        let skipped = run(&source, &destination, selection, Operation::Move)?;
        assert_eq!((skipped.files, skipped.skipped), (0, 1));
        assert!(skipped.skip_details[0].ends_with("album.zip"));
        assert!(file.exists());
        let mut updates = Vec::new();
        let report = transfer(
            &source,
            &destination,
            selection,
            Operation::Move,
            true,
            &mut |update| updates.push(update),
        )?;
        assert_eq!((report.files, report.renamed, report.skipped), (1, 1, 0));
        assert!(!file.exists());
        assert_eq!(fs::read(existing)?, b"existing");
        assert_eq!(fs::read(destination.join("album (1).zip"))?, payload);
        assert!(updates
            .iter()
            .any(|update| update.written > 0 && update.written < report.bytes));
        let last = updates.last().unwrap();
        assert_eq!(last.written, report.bytes);
        assert_eq!(last.processed, last.total);
        Ok(())
    }
}
