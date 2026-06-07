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
const MAX_AGENT_STEPS: usize = 8;

const CHAT_SYSTEM: &str = "Você é um assistente útil e direto. \
Responda sempre no idioma do usuário (português quando ele escrever em português). \
Seja claro, objetivo e formate quando ajudar a leitura.";

const AGENT_SYSTEM: &str = r#"Você é um AGENTE DE AUTOMAÇÃO LOCAL rodando na máquina Windows do usuário, com acesso TOTAL ao PowerShell.
O usuário descreve uma tarefa em linguagem natural. Sua função é REALIZÁ-LA, passo a passo, emitindo comandos PowerShell reais.

Para CADA passo responda SOMENTE com um objeto JSON com os campos:
- "explanation": em português, 1-2 frases, o que você fará neste passo (ou o resumo final).
- "powershell":  UM comando/script PowerShell para executar o passo. String vazia "" se nenhum comando for necessário.
- "task_complete": true quando a tarefa inteira estiver concluída e nenhum comando adicional for necessário; senão false.

Depois de cada comando você receberá a saída (stdout/stderr) e poderá decidir o próximo passo com base nela.

Regras:
- Comandos NÃO interativos (nunca peça confirmação; use -Force quando fizer sentido).
- Abrir programas:  Start-Process  (ex.: Start-Process notepad ; Start-Process calc ; Start-Process msedge "https://google.com").
- Criar pastas/arquivos:  New-Item -ItemType Directory -Force ... / New-Item -ItemType File ...
- Área de trabalho:  use  [Environment]::GetFolderPath('Desktop')  para o caminho correto.
- Planilhas Excel via COM (ex.):
    $x = New-Object -ComObject Excel.Application; $x.Visible = $true; $wb = $x.Workbooks.Add(); $ws = $wb.Worksheets.Item(1); $ws.Cells.Item(1,1) = 'Olá'; $wb.SaveAs((Join-Path ([Environment]::GetFolderPath('Desktop')) 'teste.xlsx'))
- Digitar/automatizar teclado:  Add-Type -AssemblyName System.Windows.Forms; [System.Windows.Forms.SendKeys]::SendWait('texto')
- Se for só conversa/saudação (ex.: "olá"), responda na "explanation", deixe "powershell" vazio e "task_complete"=true.
- Faça um passo objetivo por vez. Não invente caminhos; descubra com comandos quando precisar.
- Ao terminar, escreva um resumo curto na "explanation", "powershell"="" e "task_complete"=true."#;

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

    fn send(&mut self, ctx: &egui::Context) {
        let text = self.input.trim().to_string();
        if text.is_empty() || self.pending {
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
        let model = self.model.trim().to_string();
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        self.pending = true;

        match self.mode {
            Mode::Chat => {
                self.chat.transcript.push(Msg::new(Role::User, text.clone()));
                self.chat.history.push(("user".into(), text));
                let history = self.chat.history.clone();
                spawn_chat(ctx2, tx, http, key, model, history);
            }
            Mode::Agent => {
                self.agent.transcript.push(Msg::new(Role::User, text.clone()));
                self.agent.history.push(("user".into(), text));
                let history = self.agent.history.clone();
                let auto = self.auto_run;
                spawn_agent(ctx2, tx, http, key, model, history, auto);
            }
        }
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
                WorkerMsg::AgentErr(e) => {
                    self.agent.transcript.push(Msg::new(Role::Error, e));
                    self.pending = false;
                }
                WorkerMsg::AgentDone(h) => {
                    self.agent.history = h;
                    self.pending = false;
                }
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
                });
            });

            if self.mode == Mode::Agent {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.auto_run, "Executar comandos automaticamente");
                    ui.label(
                        egui::RichText::new("⚠ o agente roda comandos REAIS no seu PC")
                            .small()
                            .color(egui::Color32::from_rgb(220, 160, 60)),
                    );
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
            let (transcript, empty_hint) = match self.mode {
                Mode::Chat => (&self.chat.transcript, "Converse normalmente com o Gemini."),
                Mode::Agent => (
                    &self.agent.transcript,
                    "Dê uma ordem e o agente executa no seu PC. Ex.: \"abra a calculadora\", \"crie uma planilha no desktop com nomes na coluna A\".",
                ),
            };
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    if transcript.is_empty() {
                        ui.add_space(24.0);
                        ui.vertical_centered(|ui| {
                            ui.label(egui::RichText::new(empty_hint).weak());
                        });
                    }
                    for m in transcript {
                        draw_msg(ui, m);
                    }
                });
        });
    }
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
            let txt = egui::RichText::new(m.text.as_str()).color(egui::Color32::from_gray(228));
            ui.label(if mono { txt.monospace() } else { txt });
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

