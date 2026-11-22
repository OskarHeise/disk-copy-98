@echo off
setlocal
set "app=%~dp0DiskCopy98.exe"
if exist "%app%" goto launch

set "app=%~dp0target\release\disk-copy-98.exe"
if exist "%app%" goto launch

echo Eseguibile assente. Compilazione in corso...
set "cargo=cargo.exe"
if exist "%USERPROFILE%\.cargo\bin\cargo.exe" set "cargo=%USERPROFILE%\.cargo\bin\cargo.exe"
"%cargo%" +stable-x86_64-pc-windows-gnu build --release --manifest-path "%~dp0Cargo.toml" --target-dir "%~dp0target"
if errorlevel 1 (
    echo Compilazione non riuscita.
    pause
    exit /b 1
)

:launch
start "" "%app%"
