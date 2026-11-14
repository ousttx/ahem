@echo off
rem Runs a command with the project-local Rust toolchain in ..\.rust when there is one, else with
rem the Rust already installed (rustup). The system PATH is not changed.
rem Example: tools\with-rust.cmd npm run tauri dev
setlocal
set "RUST_ROOT=%~dp0..\.rust"
if not exist "%RUST_ROOT%\cargo\bin" goto cmake
set "RUSTUP_HOME=%RUST_ROOT%\rustup"
set "CARGO_HOME=%RUST_ROOT%\cargo"
set "PATH=%CARGO_HOME%\bin;%PATH%"
:cmake
rem libopus (Opus in file transcription) builds with CMake: without one on PATH, take the CMake
rem of Visual Studio Build Tools.
where cmake >nul 2>nul && goto run
set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
for /f "usebackq delims=" %%i in (`"%VSWHERE%" -latest -products * -find Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe`) do set "PATH=%%~dpi;%PATH%"
:run
%*
exit /b %ERRORLEVEL%
