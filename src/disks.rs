use std::{
    io,
    os::windows::process::CommandExt,
    path::PathBuf,
    process::{Command, Output},
};

const LIST_DISKS: &str = r#"
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$disks = @(Storage\Get-Disk)
$withLetter = @{}
$readable = @{}
Storage\Get-Volume | ForEach-Object {
    if ($_.DriveLetter -and $_.FileSystem -and $_.FileSystem -notin @('RAW', 'Unknown')) {
        $readable[[string]$_.DriveLetter] = $true
    }
}
if ($disks | Where-Object { $_.NumberOfPartitions -gt 0 }) {
    Storage\Get-Partition | ForEach-Object {
        if ($readable.ContainsKey([string]$_.DriveLetter)) { $withLetter[$_.DiskNumber] += [string]$_.DriveLetter }
    }
}
foreach ($disk in $disks) {
    if ($withLetter.ContainsKey($disk.Number)) {
        if ($disk.BusType -eq 'USB') { "USB`t" + $withLetter[$disk.Number] }
        continue
    }
    $name = ([string]$disk.FriendlyName -replace '[\x00-\x1f]', ' ').Trim()
    $disk.Number, $disk.BusType, $disk.PartitionStyle, ([int]$disk.IsOffline), $name, $disk.Size, ([int]($disk.IsBoot -or $disk.IsSystem)), ([string]$disk.UniqueId).Trim() -join "`t"
}
"#;

#[derive(Clone)]
pub struct Disk {
    pub number: u32,
    pub size: u64,
    pub identity: String,
    pub label: String,
    pub notice: String,
    pub usb: bool,
    pub mountable: bool,
}

#[derive(Default)]
pub struct Inventory {
    pub without_drive_letters: Vec<Disk>,
    pub usb_drive_letters: String,
}

fn parse_disk(line: &str) -> Option<Disk> {
    let mut fields = line.splitn(8, '\t');
    let number: u32 = fields.next()?.parse().ok()?;
    let bus = fields.next()?;
    let partition = fields.next()?;
    let offline = fields.next()?;
    let name = fields.next()?;
    let size = fields.next()?.parse().ok()?;
    let protected = fields.next()? != "0";
    let identity = fields.next()?.to_owned();
    let mountable = !protected && size > 0 && !identity.is_empty();
    let name = if name.is_empty() {
        format!("Disco {number}")
    } else {
        format!("Disco {number} · {name}")
    };
    let reason = match (offline, partition) {
        ("1", _) => "offline",
        (_, "RAW") => "RAW",
        _ => "senza lettera di unità",
    };
    let label = format!("{name} ({bus}, {reason})");
    let detail = if mountable {
        "Premi Mostra file per aprire il disco tramite WSL."
    } else {
        "Windows non espone un volume da sfogliare."
    };
    Some(Disk {
        number,
        size,
        identity,
        notice: format!("{label}. {detail}"),
        label,
        usb: bus == "USB",
        mountable,
    })
}

pub fn hidden(path: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(path);
    command.creation_flags(0x0800_0000);
    command
}

pub fn powershell(script: &str, variables: &[(&str, &str)]) -> io::Result<Output> {
    let windows = std::env::var_os("SystemRoot").ok_or(io::ErrorKind::NotFound)?;
    let powershell = PathBuf::from(windows).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    hidden(powershell)
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .envs(variables.iter().copied())
        .output()
}

pub fn scan() -> io::Result<Inventory> {
    let result = powershell(LIST_DISKS, &[])?;
    if !result.status.success() {
        return Err(io::Error::other(String::from_utf8_lossy(&result.stderr)));
    }
    let output = String::from_utf8(result.stdout).map_err(io::Error::other)?;
    let mut inventory = Inventory::default();
    for line in output.lines() {
        if let Some(letters) = line.strip_prefix("USB\t") {
            inventory.usb_drive_letters.push_str(letters);
        } else if let Some(disk) = parse_disk(line) {
            inventory.without_drive_letters.push(disk);
        }
    }
    Ok(inventory)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_a_raw_usb_disk_without_supplying_a_path() {
        let disk = parse_disk("1\tUSB\tRAW\t0\tSAMSUNG HD103UJ\t1000204886016\t0\tserial").unwrap();
        assert!(disk.usb);
        assert_eq!(disk.label, "Disco 1 · SAMSUNG HD103UJ (USB, RAW)");
        assert!(!std::path::Path::new(&disk.label).is_absolute());
        assert!(disk.notice.contains("Premi Mostra file"));
    }
}
