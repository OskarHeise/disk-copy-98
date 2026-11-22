use crate::disks::{self, Disk};
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Child, Output, Stdio},
};

const SCRIPT: &str = include_str!("wsl_mount.sh");
const CHECK_DISK: &str = r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$disk = Storage\Get-Disk -Number ([uint32]$env:DC98_DISK)
if ($disk.IsBoot -or $disk.IsSystem -or $disk.Size -ne [uint64]$env:DC98_SIZE -or
    ([string]$disk.UniqueId).Trim() -ne $env:DC98_IDENTITY) {
    throw 'Il disco selezionato è cambiato.'
}
if ($disk.NumberOfPartitions -gt 0) {
    foreach ($partition in (Storage\Get-Partition -DiskNumber $disk.Number)) {
        if ($partition.AccessPaths -match '^[A-Z]:\\') {
            $volume = $partition | Storage\Get-Volume -ErrorAction SilentlyContinue
            if ($volume.FileSystem -and $volume.FileSystem -notin @('RAW', 'Unknown')) {
                throw 'Il disco ha volumi montati in Windows. Usa il percorso di Windows.'
            }
        }
    }
}
"#;
const USB_PARENT: &str = r#"
$id = (Get-CimInstance Win32_DiskDrive -Filter "Index=$($disk.Number)").PNPDeviceID
while ($id) {
    if ($id -match '^USB\\VID_[0-9a-f]{4}&PID_[0-9a-f]{4}\\') {
        $id
        exit
    }
    $id = (Get-PnpDeviceProperty -InstanceId $id -KeyName DEVPKEY_Device_Parent -ErrorAction SilentlyContinue).Data
}
throw 'Impossibile identificare il dispositivo USB in Windows.'
"#;
const USB_STATE: &str = r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$json = & $env:DC98_USB state
if ($LASTEXITCODE -ne 0) { throw 'Impossibile leggere lo stato di usbipd-win.' }
$usb = @(($json | ConvertFrom-Json).Devices | Where-Object {
    $_.InstanceId -eq $env:DC98_USB_ID -and $_.BusId -match '^[1-9][0-9]?-[1-9][0-9]?$'
})
if ($usb.Count -eq 1) {
    $usb[0].BusId, ([int][bool]$usb[0].PersistedGuid), ([int][bool]$usb[0].ClientIPAddress) -join [char]9
}
"#;
const ELEVATE: &str = r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    & ([scriptblock]::Create($env:DC98_ELEVATED))
    exit $LASTEXITCODE
}
$payload = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($env:DC98_ELEVATED))
$ps = Join-Path $PSHOME 'powershell.exe'
$child = Start-Process -FilePath $ps -ArgumentList "-NoProfile -NonInteractive -EncodedCommand $payload" -Verb RunAs -WindowStyle Hidden -Wait -PassThru
exit $child.ExitCode
"#;

pub struct Volume {
    pub path: PathBuf,
    pub label: String,
    pub readonly: bool,
    pub move_access: Option<MoveAccess>,
}

#[derive(Clone)]
pub struct MoveAccess {
    distro: String,
    mountpoint: String,
}

impl MoveAccess {
    pub fn run<T>(&self, transfer: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        checked(
            linux(&self.distro, &["begin-move", &self.mountpoint])?,
            "Impossibile aprire il volume in scrittura.",
        )?;
        let result = transfer();
        let close = linux(&self.distro, &["end-move", &self.mountpoint]).and_then(|output| {
            checked(
                output,
                "Impossibile chiudere la sessione di scrittura. Usa Espelli WSL.",
            )
        });
        match (result, close) {
            (Ok(report), Ok(_)) => Ok(report),
            (Err(error), Ok(_)) | (Ok(_), Err(error)) => Err(error),
            (Err(error), Err(close)) => Err(io::Error::other(format!("{error} {close}"))),
        }
    }
}

