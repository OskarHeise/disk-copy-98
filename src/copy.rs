use std::{
    fs::{self, File, Metadata, OpenOptions},
    io,
    os::windows::{
        fs::{MetadataExt, OpenOptionsExt},
        io::{AsRawHandle, RawHandle},
    },
    path::{Component, Path, PathBuf},
};

#[link(name = "kernel32")]
extern "system" {
    fn SetFileInformationByHandle(file: RawHandle, class: i32, info: *const u8, size: u32) -> i32;
}

const FILE_SHARE_READ: u32 = 1;

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
}

fn is_link(metadata: &Metadata) -> bool {
    metadata.file_attributes() & 0x400 != 0
}

pub fn list(root: &Path, directory: &Path) -> io::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    let metadata = source_metadata(root, directory)?;
    if metadata.is_dir() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
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

pub fn copy(source: &Path, destination: &Path) -> io::Result<Report> {
    let (metadata, source, destination) = validate(source, destination)?;
    let mut report = Report::default();
    if metadata.is_dir() {
        for entry in fs::read_dir(&source)? {
            let entry = entry?;
            let path = entry.path();
            let target = destination.join(entry.file_name());
            let metadata = entry.metadata()?;
            check_overlap(&path, &target, &metadata)?;
            transfer_entry(&path, &target, metadata, Operation::Copy, &mut report)?;
        }
    } else {
        let name = source.file_name().ok_or(io::ErrorKind::InvalidInput)?;
        transfer_entry(
            &source,
            &destination.join(name),
            metadata,
            Operation::Copy,
            &mut report,
        )?;
    }
    Ok(report)
}

pub fn transfer_selection(
    source: &Path,
    destination: &Path,
    selected: &[PathBuf],
    operation: Operation,
) -> io::Result<Report> {
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

    let mut report = Report::default();
    'entries: for (relative, input, metadata) in entries {
        let mut target = destination.clone();
        for part in relative.parent().into_iter().flat_map(Path::components) {
            target.push(part.as_os_str());
            if !ensure_directory(&target).map_err(|error| at(&target, error))? {
                report.skipped += 1;
                continue 'entries;
            }
        }
        target.push(relative.file_name().ok_or(io::ErrorKind::InvalidInput)?);
        transfer_entry(&input, &target, metadata, operation, &mut report)?;
    }
    Ok(report)
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
    operation: Operation,
    report: &mut Report,
) -> io::Result<()> {
    transfer_one(source, target, metadata, operation, report).map_err(|error| at(source, error))
}

fn transfer_one(
    source: &Path,
    target: &Path,
    metadata: Metadata,
    operation: Operation,
    report: &mut Report,
) -> io::Result<()> {
    if is_link(&metadata) {
        report.skipped += 1;
    } else if metadata.is_dir() {
        if !ensure_directory(target).map_err(|error| at(target, error))? {
            report.skipped += 1;
            return Ok(());
        }
        let skipped = report.skipped;
        let entries = fs::read_dir(source)?.collect::<io::Result<Vec<_>>>()?;
        for entry in entries {
            transfer_entry(
                &entry.path(),
                &target.join(entry.file_name()),
                entry.metadata()?,
                operation,
                report,
            )?;
        }
        if operation == Operation::Copy {
            report.folders += 1;
        } else if skipped == report.skipped {
            match fs::remove_dir(source) {
                Ok(()) => report.folders += 1,
                Err(error) if error.kind() == io::ErrorKind::DirectoryNotEmpty => {
                    report.skipped += 1
                }
                Err(error) => return Err(error),
            }
        }
    } else if metadata.is_file() {
        let mut input = if operation == Operation::Move {
            const DELETE: u32 = 0x1_0000;
            const GENERIC_READ: u32 = 0x8000_0000;
            const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x20_0000;
            OpenOptions::new()
                .access_mode(GENERIC_READ | DELETE)
                .share_mode(FILE_SHARE_READ)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                .open(source)?
        } else {
            File::open(source)?
        };
        if is_link(&input.metadata()?) {
            report.skipped += 1;
            return Ok(());
        }
        let output = if operation == Operation::Move {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .share_mode(FILE_SHARE_READ)
                .open(target)
        } else {
            File::create_new(target)
        };
        let mut output = match output {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                report.skipped += 1;
                return Ok(());
            }
            Err(error) => return Err(at(target, error)),
        };
        let result = io::copy(&mut input, &mut output).and_then(|bytes| {
            output.sync_all()?;
            Ok(bytes)
        });
        match result {
            Ok(bytes) => {
                if operation == Operation::Move {
                    // Delete the opened file only after the new copy is safely written.
                    const FILE_DISPOSITION_INFO: i32 = 4;
                    if unsafe {
                        SetFileInformationByHandle(
                            input.as_raw_handle(),
                            FILE_DISPOSITION_INFO,
                            &1,
                            1,
                        )
                    } == 0
                    {
                        return Err(io::Error::other(format!(
                            "Copia completata, rimozione dall'origine non riuscita: {}",
                            io::Error::last_os_error()
                        )));
                    }
                    drop(input);
                }
                report.files += 1;
                report.bytes += bytes;
            }
            Err(error) => {
                drop(output);
                let _ = fs::remove_file(target);
                return Err(at(target, error));
            }
        }
    } else {
        report.skipped += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
            copy(&source, &nested).err().unwrap().kind(),
            io::ErrorKind::InvalidInput
        );
        let report = copy(&source, &destination)?;
        assert_eq!((report.files, report.skipped, report.bytes), (1, 1, 3));
        assert_eq!(fs::read(destination.join("nested/new.txt"))?, b"new");
        assert_eq!(fs::read(destination.join("existing.txt"))?, b"keep");

        let report = copy(&nested.join("new.txt"), &destination)?;
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
        let report = transfer_selection(&source, &destination, &selection, Operation::Move)?;
        assert_eq!((report.files, report.skipped, report.bytes), (2, 1, 6));
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
        let entries = list(&source, &source)?;
        assert_eq!(entries.len(), 3);
        assert!(entries[0].directory);
        let entries = list(&file, &file)?;
        assert_eq!(entries[0].path, file);
        assert_eq!(entries[0].length, 5);

        let report = transfer_selection(
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
        let error = transfer_selection(&source, &fixture.0, &[item.clone()], Operation::Move)
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(fs::read(item)?, b"stay");
        assert!(!source.join("stay.txt").exists());
        Ok(())
    }
}
