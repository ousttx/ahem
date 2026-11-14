@echo off
rem Starts the app in development mode (with the project-local Rust toolchain if there is one).
cd /d "%~dp0.."
call "%~dp0with-rust.cmd" npm run tauri dev
