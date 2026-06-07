# Cria atalhos do Abyss na Área de Trabalho e no Menu Iniciar.
# Uso:  powershell -ExecutionPolicy Bypass -File scripts\criar_atalhos.ps1

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot          # pasta raiz do projeto
$exe  = Join-Path $root "target\release\abyss.exe"
$ico  = Join-Path $root "assets\abyss.ico"
$work = Split-Path -Parent $exe

if (-not (Test-Path $exe)) {
    Write-Host "abyss.exe nao encontrado. Compile primeiro:  cargo build --release  (ou build.bat)" -ForegroundColor Yellow
    exit 1
}

function New-AbyssShortcut($linkPath) {
    $ws = New-Object -ComObject WScript.Shell
    $s  = $ws.CreateShortcut($linkPath)
    $s.TargetPath       = $exe
    $s.WorkingDirectory = $work
    $s.IconLocation     = "$ico,0"
    $s.Description       = "Abyss - cliente desktop do Google Gemini"
    $s.Save()
    Write-Host "OK -> $linkPath" -ForegroundColor Green
}

# 1) Area de Trabalho
$desktop = [Environment]::GetFolderPath('Desktop')
New-AbyssShortcut (Join-Path $desktop "Abyss.lnk")

# 2) Menu Iniciar (tela inicial / busca do Windows)
$startMenu = [Environment]::GetFolderPath('Programs')
New-AbyssShortcut (Join-Path $startMenu "Abyss.lnk")

Write-Host "`nAtalhos criados. Procure por 'Abyss' no Iniciar ou use o icone da Area de Trabalho." -ForegroundColor Cyan