enum Attachment {
    Usb {
        program: PathBuf,
        identity: String,
        bus: String,
    },
    Physical(String),
}

pub struct MountedDisk {
    pub label: String,
    pub volumes: Vec<Volume>,
    distro: String,
    folder: Option<String>,
    attachment: Option<Attachment>,
    keep_alive: Child,
}

fn decode(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xff, 0xfe]) || bytes.iter().skip(1).step_by(2).take(16).any(|&b| b == 0)
    {
        let units = bytes
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect::<Vec<_>>();
        String::from_utf16_lossy(&units)
            .trim_start_matches('\u{feff}')
            .to_owned()
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

fn checked(output: Output, action: &str) -> io::Result<String> {
    if output.status.success() {
        return Ok(decode(&output.stdout));
    }
    let stderr = decode(&output.stderr);
    let stdout = decode(&output.stdout);
    let detail = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    let detail = detail.lines().next().unwrap_or_default();
    Err(io::Error::other(if detail.is_empty() {
        action.to_owned()
    } else {
        format!("{action} {detail}")
    }))
}

fn wsl() -> std::process::Command {
    let windows = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    disks::hidden(PathBuf::from(windows).join(r"System32\wsl.exe"))
}

fn distro() -> io::Result<String> {
    let output = checked(
        wsl().args(["--list", "--verbose"]).output()?,
        "WSL non è disponibile.",
    )?;
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim_end();
            let (name, rest) = line.get(2..)?.split_once("  ")?;
            let name = name.trim();
            if rest.split_whitespace().last() != Some("2")
                || name.is_empty()
                || name.starts_with("docker-desktop")
                || name.contains(['\\', '/', ':'])
            {
                return None;
            }
            Some((!line.starts_with('*'), name))
        })
        .min_by_key(|(non_default, _)| *non_default)
        .map(|(_, name)| name.to_owned())
        .ok_or_else(|| io::Error::other("Serve una distribuzione WSL 2, ad esempio Ubuntu."))
}

