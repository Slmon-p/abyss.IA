# Abyss — Guia completo de instalação e requisitos

Tudo que precisa estar na máquina para **rodar** e para **compilar/desenvolver** o Abyss
(cliente desktop nativo do Google Gemini, em Rust + egui — sem Chromium/Electron).

---

## A) Só quero RODAR o app (usuário final)

O `abyss.exe` é **standalone** (o runtime do MinGW é linkado estaticamente; `ldd` confirma que
ele só usa DLLs padrão do Windows). Requisitos mínimos:

| Requisito | Detalhe |
|---|---|
| **Windows 10/11 64-bit** | obrigatório |
| **PowerShell** | já vem no Windows — usado pelo modo Agente |
| **Internet** | para falar com a API do Gemini |
| **API Key do Gemini** | gere em <https://aistudio.google.com/apikey> |

Passos:
1. Tenha o arquivo **`target\release\abyss.exe`** (já compilado) — ou rode `build.bat`.
2. Crie os atalhos (Área de Trabalho + Menu Iniciar):
   ```bat
   powershell -ExecutionPolicy Bypass -File scripts\criar_atalhos.ps1
   ```
3. Abra pelo atalho **Abyss** (Área de Trabalho ou busca do Iniciar).
4. Em **⚙ Configurações**, cole sua **API Key**.

> ⚠️ Para a função **🔄 Auto-update Abyss** (o agente editar o próprio código) funcionar,
> a máquina precisa do **toolchain de compilação** da seção B, porque o app roda `cargo build`.
> O resto do app (Chat, Agente em outras pastas, memória) funciona só com o `.exe`.

---

## B) Quero COMPILAR / desenvolver

### 1. Rust (toolchain GNU)
Instale via rustup escolhendo o host **GNU** (não o MSVC):
```bat
rustup-init.exe -y --default-host x86_64-pc-windows-gnu --default-toolchain stable
```
Confira:
```bat
rustc -vV          REM deve mostrar: host: x86_64-pc-windows-gnu
cargo --version
```

### 2. MSYS2 + GCC MinGW (o linker do target GNU)
Instale o **MSYS2** (https://www.msys2.org) e depois o gcc **mingw64** (variante msvcrt):
```bash
pacman -S --needed mingw-w64-x86_64-gcc
```
Resultado esperado: `gcc.exe` em **`C:\msys64\mingw64\bin`**.
> Não use o `ucrt64` — há conflito de CRT com o target padrão do Rust.

### 3. Linker fixado (já incluso no projeto)
O arquivo **`.cargo/config.toml`** já aponta o linker do MinGW, então o build funciona
mesmo sem mexer no PATH:
```toml
[target.x86_64-pc-windows-gnu]
linker = "C:/msys64/mingw64/bin/gcc.exe"
ar     = "C:/msys64/mingw64/bin/ar.exe"
```

### 4. Compilar e rodar
```bat
set PATH=%USERPROFILE%\.cargo\bin;C:\msys64\mingw64\bin;%PATH%
cargo build --release        REM ou build.bat
cargo run --release          REM ou run.bat
```
Binário final: **`target\release\abyss.exe`**.

### Versões validadas
- Rust `stable-x86_64-pc-windows-gnu` **1.96.0**
- GCC MinGW-w64 **16.x** (MSYS2)
- Crates: `eframe`/`egui` 0.28, `ureq` 2.x (TLS via `native-tls`/SChannel), `serde_json` 1,
  `image` 0.25 (ícone), build: `winresource` 0.1 (embute o ícone no `.exe`).

---

## C) Dependências por funcionalidade (resumo)

| Funcionalidade | Precisa de |
|---|---|
| Chat / Agente / Memória | só `abyss.exe` + internet + API Key |
| Modo Agente (comandos, abrir apps, Excel) | **PowerShell** (nativo do Windows) |
| Editar arquivos numa pasta | nada extra (é feito pelo próprio app) |
| **🔄 Auto-update Abyss** | **Rust (GNU) + GCC MinGW** (seção B) — o app roda `cargo build` |
| Embutir ícone ao compilar | `windres.exe`/`ar.exe` do MinGW (já vêm com o gcc) |

---

## D) Modelos do Gemini (grátis)

Na barra superior há o seletor **Gemini:**, dividido em dois grupos:

- **Flash — rápidos, cota maior:** `gemini-2.5-flash` (padrão), `gemini-2.5-flash-lite`,
  `gemini-2.0-flash`, `gemini-2.0-flash-lite`, `gemini-1.5-flash`, `gemini-1.5-flash-8b`.
- **Pro — raciocínio profundo, cota baixa:** `gemini-2.5-pro`, `gemini-1.5-pro`.
  São os modelos de raciocínio mais forte, mas a cota gratuita é bem menor
  (historicamente ~50 requisições/dia no 1.5 Pro).

> A camada gratuita tem **limite diário de requisições por modelo**. Cada passo do Agente é
> 1 requisição, então tarefas/auto-update longos esgotam a cota rápido — principalmente nos
> **Pro**. Troque de modelo no seletor (ex.: use Flash para tarefas longas e Pro para raciocínio
> pontual) ou gere uma key com billing.

---

## E) Onde ficam os dados

| Item | Local |
|---|---|
| Memória | `abyss_memory.json` (ao lado do `.exe`) |
| Cópia de auto-edição | `updateabyss/` (na raiz do projeto; ignorada pelo Git) |
| Atalhos | Área de Trabalho e `Menu Iniciar\Programs\Abyss.lnk` |

---

## F) Atalhos

Recriar a qualquer momento:
```bat
powershell -ExecutionPolicy Bypass -File scripts\criar_atalhos.ps1
```
Cria **Abyss** na Área de Trabalho e no Menu Iniciar (ícone próprio do app).
