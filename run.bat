@echo off
REM Compila (se preciso) e abre o Hellsing.
set PATH=%USERPROFILE%\.cargo\bin;C:\msys64\mingw64\bin;%PATH%
cargo run --release
