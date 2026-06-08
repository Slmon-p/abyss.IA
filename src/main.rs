// Abyss — cliente desktop nativo e leve para o Google Gemini.
//
// Backend (lógica): chamadas HTTP à API do Gemini + execução de comandos no SO.
// Frontend (UI):     egui/eframe (OpenGL, sem Chromium/WebView).
//
// Dois modos:
//   - IA Normal:    chat de texto comum.
//   - Agente Local: a IA recebe a tarefa, decide e EXECUTA comandos PowerShell reais
//                   no Windows (abrir programas, criar pastas, mexer no Excel, etc.),
//                   em laço passo-a-passo, lendo a saída de cada comando.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // sem janela de console no release

use eframe::egui;
use serde_json::json;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

// ---- Configuração padrão (pode ser trocada na UI, em ⚙ Configurações) ----
const DEFAULT_API_KEY: &str = "AQ.Ab8RN6KsIezTPxmcZCPV2ebOHVEaIxsM-DpmzQw_obsIeL4NSg";
const DEFAULT_MODEL: &str = "gemini-2.5-flash";

/// Modelos Flash — rápidos, cota gratuita maior.
const FLASH_MODELS: &[&str] = &[
    "gemini-2.5-flash",
    "gemini-2.5-flash-lite",
    "gemini-2.0-flash",
    "gemini-2.0-flash-lite",
    "gemini-1.5-flash",
    "gemini-1.5-flash-8b",
];

/// Modelos Pro — raciocínio profundo, cota gratuita baixa (~50/dia, historicamente, no 1.5 Pro).
const PRO_MODELS: &[&str] = &["gemini-2.5-pro", "gemini-1.5-pro"];
const MAX_AGENT_STEPS: usize = 16;

const CHAT_SYSTEM: &str = "Você é um assistente útil e direto. \
Responda sempre no idioma do usuário (português quando ele escrever em português). \
Seja claro e objetivo. SEMPRE coloque código, SQL, comandos ou qualquer trecho destinado a ser \
copiado dentro de um bloco markdown com três crases (```), indicando a linguagem \
(ex.: ```sql, ```txt, ```python, ```bash).";

const AGENT_SYSTEM: &str = r#"Você é um AGENTE DEV/AUTOMAÇÃO rodando na máquina Windows do usuário.
Você tem acesso ao COMPUTADOR INTEIRO (qualquer pasta/arquivo do Windows) e pode:
- ler arquivos (para entender antes de editar),
- criar/editar QUALQUER tipo de arquivo de texto (código, config, .md, .json, .html, etc.),
- executar comandos PowerShell.

Sobre caminhos:
- Use caminhos ABSOLUTOS do Windows para acessar qualquer lugar do PC (ex.: C:\Users\<voce>\Desktop\arquivo.txt). Descubra pastas com comandos: $env:USERPROFILE, [Environment]::GetFolderPath('Desktop'), Get-ChildItem.
- Caminhos RELATIVOS são resolvidos dentro da PASTA DE TRABALHO (abaixo) — prefira-os só quando o usuário pedir para trabalhar DENTRO de uma pasta específica.

Para CADA passo responda SOMENTE com um objeto JSON:
- "explanation": em português, 1-2 frases, o que fará neste passo (ou o resumo final).
- "action": "read_file" | "write_file" | "run" | "finish".
- "path": caminho RELATIVO à pasta de trabalho (para read_file e write_file).
- "content": o conteúdo COMPLETO e final do arquivo (apenas para write_file; sobrescreve o arquivo inteiro — NÃO use diffs/trechos).
- "powershell": o comando (apenas para action="run").
- "task_complete": true quando a tarefa inteira terminou.

Depois de cada passo você recebe o resultado (saída do comando, conteúdo do arquivo, ou confirmação de escrita) e decide o próximo.

Regras:
- Para EDITAR um arquivo: faça read_file antes, depois write_file com o conteúdo completo já alterado.
- Para CRIAR arquivo novo: write_file direto com o conteúdo.
- Comandos PowerShell NÃO interativos. Abrir programas: Start-Process (ex.: Start-Process notepad).
- Faça UM passo objetivo por vez. Não invente caminhos; use read_file ou "run" (ex.: Get-ChildItem) para descobrir.
- Se for só conversa/saudação ("olá"), use action="finish" e task_complete=true.
- Ao terminar, action="finish", path/content/powershell vazios, task_complete=true, e um resumo na "explanation"."#;

// ----------------------------- Modelo de dados da UI -----------------------------

#[derive(PartialEq, Eq, Clone, Copy)]
enum Mode {
    Chat,
    Agent,
}

#[derive(Clone, Copy)]
enum Role {
    User,
    Model,
    Cmd,
    Output,
    Error,
}

#[derive(Clone)]
struct Msg {
    role: Role,
    text: String,
}
impl Msg {
    fn new(role: Role, text: impl Into<String>) -> Self {
        Self { role, text: text.into() }
    }
}

#[derive(Default)]
struct ModeState {
    transcript: Vec<Msg>,             // o que aparece na tela
    history: Vec<(String, String)>,   // histórico para a API: (role, texto)  role = "user" | "model"
}

// Mensagens vindas das threads de trabalho para a UI.
enum WorkerMsg {
    Chat(String),
    ChatErr(String),
    AgentSay(String),
    AgentCmd(String),
    AgentOut(String),
    AgentErr(String),
    AgentDone(Vec<(String, String)>),
    WorkDir(String),
}

#[derive(Clone)]
struct MemoryEntry {
    id: u64,
    ts: u64,
    text: String,
}