fn linux(distro: &str, args: &[&str]) -> io::Result<Output> {
    let mut child = wsl()
        .args(["-d", distro, "-u", "root", "--exec", "sh", "-s", "--"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let written = child
        .stdin
        .take()
        .unwrap()
        .write_all(SCRIPT.replace("\r\n", "\n").as_bytes());
    let output = child.wait_with_output()?;
    if output.status.success() {
        written?;
    }
    Ok(output)
}

fn usbipd() -> Option<PathBuf> {
    let program = PathBuf::from(std::env::var_os("ProgramFiles")?).join(r"usbipd-win\usbipd.exe");
    program.is_file().then_some(program)
}

fn install_usbipd() -> io::Result<PathBuf> {
    if let Some(program) = usbipd() {
        return Ok(program);
    }
    let failure =
        "Serve usbipd-win 5 o successivo. Installa dorssel.usbipd-win da WinGet e riprova.";
    let output = disks::hidden("winget.exe")
        .args([
            "install",
            "--id",
            "dorssel.usbipd-win",
            "--exact",
            "--silent",
            "--accept-package-agreements",
            "--accept-source-agreements",
        ])
        .output()
        .map_err(|_| io::Error::other(failure))?;
    checked(output, failure)?;
    usbipd().ok_or_else(|| io::Error::other(failure))
}

fn ps_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn with_disk(disk: &Disk, script: &str) -> String {
    format!(
        "$env:DC98_DISK='{}'; $env:DC98_SIZE='{}'; $env:DC98_IDENTITY={};\n{CHECK_DISK}\n{script}",
        disk.number,
        disk.size,
        ps_quote(&disk.identity)
    )
}

fn usb_state_script(program: &Path, identity: &str) -> String {
    format!(
        "$env:DC98_USB={}; $env:DC98_USB_ID={};\n{USB_STATE}",
        ps_quote(&program.to_string_lossy()),
        ps_quote(identity)
    )
}

fn elevate(script: &str, action: &str) -> io::Result<()> {
    let result = disks::powershell(ELEVATE, &[("DC98_ELEVATED", script)])?;
    checked(result, action)?;
    Ok(())
}

fn attach_usb(disk: &Disk, distro: &str) -> io::Result<(Attachment, String)> {
    let program = install_usbipd()?;
    let identity = checked(
        disks::powershell(&with_disk(disk, USB_PARENT), &[])?,
        "Impossibile verificare il disco USB.",
    )?;
    let identity = identity.trim();
    let state_script = usb_state_script(&program, identity);
    let read_state = || -> io::Result<(String, bool, bool)> {
        let output = checked(
            disks::powershell(&state_script, &[])?,
            "Controllo USB non riuscito.",
        )?;
        let fields: Vec<_> = output.trim().split('\t').collect();
        match fields.as_slice() {
            [bus, bound, attached] => Ok(((*bus).to_owned(), *bound == "1", *attached == "1")),
            _ => Err(io::Error::other(
                "Il dispositivo USB selezionato non è più presente.",
            )),
        }
    };
    let (bus, bound, attached) = read_state()?;
    if attached {
        return Err(io::Error::other(
            "Il dispositivo USB è già collegato a un'altra sessione.",
        ));
    }
    checked(
        linux(distro, &["prepare-usb"])?,
        "Preparazione del supporto USB in WSL non riuscita.",
    )?;
    if !bound {
        // Resolve the same USB instance again inside the administrator process.
        let script = format!(
            "$record = & {{ {state_script} }}\n\
             if (-not $record -or ($record -split [char]9)[0] -ne {}) {{ exit 1 }}\n\
             & $env:DC98_USB bind --busid {}\nexit $LASTEXITCODE",
            ps_quote(&bus),
            ps_quote(&bus)
        );
        elevate(
            &script,
            "Condivisione USB non riuscita. Conferma la richiesta di Windows e riprova.",
        )?;
    }
    if read_state()? != (bus.clone(), true, false) {
        return Err(io::Error::other("Lo stato del dispositivo USB è cambiato."));
    }
    let (hub, port) = bus.split_once('-').unwrap();
    let remote = format!(
        "{:04x}{:04x}",
        hub.parse::<u16>().unwrap(),
        port.parse::<u16>().unwrap()
    );
    checked(
        disks::hidden(&program)
            .args(["attach", "--wsl", "--busid", &bus])
            .output()?,
        "Collegamento USB a WSL non riuscito.",
    )?;
    Ok((
        Attachment::Usb {
            program,
            identity: identity.to_owned(),
            bus,
        },
        remote,
    ))
}

impl MountedDisk {
    pub fn open(disk: Disk) -> io::Result<Self> {
        if !disk.mountable {
            return Err(io::Error::other(disk.notice));
        }
        let distro = distro()?;
        let user = |flag| -> io::Result<String> {
            let id = checked(
                wsl().args(["-d", &distro, "--exec", "id", flag]).output()?,
                "Impossibile identificare l'utente Linux.",
            )?;
            id.trim()
                .parse::<u32>()
                .map(|id| id.to_string())
                .map_err(io::Error::other)
        };
        let (uid, gid) = (user("-u")?, user("-g")?);
        let keep_alive = wsl()
            .args(["-d", &distro, "--exec", "cat"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let mut mounted = Self {
            label: disk.label.clone(),
            volumes: Vec::new(),
            distro,
            folder: None,
            attachment: None,
            keep_alive,
        };
        let outcome = mounted.connect(&disk, &uid, &gid);
        if let Err(error) = outcome {
            return match mounted.close() {
                Ok(()) => Err(error),
                Err(cleanup) => Err(io::Error::other(format!("{error} {cleanup}"))),
            };
        }
        Ok(mounted)
    }

    fn connect(&mut self, disk: &Disk, uid: &str, gid: &str) -> io::Result<()> {
        let (remote, previous) = if disk.usb {
            let (attachment, remote) = attach_usb(disk, &self.distro)?;
            self.attachment = Some(attachment);
            (remote, String::new())
        } else {
            let previous = checked(
                linux(&self.distro, &["snapshot"])?,
                "Inventario Linux non disponibile.",
            )?
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
            let physical = format!(r"\\.\PHYSICALDRIVE{}", disk.number);
            let script = with_disk(
                disk,
                &format!(
                    r#"& "$env:SystemRoot\System32\wsl.exe" --mount {} --bare
exit $LASTEXITCODE"#,
                    ps_quote(&physical)
                ),
            );
            elevate(&script, "Collegamento disco a WSL non riuscito.")?;
            self.attachment = Some(Attachment::Physical(physical));
            ("-".to_owned(), previous)
        };
        let size = disk.size.to_string();
        let output = linux(
            &self.distro,
            &["mount", &size, &remote, &previous, uid, gid],
        )?;
        if !output.status.success() {
            let stderr = decode(&output.stderr);
            let detail = stderr.lines().last().unwrap_or_default();
            return Err(io::Error::other(format!(
                "{} · WSL non ha aperto il disco. {detail}",
                self.label
            )));
        }
        for line in decode(&output.stdout).lines() {
            let fields: Vec<_> = line.split('\t').collect();
            let [path, device, filesystem, mode] = fields.as_slice() else {
                continue;
            };
            let Some((base, volume)) = path.rsplit_once('/') else {
                continue;
            };
            if !base.starts_with("/mnt/wsl/disk-copy-98.") || !volume.starts_with("volume-") {
                continue;
            }
            self.folder = Some(base.to_owned());
            let move_access = (*mode == "move").then(|| MoveAccess {
                distro: self.distro.clone(),
                mountpoint: (*path).to_owned(),
            });
            let path = PathBuf::from(format!(
                r"\\wsl.localhost\{}{}",
                self.distro,
                path.replace('/', "\\")
            ));
            let readonly = *mode != "rw";
            self.volumes.push(Volume {
                path,
                label: format!(
                    "{} · {} ({filesystem}{})",
                    self.label,
                    device,
                    if move_access.is_some() {
                        ", copia e spostamento"
                    } else if readonly {
                        ", sola lettura"
                    } else {
                        ""
                    }
                ),
                readonly,
                move_access,
            });
        }
        if self.volumes.is_empty() {
            return Err(io::Error::other(
                "WSL non ha restituito un percorso di montaggio.",
            ));
        }
        Ok(())
    }

    pub fn close(&mut self) -> io::Result<()> {
        if let Some(folder) = &self.folder {
            checked(
                linux(&self.distro, &["unmount", folder])?,
                "Smontaggio Linux non riuscito.",
            )?;
            self.folder = None;
        }
        if let Some(attachment) = &self.attachment {
            match attachment {
                Attachment::Usb {
                    program,
                    identity,
                    bus,
                } => {
                    let state = checked(
                        disks::powershell(&usb_state_script(program, identity), &[])?,
                        "Controllo USB non riuscito.",
                    )?;
                    if state.trim() == format!("{bus}\t1\t1") {
                        checked(
                            disks::hidden(program)
                                .args(["detach", "--busid", bus])
                                .output()?,
                            "Scollegamento USB non riuscito.",
                        )?;
                    }
                }
                Attachment::Physical(path) => {
                    let script = format!(
                        r#"& "$env:SystemRoot\System32\wsl.exe" --unmount {}
exit $LASTEXITCODE"#,
                        ps_quote(path)
                    );
                    elevate(&script, "Scollegamento disco da WSL non riuscito.")?;
                }
            }
            self.attachment = None;
        }
        drop(self.keep_alive.stdin.take());
        let _ = self.keep_alive.wait();
        Ok(())
    }
}
