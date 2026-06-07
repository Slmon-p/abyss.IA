# Abyss — Cliente Desktop Nativo para o Google Gemini

Aplicativo desktop **nativo, leve e sem Chromium** para conversar com o Google Gemini.
Dois modos:

- **💬 IA Normal** — chat de texto comum (dúvidas, gerar texto, etc.).
- **🤖 Agente Local** — você dá uma ordem em linguagem natural e a IA **executa comandos
  reais no seu PC** (abrir programas, criar pastas, preencher planilha no Excel, automatizar
  teclado…), passo a passo, lendo a saída de cada comando e decidindo o próximo.

---

## 1. Stack escolhida e por quê (RAM baixíssima, sem Electron)

| Camada | Tecnologia | Por quê |
|---|---|---|
| **Linguagem** | **Rust** | Compilado para binário nativo, sem runtime/GC. Uso de RAM previsível e baixo. |
| **Frontend (GUI)** | **egui / eframe** (renderer **glow** = OpenGL) | GUI em **modo imediato**, **sem WebView e sem Chromium**. **Processo único**, ocioso em **~120 MB de Working Set** nesta máquina — e boa parte disso é memória **compartilhada** do driver OpenGL/GDI, não heap do app (Electron sobe 3–5 processos e costuma passar de 300–500 MB no total). Binário único, sem dependências de navegador. |
| **HTTP** | **ureq** (bloqueante) | Cliente HTTP minúsculo, sem runtime async pesado. |
| **TLS** | **native-tls → SChannel** | Usa o **TLS do próprio Windows**. Zero biblioteca de criptografia em C/Rust para compilar e carregar — menos RAM e build mais simples. |
| **JSON** | **serde_json** | Padrão da indústria. |

**Por que não Tauri?** Tauri é ótimo e leve *para web*, mas ainda usa o **WebView2 (engine do Edge/Chromium)**
para renderizar a tela. O requisito era "nada de Chromium". egui renderiza direto via OpenGL, então
não há engine de navegador nenhuma no processo.

**Por que não C++/Qt ou ImGui?** Entregariam RAM parecida, mas Rust dá **memory-safety**, gerenciamento
de dependências trivial (`cargo`) e um binário único — sem DLLs do Qt para distribuir.

### Arquitetura (o que fica onde)

```
┌──────────────────────────── Processo único (abyss.exe) ─────────────────────────────┐
│                                                                                         │
│  FRONTEND (thread principal / UI)            BACKEND (threads de trabalho)              │
│  ───────────────────────────────             ────────────────────────────              │
│  egui/eframe desenha a janela:               • call_gemini()  → HTTPS p/ a API Gemini   │
│   • seletor Normal / Agente                  • run_powershell() → executa no SO         │
│   • área de mensagens (ScrollArea)           • laço do Agente (decide → executa →       │
│   • campo de input + botão Enviar              lê saída → próximo passo)                 │
│                                                                                         │
│        envia tarefa  ─────────────►  std::thread + canal mpsc  ─────────────►           │
│        recebe updates ◄───────────  (não trava a UI; ctx.request_repaint())  ◄──────    │
└─────────────────────────────────────────────────────────────────────────────────────────┘
```

- A **UI nunca bloqueia**: toda chamada de rede / execução de comando roda numa **thread**
  separada, que devolve resultados pela `std::sync::mpsc` e pede repaint da tela.
- **Modo Normal:** monta o histórico → `POST .../models/<modelo>:generateContent` → mostra o texto.
- **Modo Agente:** o Gemini responde em **JSON estruturado** (`responseSchema`) com
  `{ explanation, powershell, task_complete }`. O app executa o `powershell`, devolve a saída
  ao modelo e repete até `task_complete=true` (máx. 8 passos). **É assim que o Gemini "mexe" no PC:**
  ele não toca no SO diretamente — ele *gera o comando*, e o app é quem executa via PowerShell
  (que no Windows abre programas, cria arquivos, controla o Excel por COM, envia teclas, etc.).

Código relevante em `src/main.rs`:
- UI: `impl eframe::App for App` (função `update`) e `draw_msg`.
- API: `call_gemini`, `contents_from`, `extract_text`.
- SO: `run_powershell`.
- Orquestração: `spawn_chat`, `spawn_agent`.

---

## 2. O que precisa estar instalado na máquina

> Já foi tudo instalado nesta máquina. Esta seção é para **reproduzir em outro PC**.

### Para **rodar** o `.exe` já compilado
O `abyss.exe` é **standalone** — `ldd` confirma que ele só usa DLLs padrão do Windows
(kernel32, opengl32, ws2_32, etc.); o runtime do MinGW é linkado estaticamente, então
**não precisa de nenhuma DLL extra**. Basta:
- **Windows 10/11 (64-bit)**.
- **PowerShell** (vem com o Windows) — necessário para o modo Agente.
- **Conexão com a internet** e uma **API Key do Google Gemini**.

Ou seja: pode copiar só o `abyss.exe` para qualquer Windows 64-bit e rodar.

### Para **compilar** do zero
1. **Rust (toolchain GNU)** — instalada via rustup:
   ```
   rustup-init.exe -y --default-host x86_64-pc-windows-gnu --default-toolchain stable
   ```
   (host precisa ser `x86_64-pc-windows-gnu`, confira com `rustc -vV`).
2. **MSYS2 + GCC MinGW (msvcrt)** — o linker do target GNU:
   ```
   pacman -S --needed mingw-w64-x86_64-gcc
   ```
   Esperado: `gcc.exe` em `C:\msys64\mingw64\bin` (NÃO use o `ucrt64`, há conflito de CRT).