struct App {
    mode: Mode,
    input: String,
    chat: ModeState,
    agent: ModeState,
    api_key: String,
    model: String,
    auto_run: bool,
    show_settings: bool,
    pending: bool,
    memory: Vec<MemoryEntry>,
    mem_path: std::path::PathBuf,
    next_mem_id: u64,
    work_dir: String,
    project_root: std::path::PathBuf,
    tx: mpsc::Sender<WorkerMsg>,
    rx: mpsc::Receiver<WorkerMsg>,
    http: ureq::Agent,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let (tx, rx) = mpsc::channel();
        let connector = native_tls::TlsConnector::new().expect("falha ao criar TLS (SChannel)");
        let http = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(20))
            .timeout_read(Duration::from_secs(180))
            .tls_connector(Arc::new(connector))
            .build();
        let mem_path = memory_path();
        let memory = load_memory(&mem_path);
        let next_mem_id = memory.iter().map(|m| m.id).max().unwrap_or(0);
        let project_root = find_project_root();
        let work_dir = project_root.to_string_lossy().to_string();
        Self {
            mode: Mode::Chat,
            input: String::new(),
            chat: ModeState::default(),
            agent: ModeState::default(),
            api_key: DEFAULT_API_KEY.to_string(),
            model: DEFAULT_MODEL.to_string(),
            auto_run: true,
            show_settings: false,
            pending: false,
            memory,
            mem_path,
            next_mem_id,
            work_dir,
            project_root,
            tx,
            rx,
            http,
        }
    }

    fn cur_mut(&mut self) -> &mut ModeState {
        match self.mode {
            Mode::Chat => &mut self.chat,
            Mode::Agent => &mut self.agent,
        }
    }

    fn clear_current(&mut self) {
        match self.mode {
            Mode::Chat => self.chat = ModeState::default(),
            Mode::Agent => self.agent = ModeState::default(),
        }
    }

    fn add_memory(&mut self, text: String) {
        self.next_mem_id += 1;
        self.memory.push(MemoryEntry {
            id: self.next_mem_id,
            ts: now_secs(),
            text,
        });
        save_memory(&self.mem_path, &self.memory);
    }

    fn remove_memory(&mut self, id: u64) {
        self.memory.retain(|m| m.id != id);
        save_memory(&self.mem_path, &self.memory);
    }

    fn clear_memory(&mut self) {
        self.memory.clear();
        save_memory(&self.mem_path, &self.memory);
    }

    /// Bloco com as memórias para injetar na systemInstruction.
    fn memory_preamble(&self) -> String {
        if self.memory.is_empty() {
            return String::new();
        }
        let mut s = String::from(
            "\n\nMEMÓRIA DO USUÁRIO (fatos e instruções que ele salvou; lembre-se e respeite sempre):\n",
        );
        for m in &self.memory {
            s.push_str("- ");
            s.push_str(&m.text);
            s.push('\n');
        }
        s
    }

    fn send(&mut self, ctx: &egui::Context) {
        let text = self.input.trim().to_string();
        if text.is_empty() || self.pending {
            return;
        }

        // MEMÓRIA: "salve isso na memória ..." → grava no JSON e confirma (sem chamar a API).
        if let Some(mem) = detect_memory_command(&text) {
            self.input.clear();
            self.add_memory(mem.clone());
            self.cur_mut()
                .transcript
                .push(Msg::new(Role::Model, format!("🧠 Salvo na memória: \"{mem}\"")));
            return;
        }

        if self.api_key.trim().is_empty() {
            self.cur_mut()
                .transcript
                .push(Msg::new(Role::Error, "Configure sua API Key em ⚙ Configurações."));
            return;
        }
        self.input.clear();

        let http = self.http.clone();
        let key = self.api_key.trim().to_string();
        let models = ordered_models(self.model.trim());
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        self.pending = true;

        match self.mode {
            Mode::Chat => {
                let system = format!("{CHAT_SYSTEM}{}", self.memory_preamble());
                self.chat.transcript.push(Msg::new(Role::User, text.clone()));
                self.chat.history.push(("user".into(), text));
                let history = self.chat.history.clone();
                spawn_chat(ctx2, tx, http, key, models, system, history);
            }
            Mode::Agent => {
                let work_dir = std::path::PathBuf::from(self.work_dir.trim());
                let system = format!(
                    "{AGENT_SYSTEM}\n\nPASTA DE TRABALHO: {}\n{}",
                    work_dir.display(),
                    self.memory_preamble()
                );
                self.agent.transcript.push(Msg::new(Role::User, text.clone()));
                self.agent.history.push(("user".into(), text));
                let history = self.agent.history.clone();
                let auto = self.auto_run;
                spawn_agent(ctx2, tx, http, key, models, system, history, work_dir, auto);
            }
        }
    }

    fn start_self_update(&mut self, ctx: &egui::Context) {
        if self.pending {
            return;
        }
        let instruction = self.input.trim().to_string();
        if instruction.is_empty() {
            self.agent.transcript.push(Msg::new(
                Role::Error,
                "Escreva no campo o que você quer mudar no Abyss e então clique em 🔄 Auto-update.",
            ));
            return;
        }
        if self.api_key.trim().is_empty() {
            self.agent
                .transcript
                .push(Msg::new(Role::Error, "Configure sua API Key em ⚙ Configurações."));
            return;
        }
        self.input.clear();
        self.agent
            .transcript
            .push(Msg::new(Role::User, format!("🔄 Auto-update: {instruction}")));
        self.pending = true;
        spawn_self_update(
            ctx.clone(),
            self.tx.clone(),
            self.http.clone(),
            self.api_key.trim().to_string(),
            ordered_models(self.model.trim()),
            self.memory_preamble(),
            instruction,
            self.project_root.clone(),
        );
    }

    fn drain(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                WorkerMsg::Chat(t) => {
                    self.chat.transcript.push(Msg::new(Role::Model, t.clone()));
                    self.chat.history.push(("model".into(), t));
                    self.pending = false;
                }
                WorkerMsg::ChatErr(e) => {
                    self.chat.transcript.push(Msg::new(Role::Error, e));
                    self.pending = false;
                }
                WorkerMsg::AgentSay(t) => self.agent.transcript.push(Msg::new(Role::Model, t)),
                WorkerMsg::AgentCmd(c) => self.agent.transcript.push(Msg::new(Role::Cmd, c)),
                WorkerMsg::AgentOut(o) => self.agent.transcript.push(Msg::new(Role::Output, o)),
                WorkerMsg::AgentErr(e) => self.agent.transcript.push(Msg::new(Role::Error, e)),
                WorkerMsg::AgentDone(h) => {
                    self.agent.history = h;
                    self.pending = false;
                }
                WorkerMsg::WorkDir(p) => self.work_dir = p,
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain();

        // ----- Topo: título, seletor de modo, ações, configurações -----
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading("Abyss");
                ui.separator();
                ui.selectable_value(&mut self.mode, Mode::Chat, "💬 IA Normal");
                ui.selectable_value(&mut self.mode, Mode::Agent, "🤖 Agente Local");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("🗑 Limpar").clicked() {
                        self.clear_current();
                    }
                    if ui.button("⚙").on_hover_text("Configurações").clicked() {
                        self.show_settings = !self.show_settings;
                    }
                    if self.pending {
                        ui.label("processando…");
                        ui.spinner();
                    }
                    ui.add_space(8.0);
                    egui::ComboBox::from_id_source("model_sel")
                        .selected_text(self.model.as_str())
                        .width(185.0)
                        .show_ui(ui, |ui| {
                            ui.label(egui::RichText::new("Flash — rápidos, cota maior").small().weak());
                            for m in FLASH_MODELS {
                                ui.selectable_value(&mut self.model, (*m).to_string(), *m);
                            }
                            ui.separator();
                            ui.label(
                                egui::RichText::new("Pro — raciocínio, cota baixa (~50/dia)")
                                    .small()
                                    .weak(),
                            );
                            for m in PRO_MODELS {
                                ui.selectable_value(&mut self.model, (*m).to_string(), *m);
                            }
                        });
                    ui.label(egui::RichText::new("Gemini:").weak());
                });
            });

            if self.mode == Mode::Agent {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.auto_run, "Executar automaticamente");
                    ui.label(
                        egui::RichText::new("⚠ roda comandos/edições REAIS")
                            .small()
                            .color(egui::Color32::from_rgb(220, 160, 60)),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("Pasta:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.work_dir)
                            .desired_width(330.0)
                            .hint_text("pasta onde o agente trabalha"),
                    );
                    if ui.button("📁").on_hover_text("Escolher pasta").clicked() {
                        spawn_folder_picker(ctx.clone(), self.tx.clone(), self.work_dir.clone());
                    }
                    if ui
                        .add_enabled(!self.pending, egui::Button::new("🔄 Auto-update Abyss"))
                        .on_hover_text(
                            "Salva no Git, edita uma cópia (updateabyss), compila e promove se passar.\n\
                             Escreva no campo de baixo O QUE mudar e clique aqui.",
                        )
                        .clicked()
                    {
                        self.start_self_update(ctx);
                    }
                });
            }

            if self.show_settings {
                ui.add_space(2.0);
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label("API Key:");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.api_key)
                                .password(true)
                                .desired_width(380.0),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("Modelo: ");
                        ui.add(egui::TextEdit::singleline(&mut self.model).desired_width(240.0));
                    });

                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label(format!("🧠 Memória ({})", self.memory.len()));
                        ui.label(
                            egui::RichText::new("— diga: \"salve isso na memória ...\"")
                                .small()
                                .weak(),
                        );
                        if !self.memory.is_empty() && ui.button("Limpar tudo").clicked() {
                            self.clear_memory();
                        }
                    });
                    let mut remove_id: Option<u64> = None;
                    egui::ScrollArea::vertical()
                        .max_height(150.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for m in &self.memory {
                                ui.horizontal(|ui| {
                                    if ui.small_button("✕").clicked() {
                                        remove_id = Some(m.id);
                                    }
                                    ui.label(egui::RichText::new(m.text.as_str()).small());
                                });
                            }
                        });
                    if let Some(id) = remove_id {
                        self.remove_memory(id);
                    }
                });
            }
            ui.add_space(4.0);
        });

        // ----- Rodapé: campo de entrada + botão enviar -----
        egui::TopBottomPanel::bottom("input").show(ctx, |ui| {
            ui.add_space(6.0);
            let hint = match self.mode {
                Mode::Chat => "Pergunte algo…  (Enter envia)",
                Mode::Agent => {
                    "Descreva a tarefa (ex.: crie uma pasta 'Projetos' na área de trabalho)…  (Enter envia)"
                }
            };
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.input)
                    .hint_text(hint)
                    .desired_width(f32::INFINITY),
            );
            // Enter envia a mensagem; mantém o foco para continuar digitando.
            let enter_send = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.add_space(4.0);
            let mut do_send = false;
            ui.horizontal(|ui| {
                if ui.add_enabled(!self.pending, egui::Button::new("Enviar  ➤")).clicked() {
                    do_send = true;
                }
                let modo = match self.mode {
                    Mode::Chat => "IA Normal",
                    Mode::Agent => "Agente Local",
                };
                ui.label(egui::RichText::new(format!("modo: {modo}  ·  {}", self.model)).weak());
            });
            if (do_send || enter_send) && !self.pending {
                self.send(ctx);
                resp.request_focus();
            }
            ui.add_space(6.0);
        });

        // ----- Centro: transcrição da conversa -----
        egui::CentralPanel::default().show(ctx, |ui| {
            let transcript = match self.mode {
                Mode::Chat => &self.chat.transcript,
                Mode::Agent => &self.agent.transcript,
            };
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for m in transcript {
                        draw_msg(ui, m);
                    }
                });
        });
    }
}

