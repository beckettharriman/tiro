@echo off
rem Builds tiro-gpu-worker.exe with the Vulkan backend and puts it next to
rem tiro.exe for the bundler (tauri.windows.conf.json runs this as its
rem beforeBundleCommand; BUILDING.md "GPU builds").
rem
rem The worker gets its own short target directory: whisper.cpp builds its
rem Vulkan shader generator as a nested CMake project, and MSVC's file
rem tracker fails (FTK1011) once those paths pass MAX_PATH, which they do
rem under <repo>\src-tauri\target with room to spare.
setlocal
set "SHORT=%SystemDrive%\tiro-gpu"
if not defined CARGO_TARGET_DIR set "CARGO_TARGET_DIR=%~dp0..\src-tauri\target"
cargo build --release --manifest-path "%~dp0..\src-tauri\Cargo.toml" --bin tiro-gpu-worker --features gpu --target-dir "%SHORT%" || exit /b 1
if not exist "%CARGO_TARGET_DIR%\release\" mkdir "%CARGO_TARGET_DIR%\release\" || exit /b 1
copy /Y "%SHORT%\release\tiro-gpu-worker.exe" "%CARGO_TARGET_DIR%\release\" >nul || exit /b 1
echo tiro-gpu-worker.exe: built in %SHORT%, copied to %CARGO_TARGET_DIR%\release\
