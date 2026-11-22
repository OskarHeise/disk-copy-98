$ErrorActionPreference = 'Stop'
$project = Split-Path -Parent $PSScriptRoot
$cargo = Get-Command cargo.exe -ErrorAction SilentlyContinue
$cargoPath = if ($cargo) { $cargo.Source } else { Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe' }
$target = 'x86_64-pc-windows-gnu'
$targetDir = Join-Path $project 'target'
$builtExe = Join-Path $targetDir "$target\release\disk-copy-98.exe"
$packagedExe = Join-Path $project 'DiskCopy98.exe'
$iconFile = Join-Path $project 'assets\disk-copy-98.ico'

& $cargoPath "+stable-$target" rustc --manifest-path (Join-Path $project 'Cargo.toml') --target-dir $targetDir --target $target --release --locked -- -C target-feature=+crt-static -C strip=symbols
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;

public static class ExecutableIcon
{
    private const int Icon = 3;
    private const int GroupIcon = 14;
    private const int ApplicationIcon = 101;

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr BeginUpdateResource(string file, bool deleteExisting);

    [DllImport("kernel32.dll", EntryPoint = "UpdateResourceW", SetLastError = true)]
    private static extern bool UpdateResource(IntPtr update, IntPtr type, IntPtr name,
        ushort language, byte[] data, uint length);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern bool EndUpdateResource(IntPtr update, bool discard);

    private static void WriteResource(IntPtr update, int type, int id, byte[] data)
    {
        if (!UpdateResource(update, (IntPtr)type, (IntPtr)id, 0, data, (uint)data.Length))
            throw new Win32Exception();
    }

    public static void Embed(string executable, string iconFile)
    {
        const int headerSize = 6;
        const int fileEntrySize = 16;
        const int groupEntrySize = 14;
        const int imageMetadataSize = 12;

        byte[] ico = File.ReadAllBytes(iconFile);
        using (var reader = new BinaryReader(new MemoryStream(ico)))
        {
            if (reader.ReadUInt16() != 0 || reader.ReadUInt16() != 1)
                throw new InvalidDataException("Invalid ICO header.");

            int count = reader.ReadUInt16();
            if (count == 0 || ico.Length < headerSize + count * fileEntrySize)
                throw new InvalidDataException("Invalid ICO directory.");

            var group = new byte[headerSize + count * groupEntrySize];
            Buffer.BlockCopy(ico, 0, group, 0, headerSize);

            IntPtr update = BeginUpdateResource(executable, false);
            if (update == IntPtr.Zero)
                throw new Win32Exception();

            bool discard = true;
            try
            {
                for (int index = 0; index < count; index++)
                {
                    int fileEntry = headerSize + index * fileEntrySize;
                    int groupEntry = headerSize + index * groupEntrySize;
                    reader.BaseStream.Position = fileEntry + 8;
                    int length = reader.ReadInt32();
                    int offset = reader.ReadInt32();
                    if (length <= 0 || offset < headerSize + count * fileEntrySize ||
                        offset > ico.Length || length > ico.Length - offset)
                        throw new InvalidDataException("Invalid ICO image.");

                    int id = index + 1;
                    Buffer.BlockCopy(ico, fileEntry, group, groupEntry, imageMetadataSize);
                    Buffer.BlockCopy(BitConverter.GetBytes((ushort)id), 0, group,
                        groupEntry + imageMetadataSize, sizeof(ushort));

                    var image = new byte[length];
                    Buffer.BlockCopy(ico, offset, image, 0, length);
                    WriteResource(update, Icon, id, image);
                }
                WriteResource(update, GroupIcon, ApplicationIcon, group);
                discard = false;
            }
            finally
            {
                if (!EndUpdateResource(update, discard) && !discard)
                    throw new Win32Exception();
            }
        }
    }
}
'@

$stagedExe = Join-Path $targetDir ([Guid]::NewGuid().ToString('N') + '.exe')
try {
    Copy-Item -LiteralPath $builtExe -Destination $stagedExe
    [ExecutableIcon]::Embed($stagedExe, $iconFile)
    Move-Item -LiteralPath $stagedExe -Destination $packagedExe -Force
    Get-Item -LiteralPath $packagedExe
} finally {
    if (Test-Path -LiteralPath $stagedExe) { Remove-Item -LiteralPath $stagedExe }
}