enum Segment {
    Text(String),
    Code { lang: String, body: String },
}

/// Divide o texto em trechos normais e blocos de código (cercas ```...```).
fn parse_segments(s: &str) -> Vec<Segment> {
    let mut segs = Vec::new();
    let mut text = String::new();
    let mut code = String::new();
    let mut lang = String::new();
    let mut in_code = false;
    for line in s.split_inclusive('\n') {
        let core = line.trim_end_matches(|c| c == '\n' || c == '\r');
        if core.trim_start().starts_with("```") {
            if in_code {
                segs.push(Segment::Code {
                    lang: std::mem::take(&mut lang),
                    body: std::mem::take(&mut code),
                });
                in_code = false;
            } else {
                if !text.is_empty() {
                    segs.push(Segment::Text(std::mem::take(&mut text)));
                }
                lang = core.trim_start().trim_start_matches("```").trim().to_string();
                in_code = true;
            }
        } else if in_code {
            code.push_str(line);
        } else {
            text.push_str(line);
        }
    }
    if in_code {
        segs.push(Segment::Code { lang, body: code }); // cerca não fechada
    } else if !text.is_empty() {
        segs.push(Segment::Text(text));
    }
    segs
}

/// Caixa de código monoespaçada com cabeçalho (linguagem) e botão Copiar.
fn code_block(ui: &mut egui::Ui, body: &str, lang: Option<&str>) {
    egui::Frame::none()
        .fill(egui::Color32::from_rgb(16, 18, 22))
        .stroke(egui::Stroke::new(1.0, egui::Color32::from_gray(58)))
        .rounding(egui::Rounding::same(5.0))
        .inner_margin(egui::Margin::same(8.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                let l = lang
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .unwrap_or("texto");
                ui.label(egui::RichText::new(l).small().weak());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("📋 Copiar").clicked() {
                        ui.output_mut(|o| o.copied_text = body.to_string());
                    }
                });
            });
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(body)
                    .monospace()
                    .color(egui::Color32::from_gray(225)),
            );
        });
    ui.add_space(4.0);
}

