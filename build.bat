@echo off
REM Compila o Hellsing em modo release.
set PATH=%USERPROFILE%\.cargo\bin;C:\msys64\mingw64\bin;%PATH%
cargo build --release
if %ERRORLEVEL%==0 (
  echo.
  echo OK - binario em: target\release\abyss.exe
)