fn run_powershell(script: &str) -> String {
    let wrapped = format!(
        "$OutputEncoding=[Console]::OutputEncoding=[Text.Encoding]::UTF8; {script}"
    );
    let out = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &wrapped,
        ])
        .output();
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
    model: String,
    history: Vec<(String, String)>,
) {
    thread::spawn(move || {
        let body = json!({
            "contents": contents_from(&history),
            "systemInstruction": { "parts": [{ "text": CHAT_SYSTEM }] },
            "generationConfig": { "temperature": 0.7 }
        });
        let msg = match call_gemini(&http, &key, &model, body) {
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

fn spawn_agent(
    ctx: egui::Context,
    tx: mpsc::Sender<WorkerMsg>,
    http: ureq::Agent,
    key: String,
    model: String,
    mut history: Vec<(String, String)>,
    auto_run: bool,
) {
    thread::spawn(move || {
        let schema = json!({
            "type": "object",
            "properties": {
                "explanation": { "type": "string" },
                "powershell": { "type": "string" },
                "task_complete": { "type": "boolean" }
            },
            "required": ["explanation", "powershell", "task_complete"]
        });

        for _ in 0..MAX_AGENT_STEPS {
            let body = json!({
                "contents": contents_from(&history),
                "systemInstruction": { "parts": [{ "text": AGENT_SYSTEM }] },
                "generationConfig": {
                    "temperature": 0.2,
                    "responseMimeType": "application/json",
                    "responseSchema": schema
                }
            });

            let v = match call_gemini(&http, &key, &model, body) {
                Ok(v) => v,
                Err(e) => {
                    let _ = tx.send(WorkerMsg::AgentErr(e));
                    ctx.request_repaint();
                    return;
                }
            };
            let raw = match extract_text(&v) {
                Some(t) => t,
                None => {
                    let _ = tx.send(WorkerMsg::AgentErr(format!("Sem resposta utilizável: {v}")));
                    ctx.request_repaint();
                    return;
                }
            };

            history.push(("model".into(), raw.clone()));

            let parsed: serde_json::Value = serde_json::from_str(&raw)
                .unwrap_or_else(|_| json!({ "explanation": raw, "powershell": "", "task_complete": true }));
            let expl = parsed.get("explanation").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let ps = parsed.get("powershell").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let done = parsed.get("task_complete").and_then(|x| x.as_bool()).unwrap_or(true);

            if !expl.trim().is_empty() {
                let _ = tx.send(WorkerMsg::AgentSay(expl));
                ctx.request_repaint();
            }

            if ps.trim().is_empty() {
                break; // sem comando = resposta final
            }

            let _ = tx.send(WorkerMsg::AgentCmd(ps.clone()));
            ctx.request_repaint();

            if !auto_run {
                let _ = tx.send(WorkerMsg::AgentOut(
                    "⏸ Execução automática DESLIGADA — comando não foi executado.".into(),
                ));
                ctx.request_repaint();
                break;
            }

            let output = run_powershell(&ps);
            let _ = tx.send(WorkerMsg::AgentOut(output.clone()));
            ctx.request_repaint();

            history.push((
                "user".into(),
                format!(
                    "Saída do comando anterior:\n{output}\n\nSe a tarefa foi concluída, defina \
                     task_complete=true e powershell vazio. Caso contrário, forneça o próximo passo."
                ),
            ));

            if done {
                break;
            }
        }

        let _ = tx.send(WorkerMsg::AgentDone(history));
        ctx.request_repaint();
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