fn draw_msg(ui: &mut egui::Ui, m: &Msg) {
    let (label, label_color, bg, mono) = match m.role {
        Role::User => ("Você", egui::Color32::from_rgb(120, 180, 255), egui::Color32::from_rgb(33, 42, 54), false),
        Role::Model => ("Gemini", egui::Color32::from_rgb(150, 220, 150), egui::Color32::from_rgb(30, 34, 40), false),
        Role::Cmd => ("▶ PowerShell", egui::Color32::from_rgb(255, 200, 120), egui::Color32::from_rgb(42, 35, 22), true),
        Role::Output => ("⤷ Saída", egui::Color32::from_rgb(170, 170, 170), egui::Color32::from_rgb(22, 24, 26), true),
        Role::Error => ("Erro", egui::Color32::from_rgb(255, 120, 120), egui::Color32::from_rgb(48, 26, 26), false),
    };
    egui::Frame::none()
        .fill(bg)
        .rounding(egui::Rounding::same(6.0))
        .inner_margin(egui::Margin::same(8.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(egui::RichText::new(label).strong().color(label_color));
            if mono {
                // Comando/saída do agente: já é um bloco monoespaçado com Copiar.
                let hint = if matches!(m.role, Role::Cmd) { "powershell" } else { "saída" };
                code_block(ui, m.text.trim_end(), Some(hint));
            } else {
                // Texto do modelo: separa blocos ``` em caixas com Copiar.
                for seg in parse_segments(&m.text) {
                    match seg {
                        Segment::Text(t) => {
                            let t = t.trim_matches(|c| c == '\n' || c == '\r');
                            if !t.trim().is_empty() {
                                ui.label(
                                    egui::RichText::new(t).color(egui::Color32::from_gray(228)),
                                );
                            }
                        }
                        Segment::Code { lang, body } => {
                            let body = body.trim_end_matches(|c| c == '\n' || c == '\r');
                            code_block(ui, body, Some(&lang));
                        }
                    }
                }
            }
        });
    ui.add_space(6.0);
}

// ----------------------------- Backend: API + execução -----------------------------

fn contents_from(history: &[(String, String)]) -> serde_json::Value {
    serde_json::Value::Array(
        history
            .iter()
            .map(|(role, text)| json!({ "role": role, "parts": [{ "text": text }] }))
            .collect(),
    )
}