3. O arquivo `.cargo/config.toml` deste projeto já fixa o linker:
   ```toml
   [target.x86_64-pc-windows-gnu]
   linker = "C:/msys64/mingw64/bin/gcc.exe"
   ar     = "C:/msys64/mingw64/bin/ar.exe"
   ```

#### Versões usadas e validadas
- Rust `stable-x86_64-pc-windows-gnu` (1.96.0)
- GCC MinGW-w64 16.x (MSYS2)
- crates: `eframe`/`egui` 0.28, `ureq` 2.x, `native-tls` 0.2, `serde_json` 1.x

---

## 3. Como compilar e rodar

No diretório do projeto (`E:\Hellsing`):

```bat
:: garanta que cargo e o gcc do mingw estão no PATH
set PATH=%USERPROFILE%\.cargo\bin;C:\msys64\mingw64\bin;%PATH%

cargo run --release
```

Ou use os atalhos prontos: **`build.bat`** (compila) e **`run.bat`** (compila se preciso e abre).

O binário final fica em: `target\release\abyss.exe`.

---

## 4. Configurar a API Key

1. Abra o app → botão **⚙** (canto superior direito).
2. Cole sua **API Key** do Gemini (gere em <https://aistudio.google.com/apikey>).
3. (Opcional) Troque o **Modelo** — padrão `gemini-2.5-flash` (gratuito).

> O app tenta a key como `?key=...` e, se a API recusar (400/401/403), tenta de novo como
> `Authorization: Bearer ...` — cobre tanto API Key clássica quanto token OAuth.
> Há uma key de teste pré-preenchida em `src/main.rs` (`DEFAULT_API_KEY`); troque pela sua.

---

## 5. Usando

**IA Normal:** digite e aperte **Enter** (ou botão **Enviar**).

**Agente Local:** descreva a tarefa. Exemplos:
- "olá" → ele só responde.
- "abra a calculadora"
- "crie uma pasta chamada Projetos na área de trabalho"
- "crie uma planilha no desktop chamada equipe.xlsx com os nomes Ana, Bruno e Carla na coluna A"
- "abra o bloco de notas e escreva 'teste automático'"

> ⚠️ **Segurança:** no modo Agente o app executa comandos REAIS. Há um botão
> **"Executar comandos automaticamente"** — desligue-o para ver o comando que o
> modelo gerou **sem** rodá-lo (modo seguro / revisão).

---

## 6. Memória persistente (`abyss_memory.json`)

O Abyss lembra de fatos e instruções suas entre sessões.

- **Para salvar:** comece (ou termine) a mensagem com um gatilho, por exemplo
  **"salve isso na memória: …"**. Outros gatilhos aceitos: *salva/salvar na memória,
  guarde na memória, anote na memória, grave na memória, memorize isso, lembre-se disso*.
  O app extrai **a lógica do que você pediu** e grava — sem gastar chamada de API.
  - Ex.: `salve isso na memória: sempre me responda em português e de forma curta`
  - Ex.: `meu nome é Simon, guarde na memória`
- **Onde fica:** um JSON ao lado do executável → `abyss_memory.json`:
  ```json
  { "memories": [ { "id": 1, "ts": 1780871899, "text": "sempre responda curto" } ] }
  ```
- **Como é usado:** a cada mensagem (Chat **e** Agente), todas as memórias são injetadas
  na `systemInstruction` enviada ao Gemini — então ele realmente lembra e respeita.
- **Gerenciar:** em **⚙ Configurações** há a seção **🧠 Memória (N)** para ver cada item,
  remover com **✕** ou **Limpar tudo**.

---

## 7. Agente Dev: pasta de trabalho, edição de arquivos e auto-edição

No modo **🤖 Agente Local** o agente agora é um agente de desenvolvimento.

### Pasta de trabalho
- Campo **Pasta:** define onde o agente atua. Botão **📁** abre o seletor de pastas do Windows.
- Tudo que o agente lê/escreve/executa acontece **dentro dessa pasta** (caminhos relativos;
  ele não consegue subir de pasta nem usar caminhos absolutos — proteção `safe_join`).

### Edição de qualquer arquivo
A cada passo o Gemini responde em JSON com uma **ação**:
- `read_file` (path) — lê um arquivo para entender antes de editar;
- `write_file` (path + content) — cria/sobrescreve **qualquer** arquivo de texto com o conteúdo completo;
- `run` (powershell) — executa um comando na pasta de trabalho;
- `finish` — encerra com um resumo.

O app executa a ação, devolve o resultado ao modelo e ele decide o próximo passo (até 16 passos).
Ex.: *"crie um index.html simples com um título Olá"*, *"abra o main.py e troque a porta 8000 por 9000"*.

### 🔄 Auto-update Abyss (o agente edita o próprio código com segurança)
Escreva no campo **o que** mudar no Abyss e clique em **🔄 Auto-update Abyss**. O fluxo:
1. **Salva no Git** o projeto atual (`git add/commit/push`).
2. **Copia** o projeto para `updateabyss/` (ignorando `.git`, `target`, `abyss_memory.json`).
3. O **agente edita** os arquivos **dentro da cópia** (read_file/write_file).
4. Roda **`cargo build`** na cópia.
5. Se **compilar** → **promove** os arquivos novos para o projeto principal e mantém a cópia.
   Depois é só **fechar o Abyss e rodar `run.bat`** para usar a versão nova.
6. Se **não compilar** → **não promove nada** e **mantém `updateabyss/`**. Clique de novo em
   🔄 (ou peça *"corrija os erros"*): como a cópia já existe, ele **retoma** dela, lê os erros do
   `cargo build` e continua iterando **até compilar** (build incremental, bem mais rápido).

> Ou seja: ele nunca te entrega código que não roda — no máximo continua ajustando. A cópia
> `updateabyss/` é mantida para você pedir mais ajustes; ela é ignorada pelo Git.