fn extract_text(v: &serde_json::Value) -> Option<String> {
    let parts = v
        .get("candidates")?
        .get(0)?
        .get("content")?
        .get("parts")?
        .as_array()?;
    let mut s = String::new();
    for p in parts {
        if let Some(t) = p.get("text").and_then(|x| x.as_str()) {
            s.push_str(t);
        }
    }
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

// Chama o Gemini. Tenta com ?key=, e em caso de 400/401/403 tenta como Bearer
// (cobre tanto API key clássica quanto token OAuth).
fn call_gemini(
    http: &ureq::Agent,
    key: &str,
    model: &str,
    body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let base = format!("https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent");
    let url_key = format!("{base}?key={key}");

    let first = http.post(&url_key).send_json(body.clone());
    let retry_auth = matches!(&first, Err(ureq::Error::Status(400 | 401 | 403, _)));

    let resp = if retry_auth {
        http.post(&base)
            .set("Authorization", &format!("Bearer {key}"))
            .send_json(body)
    } else {
        first
    };

    match resp {
        Ok(r) => r
            .into_json::<serde_json::Value>()
            .map_err(|e| format!("Resposta inválida: {e}")),
        Err(ureq::Error::Status(code, r)) => {
            let t = r.into_string().unwrap_or_default();
            Err(format!("HTTP {code}: {t}"))
        }
        Err(e) => Err(format!("Erro de rede: {e}")),
    }
}

/// Lista de modelos a tentar, começando pelo selecionado, depois os Flash e por fim os Pro.
fn ordered_models(selected: &str) -> Vec<String> {
    let mut v = vec![selected.to_string()];
    for m in FLASH_MODELS.iter().chain(PRO_MODELS.iter()) {
        if *m != selected {
            v.push((*m).to_string());
        }
    }
    v
}

/// Tenta os modelos em ordem; se um falhar (cota/erro), passa para o próximo até um responder.
fn call_gemini_fallback(
    http: &ureq::Agent,
    key: &str,
    models: &[String],
    body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let mut last = String::from("nenhum modelo disponível");
    for model in models {
        match call_gemini(http, key, model, body.clone()) {
            Ok(v) => {
                if v.get("candidates").and_then(|c| c.get(0)).is_some() {
                    return Ok(v);
                }
                last = format!(
                    "{model}: {}",
                    v.get("error")
                        .map(|e| e.to_string())
                        .unwrap_or_else(|| "resposta sem candidatos".to_string())
                );
            }
            Err(e) => last = format!("{model}: {e}"),
        }
    }
    Err(format!("Todos os modelos falharam — último: {last}"))
}

// ----------------------------- Memória (JSON persistente) -----------------------------

fn memory_path() -> std::path::PathBuf {
    let mut dir = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    dir.push("abyss_memory.json");
    dir
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn load_memory(path: &std::path::Path) -> Vec<MemoryEntry> {
    let data = std::fs::read_to_string(path).unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(&data).unwrap_or_else(|_| json!({}));
    let mut out = Vec::new();
    if let Some(arr) = v.get("memories").and_then(|x| x.as_array()) {
        for m in arr {
            let text = m
                .get("text")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if text.is_empty() {
                continue;
            }
            out.push(MemoryEntry {
                id: m.get("id").and_then(|x| x.as_u64()).unwrap_or(0),
                ts: m.get("ts").and_then(|x| x.as_u64()).unwrap_or(0),
                text,
            });
        }
    }
    out
}

fn save_memory(path: &std::path::Path, mems: &[MemoryEntry]) {
    let arr: Vec<serde_json::Value> = mems
        .iter()
        .map(|m| json!({ "id": m.id, "ts": m.ts, "text": m.text }))
        .collect();
    let v = json!({ "memories": arr });
    let _ = std::fs::write(path, serde_json::to_string_pretty(&v).unwrap_or_default());
}

/// Busca case-insensitive que devolve (início, fim) em bytes na string ORIGINAL.
fn ci_find(haystack: &str, needle: &str) -> Option<(usize, usize)> {
    let h = haystack.to_lowercase();
    let n = needle.to_lowercase();
    let bpos = h.find(&n)?;
    let char_start = h[..bpos].chars().count();
    let char_end = char_start + n.chars().count();
    let start = haystack.char_indices().nth(char_start).map(|(i, _)| i).unwrap_or(0);
    let end = haystack
        .char_indices()
        .nth(char_end)
        .map(|(i, _)| i)
        .unwrap_or(haystack.len());
    Some((start, end))
}

/// Detecta "salve isso na memória ..." e devolve só o conteúdo (a lógica) a memorizar.
fn detect_memory_command(text: &str) -> Option<String> {
    const TRIGGERS: &[&str] = &[
        "salve isso na memória",
        "salve isso na memoria",
        "salva isso na memória",
        "salva isso na memoria",
        "salvar isso na memória",
        "salvar isso na memoria",
        "salve na memória",
        "salve na memoria",
        "salva na memória",
        "salva na memoria",
        "salvar na memória",
        "salvar na memoria",
        "guarde na memória",
        "guarde na memoria",
        "adicione à memória",
        "adicione a memoria",
        "anote na memória",
        "anote na memoria",
        "grave na memória",
        "grave na memoria",
        "memorize isso",
        "memoriza isso",
        "lembre-se disso",
        "lembre disso",
    ];
    let lower = text.to_lowercase();
    let trigger = TRIGGERS.iter().find(|t| lower.contains(**t))?;
    let (start, end) = ci_find(text, trigger)?;

    let mut content = String::new();
    content.push_str(text[..start].trim());
    if !content.is_empty() {
        content.push(' ');
    }
    content.push_str(text[end..].trim());

    let content = content
        .trim()
        .trim_start_matches(|c: char| matches!(c, ':' | '-' | '—' | ',' | '.' | ' '))
        .trim_end_matches(|c: char| matches!(c, ',' | ';' | ' '))
        .trim();
    let content = content.strip_prefix("que ").unwrap_or(content).trim();
    if content.is_empty() {
        None
    } else {
        Some(content.to_string())
    }
}

// ----------------------------- Sistema de arquivos / projeto -----------------------------

/// Resolve `p`: se for ABSOLUTO, usa direto (acesso ao PC inteiro);
/// se for RELATIVO, resolve dentro de `base` (a pasta de trabalho).
fn resolve_path(base: &std::path::Path, p: &str) -> std::path::PathBuf {
    let pp = std::path::Path::new(p.trim());
    if pp.is_absolute() {
        pp.to_path_buf()
    } else {
        base.join(pp)
    }
}

fn write_file_in(base: &std::path::Path, rel: &str, content: &str) -> String {
    let path = resolve_path(base, rel);
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return format!("ERRO ao criar pasta de {rel}: {e}");
        }
    }
    match std::fs::write(&path, content) {
        Ok(_) => format!("OK: {} bytes gravados em {}", content.len(), path.display()),
        Err(e) => format!("ERRO ao gravar {}: {e}", path.display()),
    }
}

fn read_file_in(base: &std::path::Path, rel: &str) -> String {
    let path = resolve_path(base, rel);
    match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => format!("ERRO ao ler {}: {e}", path.display()),
    }
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…(truncado)", &s[..end])
}

const SKIP_NAMES: &[&str] = &[".git", "target", "updateabyss", "abyss_memory.json"];

fn copy_tree(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if SKIP_NAMES.contains(&name.to_string_lossy().as_ref()) {
            continue;
        }
        let from = entry.path();
        let to = dst.join(&name);
        if from.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            if let Some(p) = to.parent() {
                std::fs::create_dir_all(p)?;
            }
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

fn promote_tree(src: &std::path::Path, dst: &std::path::Path, count: &mut usize) -> std::io::Result<()> {
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if SKIP_NAMES.contains(&name.to_string_lossy().as_ref()) {
            continue;
        }
        let from = entry.path();
        let to = dst.join(&name);
        if from.is_dir() {
            std::fs::create_dir_all(&to)?;
            promote_tree(&from, &to, count)?;
        } else {
            if let Some(p) = to.parent() {
                std::fs::create_dir_all(p)?;
            }
            std::fs::copy(&from, &to)?;
            *count += 1;
        }
    }
    Ok(())
}

/// `cargo build` (debug) dentro de `dir`. Retorna (compilou, log).
fn build_dir(dir: &std::path::Path) -> (bool, String) {
    let out = std::process::Command::new("powershell")
        .current_dir(dir)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            "$env:Path = \"$env:USERPROFILE\\.cargo\\bin;C:\\msys64\\mingw64\\bin;$env:Path\"; cargo build 2>&1 | Out-String; exit $LASTEXITCODE",
        ])
        .output();
    match out {
        Ok(o) => {
            let mut log = String::from_utf8_lossy(&o.stdout).to_string();
            let se = String::from_utf8_lossy(&o.stderr);
            if !se.trim().is_empty() {
                log.push_str(&se);
            }
            (o.status.success(), log)
        }
        Err(e) => (false, format!("Falha ao iniciar cargo: {e}")),
    }
}

fn find_project_root() -> std::path::PathBuf {
    let start = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|p| p.to_path_buf()))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let mut d = start.clone();
    loop {
        if d.join("Cargo.toml").exists() {
            return d;
        }
        match d.parent() {
            Some(p) => d = p.to_path_buf(),
            None => return std::env::current_dir().unwrap_or(start),
        }
    }
}

fn run_powershell(script: &str, work_dir: &std::path::Path) -> String {
    let wrapped = format!(
        "$OutputEncoding=[Console]::OutputEncoding=[Text.Encoding]::UTF8; {script}"
    );
    let mut cmd = std::process::Command::new("powershell");
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        &wrapped,
    ]);
    if work_dir.is_dir() {
        cmd.current_dir(work_dir);
    }
    let out = cmd.output();
    match out {
        Ok(o) => {
            let so = String::from_utf8_lossy(&o.stdout);
            let se = String::from_utf8_lossy(&o.stderr);
            let mut s = String::new();
            if !so.trim().is_empty() {
                s.push_str(so.trim_end());
            }
            if !se.trim().is_empty() {
                if !s.is_empty() {
                    s.push('\n');
                }
                s.push_str("[stderr] ");
                s.push_str(se.trim_end());
            }
            let code = o.status.code().unwrap_or(-1);
            if s.trim().is_empty() {
                format!("(ok, sem saída — exit {code})")
            } else {
                format!("{s}\n(exit {code})")
            }
        }
        Err(e) => format!("Falha ao iniciar o PowerShell: {e}"),
    }
}

fn spawn_chat(
    ctx: egui::Context,
    tx: mpsc::Sender<WorkerMsg>,
    http: ureq::Agent,
    key: String,
    models: Vec<String>,
    system: String,
    history: Vec<(String, String)>,
) {
    thread::spawn(move || {
        let body = json!({
            "contents": contents_from(&history),
            "systemInstruction": { "parts": [{ "text": system }] },
            "generationConfig": { "temperature": 0.7 }
        });
        let msg = match call_gemini_fallback(&http, &key, &models, body) {
            Ok(v) => match extract_text(&v) {
                Some(t) => WorkerMsg::Chat(t),
                None => WorkerMsg::ChatErr(format!("Sem resposta utilizável da API: {v}")),
            },
            Err(e) => WorkerMsg::ChatErr(e),
        };
        let _ = tx.send(msg);
        ctx.request_repaint();
    });
}

/// Núcleo do agente: loop de passos (read_file / write_file / run) na pasta de trabalho.
/// Devolve true se terminou normalmente; false se houve erro de API (já reportado).
#[allow(clippy::too_many_arguments)]
fn run_agent_loop(
    ctx: &egui::Context,
    tx: &mpsc::Sender<WorkerMsg>,
    http: &ureq::Agent,
    key: &str,
    models: &[String],
    system: &str,
    history: &mut Vec<(String, String)>,
    work_dir: &std::path::Path,
    auto_run: bool,
    max_steps: usize,
) -> bool {
    let schema = json!({
        "type": "object",
        "properties": {
            "explanation": { "type": "string" },
            "action": { "type": "string", "enum": ["run", "write_file", "read_file", "finish"] },
            "path": { "type": "string" },
            "content": { "type": "string" },
            "powershell": { "type": "string" },
            "task_complete": { "type": "boolean" }
        },
        "required": ["explanation", "action", "task_complete"]
    });

    for _ in 0..max_steps {
        let body = json!({
            "contents": contents_from(history),
            "systemInstruction": { "parts": [{ "text": system }] },
            "generationConfig": {
                "temperature": 0.2,
                "responseMimeType": "application/json",
                "responseSchema": schema
            }
        });

        let v = match call_gemini_fallback(http, key, models, body) {
            Ok(v) => v,
            Err(e) => {
                let _ = tx.send(WorkerMsg::AgentErr(e));
                ctx.request_repaint();
                return false;
            }
        };
        let raw = match extract_text(&v) {
            Some(t) => t,
            None => {
                let _ = tx.send(WorkerMsg::AgentErr(format!("Sem resposta utilizável: {v}")));
                ctx.request_repaint();
                return false;
            }
        };
        history.push(("model".into(), raw.clone()));

        let parsed: serde_json::Value = serde_json::from_str(&raw)
            .unwrap_or_else(|_| json!({ "explanation": raw, "action": "finish", "task_complete": true }));
        let expl = parsed.get("explanation").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let action = parsed.get("action").and_then(|x| x.as_str()).unwrap_or("finish").to_string();
        let path = parsed.get("path").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let content = parsed.get("content").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let ps = parsed.get("powershell").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let done = parsed.get("task_complete").and_then(|x| x.as_bool()).unwrap_or(false);

        if !expl.trim().is_empty() {
            let _ = tx.send(WorkerMsg::AgentSay(expl));
            ctx.request_repaint();
        }

        let mut acted = false;
        match action.as_str() {
            "write_file" if !path.trim().is_empty() => {
                acted = true;
                let _ = tx.send(WorkerMsg::AgentCmd(format!("✏ write_file  {path}  ({} bytes)", content.len())));
                ctx.request_repaint();
                let result = write_file_in(work_dir, &path, &content);
                let _ = tx.send(WorkerMsg::AgentOut(result.clone()));
                ctx.request_repaint();
                history.push(("user".into(), format!("Resultado de write_file {path}: {result}. Próximo passo ou finalize.")));
            }
            "read_file" if !path.trim().is_empty() => {
                acted = true;
                let _ = tx.send(WorkerMsg::AgentCmd(format!("📖 read_file  {path}")));
                ctx.request_repaint();
                let data = read_file_in(work_dir, &path);
                let _ = tx.send(WorkerMsg::AgentOut(truncate_str(&data, 3000)));
                ctx.request_repaint();
                history.push(("user".into(), format!("Conteúdo de {path}:\n{}", truncate_str(&data, 16000))));
            }
            "run" if !ps.trim().is_empty() => {
                acted = true;
                let _ = tx.send(WorkerMsg::AgentCmd(format!("▶ {ps}")));
                ctx.request_repaint();
                if !auto_run {
                    let _ = tx.send(WorkerMsg::AgentOut(
                        "⏸ Execução automática DESLIGADA — comando não executado.".into(),
                    ));
                    ctx.request_repaint();
                    return true;
                }
                let output = run_powershell(&ps, work_dir);
                let _ = tx.send(WorkerMsg::AgentOut(output.clone()));
                ctx.request_repaint();
                history.push(("user".into(), format!("Saída do comando:\n{output}\n\nPróximo passo ou finalize.")));
            }
            _ => {}
        }

        if !acted || done {
            break;
        }
    }
    true
}

fn spawn_agent(
    ctx: egui::Context,
    tx: mpsc::Sender<WorkerMsg>,
    http: ureq::Agent,
    key: String,
    models: Vec<String>,
    system: String,
    mut history: Vec<(String, String)>,
    work_dir: std::path::PathBuf,
    auto_run: bool,
) {
    thread::spawn(move || {
        run_agent_loop(
            &ctx, &tx, &http, &key, &models, &system, &mut history, &work_dir, auto_run, MAX_AGENT_STEPS,
        );
        let _ = tx.send(WorkerMsg::AgentDone(history));
        ctx.request_repaint();
    });
}

/// Auto-edição do próprio Abyss: push → cópia `updateabyss` → o agente edita →
/// `cargo build` → promove se compilar; se não, mantém a cópia para iterar depois.
fn spawn_self_update(
    ctx: egui::Context,
    tx: mpsc::Sender<WorkerMsg>,
    http: ureq::Agent,
    key: String,
    models: Vec<String>,
    memory_block: String,
    instruction: String,
    project_root: std::path::PathBuf,
) {
    thread::spawn(move || {
        let say = |s: String| {
            let _ = tx.send(WorkerMsg::AgentSay(s));
            ctx.request_repaint();
        };
        let out = |s: String| {
            let _ = tx.send(WorkerMsg::AgentOut(s));
            ctx.request_repaint();
        };
        let err = |s: String| {
            let _ = tx.send(WorkerMsg::AgentErr(s));
            ctx.request_repaint();
        };

        let update_dir = project_root.join("updateabyss");
        let resuming = update_dir.exists();

        let seed = if resuming {
            // Já existe uma cópia — continua iterando nela (mantém mudanças e o cache de build).
            say("📂 'updateabyss' já existe — retomando a iteração nela (build incremental).".into());
            let (ok0, log0) = build_dir(&update_dir);
            if ok0 {
                format!(
                    "A cópia já compila. Aplique o pedido a seguir mantendo o projeto compilável. Pedido: {instruction}"
                )
            } else {
                format!(
                    "A cópia ainda NÃO compila. Corrija os ERROS de compilação abaixo e também atenda ao pedido.\n\
                     Pedido: {instruction}\n\nERROS:\n{}",
                    truncate_str(&log0, 6000)
                )
            }
        } else {
            // Primeira vez: salva no Git e cria a cópia.
            say("🔄 Auto-update: enviando o projeto atual ao GitHub…".into());
            let push = run_powershell(
                "git add -A; git commit -m \"snapshot antes do auto-update\" 2>&1 | Out-String; git push origin main 2>&1 | Out-String",
                &project_root,
            );
            out(truncate_str(&push, 2000));

            say(format!("📁 Criando a cópia de trabalho: {}", update_dir.display()));
            if let Err(e) = copy_tree(&project_root, &update_dir) {
                err(format!("Falha ao copiar o projeto: {e}"));
                let _ = tx.send(WorkerMsg::AgentDone(vec![]));
                ctx.request_repaint();
                return;
            }
            format!(
                "Você está editando uma CÓPIA do projeto Abyss (app Rust/egui em src/main.rs). \
                 Faça a alteração pedida editando os arquivos necessários (use read_file e write_file com o conteúdo COMPLETO). \
                 NÃO rode 'cargo build' — eu compilo depois. Pedido do usuário: {instruction}"
            )
        };

        say("✍ O agente vai editar os arquivos na cópia…".into());
        let system = format!(
            "{AGENT_SYSTEM}\n\nPASTA DE TRABALHO: {}\n{}",
            update_dir.display(),
            memory_block
        );
        let mut history: Vec<(String, String)> = vec![("user".to_string(), seed)];
        let ok_loop = run_agent_loop(
            &ctx, &tx, &http, &key, &models, &system, &mut history, &update_dir, true, 24,
        );
        if !ok_loop {
            say("Interrompido por erro de API. A pasta 'updateabyss' foi mantida para retomar depois.".into());
            let _ = tx.send(WorkerMsg::AgentDone(history));
            ctx.request_repaint();
            return;
        }

        say("🛠 Compilando a cópia (cargo build)… na 1ª vez pode levar alguns minutos.".into());
        let (built, log) = build_dir(&update_dir);
        out(truncate_str(&log, 6000));

        if built {
            let mut count = 0usize;
            match promote_tree(&update_dir, &project_root, &mut count) {
                Ok(()) => say(format!(
                    "✅ Compilou! Promovi {count} arquivo(s) para o projeto principal. \
                     Feche o Abyss e rode run.bat para compilar/usar a nova versão. (A pasta 'updateabyss' foi mantida.)"
                )),
                Err(e) => err(format!("Compilou, mas falhou ao promover: {e}")),
            }
        } else {
            err(
                "❌ A cópia NÃO compilou — não promovi nada e MANTIVE a pasta 'updateabyss'. \
                 Clique de novo em 🔄 Auto-update (ou peça 'corrija os erros') que eu continuo iterando nela até compilar."
                    .into(),
            );
        }

        let _ = tx.send(WorkerMsg::AgentDone(history));
        ctx.request_repaint();
    });
}

fn spawn_folder_picker(ctx: egui::Context, tx: mpsc::Sender<WorkerMsg>, start: String) {
    thread::spawn(move || {
        let script = format!(
            "Add-Type -AssemblyName System.Windows.Forms; \
             $f = New-Object System.Windows.Forms.FolderBrowserDialog; \
             try {{ $f.SelectedPath = '{}' }} catch {{}}; \
             if ($f.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {{ [Console]::Out.Write($f.SelectedPath) }}",
            start.replace('\'', "''")
        );
        if let Ok(o) = std::process::Command::new("powershell")
            .args(["-NoProfile", "-STA", "-Command", &script])
            .output()
        {
            let p = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if !p.is_empty() {
                let _ = tx.send(WorkerMsg::WorkDir(p));
                ctx.request_repaint();
            }
        }
    });
}

fn load_icon() -> egui::IconData {
    let png = include_bytes!("../assets/abyss.png");
    match image::load_from_memory(png) {
        Ok(img) => {
            let rgba = img.to_rgba8();
            let (w, h) = rgba.dimensions();
            egui::IconData { rgba: rgba.into_raw(), width: w, height: h }
        }
        Err(_) => egui::IconData { rgba: vec![0, 0, 0, 0], width: 1, height: 1 },
    }
}

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 640.0])
            .with_min_inner_size([520.0, 400.0])
            .with_title("Abyss")
            .with_icon(Arc::new(load_icon())),
        ..Default::default()
    };
    eframe::run_native(
        "Abyss",
        native_options,
        Box::new(|cc| Ok(Box::new(App::new(cc)) as Box<dyn eframe::App>)),
    )
}

#[cfg(test)]
mod tests {
    use super::{detect_memory_command, ordered_models, parse_segments, resolve_path, truncate_str, Segment};
    use std::path::Path;

    #[test]
    fn parse_segments_extrai_bloco_sql() {
        let s = "Veja a query:\n```sql\nSELECT * FROM t;\n```\npronto";
        let segs = parse_segments(s);
        assert_eq!(segs.len(), 3);
        assert!(matches!(&segs[0], Segment::Text(t) if t.contains("Veja")));
        match &segs[1] {
            Segment::Code { lang, body } => {
                assert_eq!(lang, "sql");
                assert!(body.contains("SELECT * FROM t;"));
            }
            _ => panic!("esperava bloco de código"),
        }
        assert!(matches!(&segs[2], Segment::Text(t) if t.contains("pronto")));
    }

    #[test]
    fn parse_segments_texto_puro() {
        let segs = parse_segments("apenas texto, sem código");
        assert_eq!(segs.len(), 1);
        assert!(matches!(&segs[0], Segment::Text(_)));
    }

    fn d(s: &str) -> Option<String> {
        detect_memory_command(s)
    }

    #[test]
    fn resolve_relativo_resolve_na_pasta() {
        let base = Path::new("C:/proj");
        assert_eq!(resolve_path(base, "src/main.rs"), Path::new("C:/proj/src/main.rs"));
    }

    #[test]
    fn resolve_absoluto_acessa_pc_inteiro() {
        let base = Path::new("C:/proj");
        // caminho absoluto é usado direto (acesso ao PC inteiro), não confinado à pasta
        let abs = resolve_path(base, r"C:\Windows\notepad.exe");
        assert!(abs.is_absolute());
        assert!(abs.to_string_lossy().to_lowercase().ends_with("notepad.exe"));
        assert!(!abs.starts_with("C:/proj"));
    }

    #[test]
    fn fallback_ordena_selecionado_primeiro() {
        let v = ordered_models("gemini-2.0-flash");
        assert_eq!(v[0], "gemini-2.0-flash");
        assert!(v.contains(&"gemini-2.5-flash".to_string()));
        assert!(v.contains(&"gemini-2.5-pro".to_string()));
        // sem duplicar o selecionado
        assert_eq!(v.iter().filter(|m| *m == "gemini-2.0-flash").count(), 1);
    }

    #[test]
    fn truncate_respeita_limite() {
        assert_eq!(truncate_str("abc", 10), "abc");
        assert!(truncate_str("abcdefghij", 5).starts_with("abcde"));
        // não deve quebrar em caractere multibyte
        let s = "áéíóú".repeat(3);
        let _ = truncate_str(&s, 5); // não pode panicar
    }

    #[test]
    fn gatilho_no_inicio_com_dois_pontos() {
        assert_eq!(
            d("salve isso na memória: responda sempre em português").as_deref(),
            Some("responda sempre em português")
        );
    }

    #[test]
    fn gatilho_no_fim_sem_virgula_sobrando() {
        assert_eq!(
            d("meu nome é Simon, guarde na memória").as_deref(),
            Some("meu nome é Simon")
        );
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(
            d("SALVE ISSO NA MEMÓRIA: beba água").as_deref(),
            Some("beba água")
        );
    }

    #[test]
    fn outra_variacao() {
        assert_eq!(d("memorize isso: gosto de café").as_deref(), Some("gosto de café"));
    }

    #[test]
    fn sem_gatilho_retorna_none() {
        assert_eq!(d("como eu salvo um arquivo no Word?"), None);
        assert_eq!(d("abra a calculadora"), None);
    }

    #[test]
    fn gatilho_sozinho_sem_conteudo_e_none() {
        assert_eq!(d("salve isso na memória"), None);
    }
}
