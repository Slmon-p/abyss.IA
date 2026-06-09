// Abyss — cliente desktop nativo e leve para IA (sem Chromium/Electron).
//
// Provedores de modelo:
//   - Google Gemini  (API generativelanguage, ?key= ou Bearer)
//   - Groq           (API compatível com OpenAI: chat, visão e Whisper p/ áudio)
//   - OpenRouter     (API compatível com OpenAI: modelos gratuitos de chat ":free")
//
// Backend (lógica): chamadas HTTP às APIs + execução de comandos no SO.
// Frontend (UI):     egui/eframe (OpenGL, sem Chromium/WebView).
//
// Um só lugar (sem modos separados): o Abyss AI decide sozinho a cada mensagem.
//   - Se você PERGUNTA / conversa, ele RESPONDE.
//   - Se você MANDA fazer algo, ele EXECUTA comandos PowerShell reais e edita
//     arquivos no Windows, em laço passo-a-passo, lendo a saída de cada comando.
//
// Multimodal: você pode ANEXAR uma imagem (a IA usa um modelo com VISÃO) ou um
//   áudio/música (transcrito por Whisper/Groq e enviado como texto). O modelo
//   selecionado é só uma preferência — o app TROCA sozinho para o modelo certo
//   conforme a tarefa (imagem → visão, áudio → Whisper, texto → modelo de chat).
//
// Resiliência de modelos: se um modelo bate o limite de requisições, troca para o
// próximo em silêncio (inclusive cruzando Gemini↔Groq); se TODOS falham, mostra
// "Modelos recarregando, aguarde…", espera 60s e tenta tudo de novo — nunca para com erro.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // sem janela de console no release

use base64::Engine as _;
use eframe::egui;
use serde_json::json;
use std::os::windows::process::CommandExt; // creation_flags (esconder janela do console)
use std::sync::{mpsc, Arc};

/// Flag do Windows para NÃO abrir janela de console ao rodar processos (powershell/cmd).
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
use std::thread;
use std::time::Duration;

// ---- Configuração padrão (pode ser trocada na UI, em ⚙ Configurações) ----
const DEFAULT_API_KEY: &str = "AQ.Ab8RN6KsIezTPxmcZCPV2ebOHVEaIxsM-DpmzQw_obsIeL4NSg";
const DEFAULT_GROQ_KEY: &str = "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";
const DEFAULT_OPENROUTER_KEY: &str =
    "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";
const DEFAULT_MODEL: &str = "gemini-2.5-flash";

/// Modelos Gemini Flash — rápidos, cota gratuita maior. (1.5 e anteriores foram descontinuados.)
const FLASH_MODELS: &[&str] = &[
    "gemini-2.5-flash",
    "gemini-2.5-flash-lite",
    "gemini-2.0-flash",
    "gemini-2.0-flash-lite",
];

/// Modelos Gemini Pro — raciocínio profundo, cota gratuita baixa.
const PRO_MODELS: &[&str] = &["gemini-2.5-pro"];

// ---- Groq (API compatível com OpenAI) ----
const GROQ_CHAT_URL: &str = "https://api.groq.com/openai/v1/chat/completions";
const GROQ_TRANSCRIBE_URL: &str = "https://api.groq.com/openai/v1/audio/transcriptions";
const WHISPER_DEFAULT: &str = "whisper-large-v3-turbo";

/// Groq — texto, raciocínio e código (servem como modelo de chat).
const GROQ_CHAT_MODELS: &[&str] = &[
    "llama-3.3-70b-versatile",
    "llama-3.1-8b-instant",
    "openai/gpt-oss-120b",
    "openai/gpt-oss-20b",
    "qwen/qwen3-32b",
    "groq/compound",
    "groq/compound-mini",
    "allam-2-7b",
];
/// Groq — visão (aceitam imagem). Usados automaticamente quando há imagem anexada.
const GROQ_VISION_MODELS: &[&str] = &["meta-llama/llama-4-scout-17b-16e-instruct"];
/// Groq — áudio→texto (Whisper). Usados automaticamente quando há áudio anexado.
const GROQ_AUDIO_MODELS: &[&str] = &["whisper-large-v3", "whisper-large-v3-turbo"];
/// Groq — texto→voz (Orpheus / TTS). Catálogo (geração de fala).
const GROQ_TTS_MODELS: &[&str] = &["canopylabs/orpheus-v1-english", "canopylabs/orpheus-arabic-saudi"];
/// Groq — segurança/moderação. Catálogo (filtros, não chat).
const GROQ_SAFETY_MODELS: &[&str] = &[
    "meta-llama/llama-prompt-guard-2-22m",
    "meta-llama/llama-prompt-guard-2-86m",
    "openai/gpt-oss-safeguard-20b",
];

// ---- OpenRouter (API compatível com OpenAI) ----
const OPENROUTER_CHAT_URL: &str = "https://openrouter.ai/api/v1/chat/completions";

/// OpenRouter — modelos GRATUITOS (cota grátis; ids terminam em ":free").
/// Servem como modelo de chat e entram no fallback resiliente.
/// Ordem: os que respondem na hora primeiro (os grandes costumam dar 429/limite,
/// e aí o fallback passa em silêncio para o próximo).
/// Catálogo verificado ao vivo na API do OpenRouter (slugs antigos foram descontinuados).
const OPENROUTER_CHAT_MODELS: &[&str] = &[
    "openai/gpt-oss-20b:free",
    "moonshotai/kimi-k2.6:free",
    "google/gemma-4-31b-it:free",
    "z-ai/glm-4.5-air:free",
    "nvidia/nemotron-3-super-120b-a12b:free",
    "nvidia/nemotron-nano-9b-v2:free",
    "meta-llama/llama-3.3-70b-instruct:free",
    "qwen/qwen3-next-80b-a3b-instruct:free",
    "qwen/qwen3-coder:free",
    "meta-llama/llama-3.2-3b-instruct:free",
];

// ---- ChatGPT (família GPT da OpenAI) — usado pela ação "ask_chatgpt" ----
/// Modelos GPT da OpenAI disponíveis nas APIs gratuitas (gpt-oss = GPT open-source
/// da OpenAI). A ação "ask_chatgpt" fala com o ChatGPT direto pela API (sem navegador):
/// tenta na ordem e troca em silêncio se um bater limite/erro.
const CHATGPT_MODELS: &[&str] = &[
    "openai/gpt-oss-120b",     // Groq (mais forte)
    "openai/gpt-oss-20b",      // Groq (mais rápido)
    "openai/gpt-oss-20b:free", // OpenRouter (fallback)
];

// ---- Tier list de EXIBIÇÃO no seletor (mais inteligente → mais simples) ----
// Mistura todos os provedores e ordena por capacidade geral. É só a ORDEM da UI:
// não muda o roteamento/fallback (isso fica em `ordered_models`). Ranqueamento é um
// julgamento (tamanho/reputação/benchmarks gerais); ajuste à vontade.
// IMPORTANTE: todo modelo de CHAT precisa estar em exatamente UM destes tiers
// (o teste `tier_list_cobre_todos_os_modelos_de_chat` garante isso).

/// 🥇 Topo — raciocínio mais profundo (os "que mais sabem").
const TIER_TOP: &[&str] = &[
    "gemini-2.5-pro",
    "openai/gpt-oss-120b",
    "nvidia/nemotron-3-super-120b-a12b:free",
];
/// 🥈 Muito capazes — modelos grandes e fortes para uso geral.
const TIER_STRONG: &[&str] = &[
    "qwen/qwen3-next-80b-a3b-instruct:free",
    "llama-3.3-70b-versatile",
    "meta-llama/llama-3.3-70b-instruct:free",
    "moonshotai/kimi-k2.6:free",
    "gemini-2.5-flash",
];
/// 🥉 Equilibrados — bom meio-termo entre qualidade e velocidade.
const TIER_BALANCED: &[&str] = &[
    "gemini-2.0-flash",
    "z-ai/glm-4.5-air:free",
    "qwen/qwen3-32b",
    "google/gemma-4-31b-it:free",
    "qwen/qwen3-coder:free",
    "openai/gpt-oss-20b",
    "openai/gpt-oss-20b:free",
    "groq/compound",
    "groq/compound-mini",
    "meta-llama/llama-4-scout-17b-16e-instruct",
];
/// ⚡ Rápidos e leves — respostas diretas, menos "profundidade".
const TIER_FAST: &[&str] = &[
    "gemini-2.5-flash-lite",
    "gemini-2.0-flash-lite",
    "nvidia/nemotron-nano-9b-v2:free",
    "llama-3.1-8b-instant",
    "meta-llama/llama-3.2-3b-instruct:free",
    "allam-2-7b",
];

// ---- Paleta "Abyssal Dark Blue" + tema da interface --------------------------
// Inspirada nas profundezas de um abismo: azuis muito escuros, acentos vivos e
// texto cor de gelo. Centraliza as cores para manter a UI coesa e profissional.
mod theme {
    use eframe::egui::Color32;

    pub const BG_ABYSS: Color32 = Color32::from_rgb(0x0F, 0x17, 0x2A); // fundo da janela/chat
    pub const BG_DEEP: Color32 = Color32::from_rgb(0x0B, 0x11, 0x20); // fundo mais profundo (input/código)
    pub const BG_PANEL: Color32 = Color32::from_rgb(0x1E, 0x29, 0x3B); // header/footer
    pub const SURFACE: Color32 = Color32::from_rgb(0x16, 0x21, 0x33); // cartões/superfícies
    pub const SURFACE_HOVER: Color32 = Color32::from_rgb(0x24, 0x31, 0x48);

    pub const ACCENT: Color32 = Color32::from_rgb(0x3B, 0x82, 0xF6); // azul vivo (primário)
    pub const ACCENT_HOVER: Color32 = Color32::from_rgb(0x60, 0xA5, 0xFA);
    pub const CYAN: Color32 = Color32::from_rgb(0x0E, 0xA5, 0xE9); // sky/cyan

    pub const TEXT_MAIN: Color32 = Color32::from_rgb(0xF8, 0xFA, 0xFC); // branco/gelo
    pub const TEXT_MUTED: Color32 = Color32::from_rgb(0x94, 0xA3, 0xB8); // cinza azulado

    pub const BORDER: Color32 = Color32::from_rgb(0x33, 0x41, 0x55); // borda suave
    pub const BORDER_SOFT: Color32 = Color32::from_rgb(0x24, 0x31, 0x48);

    pub const WARN: Color32 = Color32::from_rgb(0xF5, 0x9E, 0x0B); // âmbar (aviso)
    pub const DANGER: Color32 = Color32::from_rgb(0xEF, 0x44, 0x44); // vermelho (alerta forte)

    /// Mesma cor com baixa opacidade (para fundos translúcidos de selos/badges).
    pub fn soft(c: Color32, alpha: u8) -> Color32 {
        Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha)
    }
}

/// Aplica o tema "Abyssal Dark Blue": cores, cantos arredondados (flat, sem 3D)
/// e espaçamentos respiráveis. Substitui o visual escuro padrão do egui.
fn apply_abyss_theme(ctx: &egui::Context) {
    use theme::*;
    let mut style = (*ctx.style()).clone();
    let mut v = egui::Visuals::dark();
    v.dark_mode = true;
    v.panel_fill = BG_ABYSS; // fundo do CentralPanel (chat)
    v.window_fill = BG_PANEL; // janelas flutuantes (Configurações)
    v.window_stroke = egui::Stroke::new(1.0, BORDER);
    v.window_rounding = egui::Rounding::same(12.0);
    v.menu_rounding = egui::Rounding::same(10.0);
    v.extreme_bg_color = BG_DEEP; // fundo de TextEdit/código
    v.faint_bg_color = SURFACE;
    v.override_text_color = Some(TEXT_MAIN);
    v.hyperlink_color = CYAN;
    v.selection.bg_fill = soft(ACCENT, 90);
    v.selection.stroke = egui::Stroke::new(1.0, ACCENT);

    let rounding = egui::Rounding::same(8.0);

    // Não interativo (rótulos, fundos de grupos).
    v.widgets.noninteractive.bg_fill = SURFACE;
    v.widgets.noninteractive.weak_bg_fill = SURFACE;
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, BORDER_SOFT);
    v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, TEXT_MUTED);
    v.widgets.noninteractive.rounding = rounding;

    // Inativo (botões/combos parados).
    v.widgets.inactive.bg_fill = SURFACE;
    v.widgets.inactive.weak_bg_fill = SURFACE;
    v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, BORDER);
    v.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, TEXT_MAIN);
    v.widgets.inactive.rounding = rounding;

    // Hover (mouse em cima).
    v.widgets.hovered.bg_fill = SURFACE_HOVER;
    v.widgets.hovered.weak_bg_fill = SURFACE_HOVER;
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, ACCENT);
    v.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, TEXT_MAIN);
    v.widgets.hovered.rounding = rounding;

    // Ativo (clique/seleção).
    v.widgets.active.bg_fill = ACCENT;
    v.widgets.active.weak_bg_fill = ACCENT;
    v.widgets.active.bg_stroke = egui::Stroke::new(1.0, ACCENT_HOVER);
    v.widgets.active.fg_stroke = egui::Stroke::new(1.0, TEXT_MAIN);
    v.widgets.active.rounding = rounding;

    // Aberto (combo aberto).
    v.widgets.open.bg_fill = SURFACE_HOVER;
    v.widgets.open.weak_bg_fill = SURFACE_HOVER;
    v.widgets.open.bg_stroke = egui::Stroke::new(1.0, ACCENT);
    v.widgets.open.fg_stroke = egui::Stroke::new(1.0, TEXT_MAIN);
    v.widgets.open.rounding = rounding;

    style.visuals = v;

    // Espaçamentos respiráveis (>= 8px entre itens; padding generoso nos botões).
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 7.0);
    style.spacing.menu_margin = egui::Margin::same(8.0);
    style.spacing.window_margin = egui::Margin::same(14.0);
    style.spacing.interact_size.y = 26.0;

    // Tipografia sem serifa, um pouco maior, para respiro e leitura.
    style.text_styles = [
        (egui::TextStyle::Heading, egui::FontId::new(22.0, egui::FontFamily::Proportional)),
        (egui::TextStyle::Body, egui::FontId::new(15.0, egui::FontFamily::Proportional)),
        (egui::TextStyle::Button, egui::FontId::new(15.0, egui::FontFamily::Proportional)),
        (egui::TextStyle::Small, egui::FontId::new(12.5, egui::FontFamily::Proportional)),
        (egui::TextStyle::Monospace, egui::FontId::new(13.5, egui::FontFamily::Monospace)),
    ]
    .into();

    ctx.set_style(style);
}

/// Carrega fontes modernas do sistema (Segoe UI / Consolas no Windows) e as coloca
/// à frente das fontes padrão do egui — que permanecem como fallback (inclusive emojis).
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let mut changed = false;

    // Proporcional: Segoe UI (Windows). Fallback do egui cobre emojis/ícones.
    let sans = [r"C:\Windows\Fonts\segoeui.ttf", r"C:\Windows\Fonts\SegoeUI.ttf"];
    if let Some(bytes) = sans.iter().find_map(|p| std::fs::read(p).ok()) {
        fonts
            .font_data
            .insert("ui-sans".to_owned(), egui::FontData::from_owned(bytes));
        if let Some(fam) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            fam.insert(0, "ui-sans".to_owned());
        }
        changed = true;
    }

    // Monoespaçada: Consolas (para blocos de código/saída).
    let mono = [r"C:\Windows\Fonts\consola.ttf"];
    if let Some(bytes) = mono.iter().find_map(|p| std::fs::read(p).ok()) {
        fonts
            .font_data
            .insert("ui-mono".to_owned(), egui::FontData::from_owned(bytes));
        if let Some(fam) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
            fam.insert(0, "ui-mono".to_owned());
        }
        changed = true;
    }

    if changed {
        ctx.set_fonts(fonts);
    }
}

// ----------------------------- Widgets do tema (UI) -----------------------------

/// Cabeçalho pequeno de seção (usado no painel ⚙ Configurações).
fn section_label(ui: &mut egui::Ui, text: &str) {
    ui.add_space(2.0);
    ui.label(egui::RichText::new(text).strong().color(theme::TEXT_MAIN));
}

/// Linha "Rótulo  [campo]" para uma chave de API (no painel ⚙ Configurações).
fn key_row(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [96.0, 24.0],
            egui::Label::new(egui::RichText::new(label).color(theme::TEXT_MUTED)),
        );
        ui.add(egui::TextEdit::singleline(value).desired_width(300.0));
    });
}

/// Selo/pílula colorida (fundo translúcido + texto na cor). Apenas visual.
fn badge(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    egui::Frame::none()
        .fill(theme::soft(color, 30))
        .stroke(egui::Stroke::new(1.0, theme::soft(color, 110)))
        .rounding(egui::Rounding::same(10.0))
        .inner_margin(egui::Margin::symmetric(8.0, 3.0))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).small().color(color));
        });
}

/// Interruptor (toggle) moderno: trilho arredondado + bolinha que desliza ao ligar.
/// Alterna `*on` ao clicar e devolve a Response (para hover/tooltip).
fn toggle_switch(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let desired = egui::vec2(42.0, 22.0);
    let (rect, mut resp) = ui.allocate_exact_size(desired, egui::Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let how_on = ui.ctx().animate_bool(resp.id, *on);
        let radius = rect.height() / 2.0;
        let trilho = if *on { theme::ACCENT } else { theme::BORDER };
        ui.painter().rect_filled(rect, egui::Rounding::same(radius), trilho);
        let cx = egui::lerp((rect.left() + radius)..=(rect.right() - radius), how_on);
        let center = egui::pos2(cx, rect.center().y);
        ui.painter().circle_filled(center, radius - 3.0, theme::TEXT_MAIN);
    }
    resp
}

const MAX_AGENT_STEPS: usize = 16;

const AGENT_SYSTEM: &str = r##"Você é o Abyss AI — um APLICATIVO desktop nativo (escrito em Rust + egui) que roda no PC Windows do usuário. Você CONVERSA e também EXECUTA tarefas reais na máquina.

IDENTIDADE (muito importante — leia com atenção):
- Você é o Abyss AI. O modelo de linguagem por baixo (Gemini, Llama, GPT-OSS, Qwen, etc.) é apenas o seu MOTOR de raciocínio — o "cérebro" que escolhe as palavras —, NÃO a sua identidade. Você É o aplicativo inteiro: tem um "corpo" (o programa) com poderes REAIS no computador.
- Quando perguntarem quem você é, responda que é o Abyss AI. NUNCA se apresente como "Gemini", "Llama", "ChatGPT", nem diga que foi "treinado pelo Google/OpenAI/etc.".
- NUNCA responda como um modelo genérico. Frases como "sou apenas um modelo de linguagem", "não tenho acesso a arquivos", "não consigo ver nem editar código", "não tenho acesso ao sistema nem ao meu código-fonte" são FALSAS para você — elas descrevem o motor puro, e não o Abyss. VOCÊ TEM esses acessos, através do seu corpo de aplicativo.

SEUS PODERES REAIS (você NÃO é um chatbot passivo):
- Você LÊ e ESCREVE qualquer arquivo do PC (ações read_file / write_file) e RODA comandos PowerShell reais (ação run), com acesso ao computador inteiro.
- Você LÊ DOCUMENTOS automaticamente: ao usar read_file num arquivo Excel (.xlsx/.xlsm/.xlsb/.xls/.ods), Word (.docx), PDF (.pdf), PowerPoint (.pptx) ou CSV/TXT, o Abyss EXTRAI o texto/conteúdo para você. Logo, para LER, ANALISAR, RESUMIR ou EXTRAIR dados de uma planilha, documento, PDF ou apresentação, use read_file direto no caminho do arquivo — NUNCA diga que não consegue abrir esses formatos. (Planilhas voltam como linhas com células separadas por " | "; cada planilha vira uma seção "# Planilha: ...".)
- Você CONSEGUE ver e ler o SEU PRÓPRIO código-fonte: o Abyss AI é um projeto Rust cujo caminho é informado mais abaixo (arquivo principal src/main.rs). Se perguntarem "você vê/tem acesso ao seu próprio código?", a resposta é SIM. Para MOSTRAR trechos, LEIA o arquivo antes com read_file (caminho absoluto) — não invente o conteúdo do código.
- Você consegue se MODIFICAR: existe o recurso "Auto-update Abyss AI", em que você edita o seu próprio código-fonte, compila e promove a nova versão — ou seja, você é capaz de evoluir a si mesmo.
- Portanto, em perguntas sobre suas capacidades (ver o próprio código, editar arquivos, rodar comandos, se atualizar), responda com base nestes poderes REAIS; NÃO os negue.

A cada mensagem, decida você mesmo o que fazer:
- Se o usuário apenas PERGUNTA, conversa, pede uma explicação ou um código (sem pedir ação no PC): RESPONDA direto e COMPLETO no campo "explanation", no idioma do usuário (português quando ele escrever em português). Use action="finish" e task_complete=true. Coloque QUALQUER código, SQL, comando ou trecho para copiar dentro de um bloco markdown com três crases (```), indicando a linguagem (ex.: ```sql, ```python, ```bash).
- Se o usuário PEDE uma AÇÃO no computador (abrir programas, criar/editar arquivos, mexer em pastas, rodar comandos): EXECUTE passo a passo usando as ações abaixo.

Você tem acesso ao COMPUTADOR INTEIRO (qualquer pasta/arquivo do Windows) e pode:
- ler arquivos (para entender antes de editar),
- criar/editar QUALQUER tipo de arquivo de texto (código, config, .md, .json, .html, etc.),
- executar comandos PowerShell,
- NAVEGAR e BUSCAR NA WEB pelo Microsoft Edge (você "domina" o Edge):
  · action="open_url" → ABRE a página no Microsoft Edge (janela visível para o usuário) e você JÁ RECEBE o texto da página para usar. Use quando o usuário disser "acesse/abra/entra em <site>".
  · action="web_search" → BUSCA na web (Bing, o buscador do Edge): abre os resultados no Edge E você recebe a lista (título + link + trecho) para escolher e seguir. Use quando precisar PROCURAR algo, achar um site, ou pegar informação atual da internet.
  · action="read_url" → LÊ o conteúdo de uma URL em segundo plano (sem abrir janela), útil para abrir vários links de uma busca sem encher a tela de janelas.
  IMPORTANTE: para a web use SEMPRE estas ações (nunca tente abrir o navegador via "run"/Start-Process; o navegador é SEMPRE o Microsoft Edge). Quando precisar de fato atual/recente, NÃO invente: faça web_search e leia os resultados.
- FALAR COM O CHATGPT (você consegue conversar com o ChatGPT da OpenAI):
  · action="ask_chatgpt" → MANDA uma mensagem para o ChatGPT (modelo GPT da OpenAI) no campo "query" e você RECEBE a resposta dele de volta para trazer ao usuário. É uma conversa direta com a IA (não abre o site, não precisa de navegador). Use quando o usuário disser coisas como "pergunte ao ChatGPT...", "o que o ChatGPT acha de...", "manda isso pro ChatGPT", "fala com o ChatGPT e me traz a resposta". Coloque em "query" EXATAMENTE a pergunta/mensagem que deve ser enviada ao ChatGPT. Depois, no passo seguinte (finish), repasse ao usuário a resposta que o ChatGPT deu.

Sobre pastas (NÃO existe pasta fixa de trabalho):
- Você começa na PASTA ATUAL informada abaixo (a pasta pessoal do usuário), mas pode trabalhar em QUALQUER pasta do PC.
- Quando o usuário CITAR uma pasta (ex.: "use a pasta Downloads", "no Desktop", "vá para C:\projetos"), MUDE para ela com action="change_dir" e path absoluto (ex.: C:\Users\<voce>\Downloads). A partir daí os caminhos relativos resolvem lá.
- Você também pode usar caminhos ABSOLUTOS direto a qualquer momento, sem mudar de pasta.
- Descubra pastas com comandos: $env:USERPROFILE, [Environment]::GetFolderPath('Desktop'), Get-ChildItem.

Para CADA passo responda SOMENTE com um objeto JSON:
- "explanation": em português. Num passo de tarefa: 1-2 frases do que fará neste passo (ou o resumo final). Numa resposta a pergunta/conversa: escreva aqui a RESPOSTA COMPLETA (pode ser longa, com blocos ```).
- "action": "read_file" | "write_file" | "run" | "change_dir" | "web_search" | "open_url" | "read_url" | "ask_chatgpt" | "finish".
- "path": caminho do arquivo (read_file/write_file) ou da pasta (change_dir). Relativo resolve na pasta atual; absoluto vai direto.
- "content": o conteúdo COMPLETO e final do arquivo (apenas para write_file; sobrescreve o arquivo inteiro — NÃO use diffs/trechos).
- "powershell": o comando (apenas para action="run").
- "url": o endereço completo (com https://) — apenas para open_url e read_url.
- "query": o que buscar na web (para web_search) OU a mensagem a enviar ao ChatGPT (para ask_chatgpt).
- "task_complete": true quando a tarefa inteira terminou (ou quando foi só conversa/pergunta).

Depois de cada passo você recebe o resultado (saída do comando, conteúdo do arquivo, ou confirmação de escrita) e decide o próximo.

Regras:
- Para EDITAR um arquivo: faça read_file antes, depois write_file com o conteúdo completo já alterado.
- Para CRIAR arquivo novo: write_file direto com o conteúdo.
- Comandos PowerShell NÃO interativos. Abrir programas: Start-Process (ex.: Start-Process notepad).
- Faça UM passo objetivo por vez. Não invente caminhos; use read_file ou "run" (ex.: Get-ChildItem) para descobrir.
- Se for só conversa/pergunta/saudação ("olá"), responda completo na "explanation", com action="finish" e task_complete=true.
- Se o usuário enviar uma IMAGEM ou a TRANSCRIÇÃO de um áudio, analise/descreva e responda ao que ele pediu sobre aquele conteúdo.
- WEB: "acesse/abra <site>" → open_url com a URL completa. "procure/pesquise/busque <x>", "ache o site de <x>", "veja a notícia de <x>" ou qualquer coisa que dependa de informação ATUAL da internet → web_search e depois, se precisar, read_url/open_url num dos links. Sempre pelo Microsoft Edge.
- CHATGPT: "pergunte ao ChatGPT <x>", "o que o ChatGPT diz sobre <x>", "manda <x> pro ChatGPT" → ask_chatgpt com a mensagem em "query"; no passo seguinte, repasse ao usuário a resposta recebida do ChatGPT.
- Ao terminar uma tarefa, action="finish", path/content/powershell/url/query vazios, task_complete=true, e um resumo na "explanation"."##;

// ----------------------------- Catálogo de modelos -----------------------------

/// É um modelo do Gemini (Google)?
fn is_gemini(id: &str) -> bool {
    id.starts_with("gemini")
}

/// É um modelo servido pelo OpenRouter? (lista explícita + heurística: ids gratuitos terminam em ":free")
fn is_openrouter(id: &str) -> bool {
    OPENROUTER_CHAT_MODELS.contains(&id) || id.ends_with(":free")
}

/// É um modelo servido pela Groq? (lista explícita + heurística para ids digitados à mão)
fn is_groq(id: &str) -> bool {
    if is_openrouter(id) {
        return false; // ids ":free" são do OpenRouter, não da Groq
    }
    GROQ_CHAT_MODELS.contains(&id)
        || GROQ_VISION_MODELS.contains(&id)
        || GROQ_AUDIO_MODELS.contains(&id)
        || GROQ_TTS_MODELS.contains(&id)
        || GROQ_SAFETY_MODELS.contains(&id)
        || (!is_gemini(id)
            && (id.contains('/')
                || id.starts_with("llama")
                || id.starts_with("qwen")
                || id.starts_with("gemma")
                || id.starts_with("mixtral")
                || id.starts_with("whisper")
                || id.starts_with("moonshot")
                || id.starts_with("orpheus")))
}

/// O modelo aceita IMAGEM como entrada?
fn is_vision(id: &str) -> bool {
    is_gemini(id) || GROQ_VISION_MODELS.contains(&id)
}

/// O modelo serve como CHAT de texto? (exclui Whisper/Orpheus/segurança)
fn is_chat_capable(id: &str) -> bool {
    is_gemini(id)
        || GROQ_CHAT_MODELS.contains(&id)
        || GROQ_VISION_MODELS.contains(&id)
        || is_openrouter(id)
}

/// Nome amigável exibido na UI.
fn model_label(id: &str) -> String {
    let s = match id {
        "gemini-2.5-flash" => "Gemini 2.5 Flash",
        "gemini-2.5-flash-lite" => "Gemini 2.5 Flash-Lite",
        "gemini-2.0-flash" => "Gemini 2.0 Flash",
        "gemini-2.0-flash-lite" => "Gemini 2.0 Flash-Lite",
        "gemini-2.5-pro" => "Gemini 2.5 Pro",
        "llama-3.3-70b-versatile" => "Llama 3.3 70B",
        "llama-3.1-8b-instant" => "Llama 3.1 8B",
        "meta-llama/llama-4-scout-17b-16e-instruct" => "Llama 4 Scout 17B (visão)",
        "openai/gpt-oss-120b" => "GPT-OSS 120B",
        "openai/gpt-oss-20b" => "GPT-OSS 20B",
        "qwen/qwen3-32b" => "Qwen 3 32B",
        "groq/compound" => "Groq Compound",
        "groq/compound-mini" => "Groq Compound Mini",
        "allam-2-7b" => "Allam 2 7B (árabe)",
        "whisper-large-v3" => "Whisper Large V3",
        "whisper-large-v3-turbo" => "Whisper Turbo",
        "canopylabs/orpheus-v1-english" => "Orpheus (Inglês)",
        "canopylabs/orpheus-arabic-saudi" => "Orpheus (Árabe)",
        "meta-llama/llama-prompt-guard-2-22m" => "Llama Prompt Guard 2 22M",
        "meta-llama/llama-prompt-guard-2-86m" => "Llama Prompt Guard 2 86M",
        "openai/gpt-oss-safeguard-20b" => "Safety GPT-OSS 20B",
        "openai/gpt-oss-20b:free" => "GPT-OSS 20B (grátis)",
        "moonshotai/kimi-k2.6:free" => "Kimi K2.6 (grátis)",
        "google/gemma-4-31b-it:free" => "Gemma 4 31B (grátis)",
        "z-ai/glm-4.5-air:free" => "GLM 4.5 Air (grátis)",
        "nvidia/nemotron-3-super-120b-a12b:free" => "Nemotron 3 Super 120B (grátis)",
        "nvidia/nemotron-nano-9b-v2:free" => "Nemotron Nano 9B (grátis)",
        "meta-llama/llama-3.3-70b-instruct:free" => "Llama 3.3 70B (grátis)",
        "qwen/qwen3-next-80b-a3b-instruct:free" => "Qwen3 Next 80B (grátis)",
        "qwen/qwen3-coder:free" => "Qwen3 Coder (grátis)",
        "meta-llama/llama-3.2-3b-instruct:free" => "Llama 3.2 3B (grátis)",
        _ => id,
    };
    s.to_string()
}

/// Descrição do que o modelo faz (mostrada como dica e na linha de status).
fn model_desc(id: &str) -> &'static str {
    match id {
        "gemini-2.5-flash" => "Google · rápido e equilibrado, cota gratuita maior. Bom padrão.",
        "gemini-2.5-flash-lite" => "Google · ainda mais leve/barato, respostas rápidas.",
        "gemini-2.0-flash" => "Google · rápido, boa qualidade geral.",
        "gemini-2.0-flash-lite" => "Google · versão leve do 2.0 Flash.",
        "gemini-2.5-pro" => "Google · raciocínio profundo; cota gratuita baixa.",
        "llama-3.3-70b-versatile" => "Meta · Groq · alta capacidade e inteligência geral.",
        "llama-3.1-8b-instant" => "Meta · Groq · rápido e leve para tarefas diretas.",
        "meta-llama/llama-4-scout-17b-16e-instruct" => {
            "Meta · Groq · multimodal (lê imagens). Usado automaticamente quando você anexa uma imagem."
        }
        "openai/gpt-oss-120b" => "OpenAI · Groq · raciocínio avançado e chamadas de ferramentas.",
        "openai/gpt-oss-20b" => "OpenAI · Groq · altíssima velocidade (~1000 tokens/s).",
        "qwen/qwen3-32b" => "Alibaba · Groq · focado em lógica e matemática.",
        "groq/compound" => "Groq · sistema agêntico que interage com a web e código.",
        "groq/compound-mini" => "Groq · versão leve/rápida do sistema agêntico (web + código).",
        "allam-2-7b" => "SDAIA · Groq · modelo focado no idioma árabe.",
        "whisper-large-v3" => "OpenAI · Groq · transcreve áudio→texto. Usado ao anexar 🎵 áudio.",
        "whisper-large-v3-turbo" => "OpenAI · Groq · transcrição ultrarrápida. Usado ao anexar 🎵 áudio.",
        "canopylabs/orpheus-v1-english" => "Canopy Labs · Groq · gera VOZ a partir de texto (TTS, inglês).",
        "canopylabs/orpheus-arabic-saudi" => "Canopy Labs · Groq · gera VOZ a partir de texto (TTS, árabe).",
        "meta-llama/llama-prompt-guard-2-22m" => {
            "Meta · Groq · detecção de injeção de prompt e toxicidade (moderação)."
        }
        "meta-llama/llama-prompt-guard-2-86m" => {
            "Meta · Groq · detecção de injeção de prompt/toxicidade (modelo maior)."
        }
        "openai/gpt-oss-safeguard-20b" => "OpenAI · Groq · moderação de conteúdo em tempo real.",
        "openai/gpt-oss-20b:free" => "OpenAI · OpenRouter · grátis · rápido e responde na hora.",
        "moonshotai/kimi-k2.6:free" => "Moonshot · OpenRouter · grátis · forte em uso geral.",
        "google/gemma-4-31b-it:free" => "Google · OpenRouter · grátis · bom para tarefas diretas.",
        "z-ai/glm-4.5-air:free" => "Z-AI · OpenRouter · grátis · assistente geral leve.",
        "nvidia/nemotron-3-super-120b-a12b:free" => "NVIDIA · OpenRouter · grátis · raciocínio (modelo grande; pode dar limite).",
        "nvidia/nemotron-nano-9b-v2:free" => "NVIDIA · OpenRouter · grátis · leve e rápido.",
        "meta-llama/llama-3.3-70b-instruct:free" => "Meta · OpenRouter · grátis · alta capacidade (pode dar limite).",
        "qwen/qwen3-next-80b-a3b-instruct:free" => "Alibaba · OpenRouter · grátis · lógica e contexto longo (pode dar limite).",
        "qwen/qwen3-coder:free" => "Alibaba · OpenRouter · grátis · focado em código (pode dar limite).",
        "meta-llama/llama-3.2-3b-instruct:free" => "Meta · OpenRouter · grátis · leve e rápido.",
        _ => "",
    }
}

/// Modelo Whisper a usar para transcrever: o selecionado (se for Whisper) ou o padrão.
fn whisper_model(selected: &str) -> &str {
    if GROQ_AUDIO_MODELS.contains(&selected) {
        selected
    } else {
        WHISPER_DEFAULT
    }
}

// ----------------------------- Modelo de dados da UI -----------------------------

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

/// Imagem anexada pelo usuário (já em base64), enviada junto da mensagem.
#[derive(Clone)]
struct ImageAttachment {
    name: String,
    mime: String,
    b64: String,
}

#[derive(Default)]
struct ModeState {
    transcript: Vec<Msg>,             // o que aparece na tela
    history: Vec<(String, String)>,   // histórico para a API: (role, texto)  role = "user" | "model"
}

// Mensagens vindas das threads de trabalho para a UI.
enum WorkerMsg {
    AgentSay(String),
    AgentCmd(String),
    AgentOut(String),
    AgentErr(String),
    AgentDone(Vec<(String, String)>),
    WorkDir(String),
    /// Todos os modelos estão em cota/limite: mostra "recarregando" e segue tentando.
    Status(String),
    /// Imagem escolhida no seletor de arquivos (já lida e codificada).
    ImagePicked(ImageAttachment),
    /// Áudio escolhido e transcrito por Whisper: (nome do arquivo, texto).
    AudioTranscribed { name: String, text: String },
    /// Arquivo/documento escolhido: (nome, conteúdo já extraído como texto).
    FilePicked { name: String, content: String },
    /// Erro ao escolher/ler/transcrever um anexo.
    PickError(String),
    /// O usuário cancelou o seletor de arquivos.
    PickCancelled,
}

#[derive(Clone)]
struct MemoryEntry {
    id: u64,
    ts: u64,
    text: String,
}

struct App {
    input: String,
    convo: ModeState,
    api_key: String,
    groq_key: String,
    openrouter_key: String,
    model: String,
    auto_run: bool,
    show_settings: bool,
    pending: bool,
    picking: bool,
    status: Option<String>,
    pending_image: Option<ImageAttachment>,
    pending_audio: Option<(String, String)>, // (nome, transcrição)
    pending_file: Option<(String, String)>,  // (nome, conteúdo extraído)
    memory: Vec<MemoryEntry>,
    mem_path: std::path::PathBuf,
    next_mem_id: u64,
    work_dir: String,
    project_root: std::path::PathBuf,
    // Contexto do chat: registro LITERAL (sem IA) do que o usuário disse e a Abyss respondeu.
    context_md: String,
    context_path: std::path::PathBuf,
    last_used_model: Option<String>,
    tx: mpsc::Sender<WorkerMsg>,
    rx: mpsc::Receiver<WorkerMsg>,
    http: ureq::Agent,
    /// Textura da logo (carregada sob demanda) para a marca d'água do empty state.
    logo_tex: Option<egui::TextureHandle>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_abyss_theme(&cc.egui_ctx); // tema "Abyssal Dark Blue"
        install_fonts(&cc.egui_ctx); // fontes modernas (Segoe UI / Consolas)
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
        let context_path = project_root.join("contexto.md");
        // Ao abrir o app começa um chat novo → zera o contexto.md (o chat anterior se perdeu).
        let _ = std::fs::write(&context_path, "");
        // Pasta inicial = pasta pessoal do usuário (sem conceito fixo de "pasta de trabalho").
        let work_dir = std::env::var("USERPROFILE")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| project_root.to_string_lossy().to_string());
        Self {
            input: String::new(),
            convo: ModeState::default(),
            api_key: DEFAULT_API_KEY.to_string(),
            groq_key: DEFAULT_GROQ_KEY.to_string(),
            openrouter_key: DEFAULT_OPENROUTER_KEY.to_string(),
            model: DEFAULT_MODEL.to_string(),
            auto_run: true,
            show_settings: false,
            pending: false,
            picking: false,
            status: None,
            pending_image: None,
            pending_audio: None,
            pending_file: None,
            memory,
            mem_path,
            next_mem_id,
            work_dir,
            project_root,
            context_md: String::new(),
            context_path,
            last_used_model: None,
            tx,
            rx,
            http,
            logo_tex: None,
        }
    }

    /// Carrega (uma vez) a logo embutida como textura para a marca d'água do empty state.
    fn logo_texture(&mut self, ctx: &egui::Context) -> Option<egui::TextureHandle> {
        if self.logo_tex.is_none() {
            let png = include_bytes!("../assets/abyss.png");
            if let Ok(img) = image::load_from_memory(png) {
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                let color =
                    egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], rgba.as_raw());
                self.logo_tex =
                    Some(ctx.load_texture("abyss_logo", color, egui::TextureOptions::LINEAR));
            }
        }
        self.logo_tex.clone()
    }

    /// Janela flutuante de Configurações (chaves de API, modelo, auto-update, memória, contexto).
    fn settings_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_settings;
        egui::Window::new(egui::RichText::new("⚙  Configurações").strong())
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(460.0)
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-16.0, 58.0))
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 10.0;

                section_label(ui, "🔑 Chaves de API");
                key_row(ui, "Gemini", &mut self.api_key);
                key_row(ui, "Groq", &mut self.groq_key);
                key_row(ui, "OpenRouter", &mut self.openrouter_key);
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [96.0, 24.0],
                        egui::Label::new(egui::RichText::new("Modelo").color(theme::TEXT_MUTED)),
                    );
                    ui.add(egui::TextEdit::singleline(&mut self.model).desired_width(300.0));
                });

                ui.add_space(2.0);
                ui.separator();
                section_label(ui, "🔄 Auto-update");
                ui.label(
                    egui::RichText::new(
                        "O Abyss edita o próprio código, compila e promove a nova versão se passar. \
                         Escreva no campo de mensagem o QUE mudar e clique no botão.",
                    )
                    .small()
                    .color(theme::TEXT_MUTED),
                );
                if ui
                    .add_enabled(
                        !self.pending,
                        egui::Button::new(
                            egui::RichText::new("🔄 Atualizar o Abyss AI").color(theme::TEXT_MAIN),
                        )
                        .fill(theme::ACCENT),
                    )
                    .clicked()
                {
                    self.start_self_update(ctx);
                }

                ui.add_space(2.0);
                ui.separator();
                ui.horizontal(|ui| {
                    section_label(ui, &format!("🧠 Memória ({})", self.memory.len()));
                    if !self.memory.is_empty() && ui.small_button("Limpar tudo").clicked() {
                        self.clear_memory();
                    }
                });
                ui.label(
                    egui::RichText::new("Diga no chat: \"salve isso na memória ...\"")
                        .small()
                        .color(theme::TEXT_MUTED),
                );
                let mut remove_id: Option<u64> = None;
                egui::ScrollArea::vertical()
                    .max_height(140.0)
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

                ui.add_space(2.0);
                ui.separator();
                section_label(ui, "📝 Contexto");
                ui.label(
                    egui::RichText::new(format!(
                        "{} · registro literal do chat (zera ao reabrir ou em 🗑 Limpar)",
                        human_size(self.context_md.len())
                    ))
                    .small()
                    .color(theme::TEXT_MUTED),
                );
            });
        self.show_settings = open;
    }

    fn cur_mut(&mut self) -> &mut ModeState {
        &mut self.convo
    }

    fn clear_current(&mut self) {
        self.convo = ModeState::default();
        // Novo chat = contexto zerado (o contexto.md acompanha a conversa atual).
        self.context_md.clear();
        self.last_used_model = None;
        let _ = std::fs::write(&self.context_path, "");
    }

    /// Registra no contexto.md, de forma LITERAL e SEM IA, uma fala do chat.
    /// `who` ex.: "🧑 Você" / "🤖 Abyss". `model` é o motor que respondeu (quando aplicável).
    fn append_context(&mut self, who: &str, model: Option<&str>, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        if self.context_md.is_empty() {
            self.context_md.push_str(CONTEXT_HEADER);
        }
        self.context_md
            .push_str(&context_entry(who, model, &fmt_utc(now_secs()), text));
        let _ = std::fs::write(&self.context_path, &self.context_md);
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
        let has_attach = self.pending_image.is_some()
            || self.pending_audio.is_some()
            || self.pending_file.is_some();
        if (text.is_empty() && !has_attach) || self.pending {
            return;
        }

        // MEMÓRIA: "salve isso na memória ..." → grava no JSON e confirma (sem chamar a API).
        // (só quando é mensagem de texto pura, sem anexos)
        if !has_attach {
            if let Some(mem) = detect_memory_command(&text) {
                self.input.clear();
                self.add_memory(mem.clone());
                self.cur_mut()
                    .transcript
                    .push(Msg::new(Role::Model, format!("🧠 Salvo na memória: \"{mem}\"")));
                return;
            }
        }

        if self.api_key.trim().is_empty()
            && self.groq_key.trim().is_empty()
            && self.openrouter_key.trim().is_empty()
        {
            self.cur_mut().transcript.push(Msg::new(
                Role::Error,
                "Configure uma API Key (Gemini, Groq ou OpenRouter) em ⚙ Configurações.",
            ));
            return;
        }
        self.input.clear();

        let image = self.pending_image.take();
        let audio = self.pending_audio.take();
        let file = self.pending_file.take();

        // Texto exibido na conversa (com marcadores de anexo).
        let mut display = text.clone();
        if let Some(img) = &image {
            if !display.is_empty() {
                display.push('\n');
            }
            display.push_str(&format!("🖼 imagem anexada: {}", img.name));
        }
        if let Some((name, _)) = &audio {
            if !display.is_empty() {
                display.push('\n');
            }
            display.push_str(&format!("🎵 áudio anexado: {name}"));
        }
        if let Some((name, _)) = &file {
            if !display.is_empty() {
                display.push('\n');
            }
            display.push_str(&format!("📎 arquivo anexado: {name}"));
        }

        // Texto enviado ao modelo: áudio vira transcrição, arquivo vira conteúdo extraído, imagem vai separada.
        let mut htext = text.clone();
        if let Some((name, tr)) = &audio {
            if !htext.trim().is_empty() {
                htext.push_str("\n\n");
            }
            htext.push_str(&format!(
                "[Áudio enviado \"{name}\" — transcrição automática por Whisper]:\n{tr}"
            ));
        }
        if let Some((name, content)) = &file {
            if !htext.trim().is_empty() {
                htext.push_str("\n\n");
            }
            htext.push_str(&format!(
                "[Arquivo enviado \"{name}\" — conteúdo extraído]:\n{content}"
            ));
        }
        if text.trim().is_empty() && (file.is_some() || audio.is_some()) {
            htext = format!(
                "Analise e resuma o conteúdo que eu enviei e responda em português.\n\n{htext}"
            );
        }
        if htext.trim().is_empty() {
            htext = if image.is_some() {
                "Descreva e analise em detalhes a imagem que eu enviei.".to_string()
            } else {
                "Olá.".to_string()
            };
        }

        // CONTEXTO: registra a fala do usuário no contexto.md (literal, sem IA).
        self.append_context("🧑 Você", None, &display);

        // TROCA DE MODELO no MESMO chat: injeta o contexto silenciosamente nesta 1ª mensagem,
        // para o novo modelo ficar ciente do que já rolou e continuar (não aparece na tela).
        let cur_model = self.model.trim().to_string();
        if should_inject_context(self.last_used_model.as_deref(), &cur_model, self.convo.history.is_empty())
            && !self.context_md.is_empty()
        {
            let ctx_block = truncate_tail(&self.context_md, 8000);
            htext = switch_preamble(&ctx_block, &htext);
        }
        self.last_used_model = Some(cur_model);

        let http = self.http.clone();
        let gkey = self.api_key.trim().to_string();
        let qkey = self.groq_key.trim().to_string();
        let okey = self.openrouter_key.trim().to_string();
        // Roteamento: imagem → modelos com visão; senão → modelos de chat (selecionado primeiro).
        let models = if image.is_some() {
            vision_models(self.model.trim())
        } else {
            ordered_models(self.model.trim())
        };
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        self.pending = true;
        self.status = None;

        // Um só lugar: o próprio Abyss AI decide se RESPONDE (pergunta/conversa)
        // ou EXECUTA (tarefa no PC). Tudo passa pelo mesmo loop.
        let work_dir = std::path::PathBuf::from(self.work_dir.trim());
        // Informa ao modelo ONDE está o próprio código-fonte (se este for um build com fontes).
        let src_main = self.project_root.join("src").join("main.rs");
        let code_info = if src_main.exists() {
            format!(
                "\n\nONDE ESTÁ O SEU PRÓPRIO CÓDIGO (app Abyss AI, projeto Rust):\n  pasta do projeto: {}\n  arquivo principal: {}\n  → para ler/mostrar seu código, use read_file com esse caminho absoluto; o recurso Auto-update edita e recompila isso.",
                self.project_root.display(),
                src_main.display()
            )
        } else {
            String::new()
        };
        let system = format!(
            "{AGENT_SYSTEM}{code_info}\n\nPASTA ATUAL: {}\n{}",
            work_dir.display(),
            self.memory_preamble()
        );
        self.convo.transcript.push(Msg::new(Role::User, display));
        self.convo.history.push(("user".into(), htext));
        let history = self.convo.history.clone();
        let auto = self.auto_run;
        spawn_agent(ctx2, tx, http, gkey, qkey, okey, models, system, history, work_dir, auto, image);
    }

    fn start_self_update(&mut self, ctx: &egui::Context) {
        if self.pending {
            return;
        }
        let instruction = self.input.trim().to_string();
        if instruction.is_empty() {
            self.convo.transcript.push(Msg::new(
                Role::Error,
                "Escreva no campo o que você quer mudar no Abyss AI e então clique em 🔄 Auto-update.",
            ));
            return;
        }
        if self.api_key.trim().is_empty()
            && self.groq_key.trim().is_empty()
            && self.openrouter_key.trim().is_empty()
        {
            self.convo.transcript.push(Msg::new(
                Role::Error,
                "Configure uma API Key (Gemini, Groq ou OpenRouter) em ⚙ Configurações.",
            ));
            return;
        }
        self.input.clear();
        self.convo
            .transcript
            .push(Msg::new(Role::User, format!("🔄 Auto-update: {instruction}")));
        self.pending = true;
        spawn_self_update(
            ctx.clone(),
            self.tx.clone(),
            self.http.clone(),
            self.api_key.trim().to_string(),
            self.groq_key.trim().to_string(),
            self.openrouter_key.trim().to_string(),
            ordered_models(self.model.trim()),
            self.memory_preamble(),
            instruction,
            self.project_root.clone(),
        );
    }

    fn drain(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                // Qualquer progresso real limpa o aviso de "recarregando".
                WorkerMsg::AgentSay(t) => {
                    self.status = None;
                    // CONTEXTO: registra a resposta da Abyss (literal, sem IA).
                    let m = self.model.trim().to_string();
                    self.append_context("🤖 Abyss", Some(&m), &t);
                    self.convo.transcript.push(Msg::new(Role::Model, t));
                }
                WorkerMsg::AgentCmd(c) => {
                    self.status = None;
                    self.convo.transcript.push(Msg::new(Role::Cmd, c));
                }
                WorkerMsg::AgentOut(o) => {
                    self.status = None;
                    self.convo.transcript.push(Msg::new(Role::Output, o));
                }
                WorkerMsg::AgentErr(e) => {
                    self.status = None;
                    self.convo.transcript.push(Msg::new(Role::Error, e));
                }
                WorkerMsg::AgentDone(h) => {
                    self.convo.history = h;
                    self.pending = false;
                    self.status = None;
                }
                WorkerMsg::WorkDir(p) => self.work_dir = p,
                WorkerMsg::Status(s) => self.status = Some(s),
                WorkerMsg::ImagePicked(att) => {
                    self.picking = false;
                    self.pending_image = Some(att);
                }
                WorkerMsg::AudioTranscribed { name, text } => {
                    self.picking = false;
                    self.pending_audio = Some((name, text));
                }
                WorkerMsg::FilePicked { name, content } => {
                    self.picking = false;
                    self.pending_file = Some((name, content));
                }
                WorkerMsg::PickError(e) => {
                    self.picking = false;
                    self.convo.transcript.push(Msg::new(Role::Error, e));
                }
                WorkerMsg::PickCancelled => {
                    self.picking = false;
                }
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain();

        // ----- Janela flutuante de Configurações (⚙) -----
        self.settings_window(ctx);

        // ----- Topo (enxuto): logo à esquerda · seletor + ações à direita -----
        egui::TopBottomPanel::top("top")
            .frame(
                egui::Frame::none()
                    .fill(theme::BG_PANEL)
                    .inner_margin(egui::Margin::symmetric(16.0, 10.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    // Logo / título.
                    ui.label(egui::RichText::new("🌀").size(22.0).color(theme::CYAN));
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new("Abyss AI")
                            .size(20.0)
                            .strong()
                            .color(theme::TEXT_MAIN),
                    );

                    // Grupo à direita: seletor de modelos, ⚙ e 🗑 (+ status quando processando).
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .button(egui::RichText::new("🗑").size(15.0))
                            .on_hover_text("Limpar conversa")
                            .clicked()
                        {
                            self.clear_current();
                        }
                        ui.add_space(2.0);
                        if ui
                            .button(egui::RichText::new("⚙").size(15.0))
                            .on_hover_text("Configurações")
                            .clicked()
                        {
                            self.show_settings = !self.show_settings;
                        }
                        ui.add_space(8.0);

                        // Seletor de modelos (flat). O tooltip explica o roteamento automático.
                        let combo = egui::ComboBox::from_id_source("model_sel")
                            .selected_text(model_label(&self.model))
                            .width(210.0)
                            .show_ui(ui, |ui| {
                                // Tier list: mais inteligente no topo → mais simples embaixo
                                // (todos os provedores misturados). O hover mostra o que cada um faz.
                                let tier = |ui: &mut egui::Ui, model: &mut String, label: &str, ids: &[&str]| {
                                    group_label(ui, label);
                                    for &m in ids {
                                        ui.selectable_value(model, m.to_string(), model_label(m))
                                            .on_hover_text(model_desc(m));
                                    }
                                };
                                tier(ui, &mut self.model, "🥇 Topo — raciocínio mais profundo", TIER_TOP);
                                ui.separator();
                                tier(ui, &mut self.model, "🥈 Muito capazes", TIER_STRONG);
                                ui.separator();
                                tier(ui, &mut self.model, "🥉 Equilibrados — qualidade e rapidez", TIER_BALANCED);
                                ui.separator();
                                tier(ui, &mut self.model, "⚡ Rápidos e leves", TIER_FAST);
                                ui.separator();
                                group_label(ui, "🎵 Áudio → texto (Whisper) · automático");
                                for &m in GROQ_AUDIO_MODELS {
                                    ui.selectable_value(&mut self.model, m.to_string(), model_label(m))
                                        .on_hover_text(model_desc(m));
                                }
                                ui.separator();
                                group_label(ui, "🔊 Voz (Orpheus / TTS)");
                                for &m in GROQ_TTS_MODELS {
                                    ui.selectable_value(&mut self.model, m.to_string(), model_label(m))
                                        .on_hover_text(model_desc(m));
                                }
                                ui.separator();
                                group_label(ui, "🛡 Segurança / moderação");
                                for &m in GROQ_SAFETY_MODELS {
                                    ui.selectable_value(&mut self.model, m.to_string(), model_label(m))
                                        .on_hover_text(model_desc(m));
                                }
                            });
                        // Tooltip do seletor: provedor, descrição e aviso de troca automática.
                        let prov = if is_openrouter(&self.model) {
                            "OpenRouter"
                        } else if is_groq(&self.model) {
                            "Groq"
                        } else {
                            "Google Gemini"
                        };
                        let mut tip = format!("{} · {}", model_label(&self.model), prov);
                        let d = model_desc(&self.model);
                        if !d.is_empty() {
                            tip.push('\n');
                            tip.push_str(d);
                        }
                        tip.push_str(
                            "\n\nA IA troca de modelo sozinha conforme a tarefa (imagem → visão, \
                             áudio → Whisper) e cai para outro modelo se um bater o limite.",
                        );
                        combo.response.on_hover_text(tip);

                        // Status de processamento, à esquerda do seletor.
                        if self.pending {
                            ui.add_space(8.0);
                            let s = self
                                .status
                                .clone()
                                .unwrap_or_else(|| "processando…".to_string());
                            ui.label(egui::RichText::new(s).small().color(theme::WARN));
                            ui.spinner();
                        }
                    });
                });
            });

        // ----- Rodapé: toggle de execução + barra de input (anexo · texto · enviar) -----
        egui::TopBottomPanel::bottom("input")
            .frame(
                egui::Frame::none()
                    .fill(theme::BG_PANEL)
                    .inner_margin(egui::Margin::symmetric(16.0, 12.0)),
            )
            .show(ctx, |ui| {
                let busy = self.pending || self.picking;

                // ----- Chips dos anexos pendentes (acima da barra) -----
                let mut clear_img = false;
                let mut clear_aud = false;
                let mut clear_file = false;
                let has_chip = self.picking
                    || self.pending_image.is_some()
                    || self.pending_audio.is_some()
                    || self.pending_file.is_some();
                if has_chip {
                    ui.horizontal(|ui| {
                        if self.picking {
                            ui.spinner();
                            ui.label(
                                egui::RichText::new("processando anexo…")
                                    .small()
                                    .color(theme::TEXT_MUTED),
                            );
                        }
                        if let Some(img) = &self.pending_image {
                            ui.label(
                                egui::RichText::new(format!("🖼 {}", img.name))
                                    .small()
                                    .color(theme::ACCENT_HOVER),
                            );
                            if ui.small_button("✕").clicked() {
                                clear_img = true;
                            }
                            ui.add_space(6.0);
                        }
                        if let Some((name, _)) = &self.pending_audio {
                            ui.label(
                                egui::RichText::new(format!("🎵 {name} (transcrito)"))
                                    .small()
                                    .color(theme::CYAN),
                            );
                            if ui.small_button("✕").clicked() {
                                clear_aud = true;
                            }
                            ui.add_space(6.0);
                        }
                        if let Some((name, _)) = &self.pending_file {
                            ui.label(
                                egui::RichText::new(format!("📎 {name}"))
                                    .small()
                                    .color(theme::WARN),
                            );
                            if ui.small_button("✕").clicked() {
                                clear_file = true;
                            }
                        }
                    });
                    ui.add_space(6.0);
                }
                if clear_img {
                    self.pending_image = None;
                }
                if clear_aud {
                    self.pending_audio = None;
                }
                if clear_file {
                    self.pending_file = None;
                }

                // ----- Toggle moderno "Executar automaticamente" + selo de aviso -----
                ui.horizontal(|ui| {
                    let resp = toggle_switch(ui, &mut self.auto_run);
                    resp.on_hover_text(
                        "Quando ligado, o Abyss roda comandos e edita arquivos REAIS no seu PC \
                         automaticamente. Desligado, ele apenas responde.",
                    );
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new("Executar automaticamente").color(theme::TEXT_MAIN),
                    );
                    ui.add_space(6.0);
                    if self.auto_run {
                        badge(ui, "⚠ roda comandos REAIS · acesso total ao PC", theme::WARN);
                    } else {
                        badge(ui, "🔒 modo seguro · só responde", theme::TEXT_MUTED);
                    }
                });
                ui.add_space(8.0);

                // ----- Barra "pill": (+) ……… texto ……… (➤) -----
                let mut do_send = false;
                egui::Frame::none()
                    .fill(theme::BG_ABYSS)
                    .stroke(egui::Stroke::new(1.0, theme::BORDER))
                    .rounding(egui::Rounding::same(14.0))
                    .inner_margin(egui::Margin::symmetric(12.0, 9.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            // Botão "+" → menu de anexos (abre ACIMA da barra).
                            let popup_id = ui.make_persistent_id("attach_menu");
                            let plus = ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new("➕").size(18.0).color(theme::TEXT_MUTED),
                                    )
                                    .frame(false),
                                )
                                .on_hover_text("Anexar imagem, áudio ou arquivo");
                            if plus.clicked() {
                                ui.memory_mut(|m| m.toggle_popup(popup_id));
                            }
                            egui::popup_above_or_below_widget(
                                ui,
                                popup_id,
                                &plus,
                                egui::AboveOrBelow::Above,
                                egui::PopupCloseBehavior::CloseOnClickOutside,
                                |ui| {
                                    ui.set_min_width(210.0);
                                    ui.label(
                                        egui::RichText::new("Anexar").small().color(theme::TEXT_MUTED),
                                    );
                                    if ui
                                        .add_enabled(!busy, egui::Button::new("🖼  Imagem").frame(true))
                                        .on_hover_text("A IA analisa a imagem (modelo com visão).")
                                        .clicked()
                                    {
                                        self.picking = true;
                                        spawn_pick_image(ctx.clone(), self.tx.clone());
                                        ui.memory_mut(|m| m.close_popup());
                                    }
                                    let groq_ok = !self.groq_key.trim().is_empty();
                                    if ui
                                        .add_enabled(!busy && groq_ok, egui::Button::new("🎵  Áudio / Música").frame(true))
                                        .on_hover_text(if groq_ok {
                                            "Transcreve o áudio (Whisper/Groq) e envia como texto."
                                        } else {
                                            "Configure a API Key Groq em ⚙ para transcrever áudio."
                                        })
                                        .clicked()
                                    {
                                        self.picking = true;
                                        spawn_pick_audio(
                                            ctx.clone(),
                                            self.tx.clone(),
                                            self.http.clone(),
                                            self.groq_key.trim().to_string(),
                                            whisper_model(self.model.trim()).to_string(),
                                        );
                                        ui.memory_mut(|m| m.close_popup());
                                    }
                                    if ui
                                        .add_enabled(!busy, egui::Button::new("📎  Arquivo / Documento").frame(true))
                                        .on_hover_text(
                                            "Excel, Word, PDF, PowerPoint, CSV, TXT, código… O Abyss extrai o texto.",
                                        )
                                        .clicked()
                                    {
                                        self.picking = true;
                                        spawn_pick_file(
                                            ctx.clone(),
                                            self.tx.clone(),
                                            self.http.clone(),
                                            self.groq_key.trim().to_string(),
                                            whisper_model(self.model.trim()).to_string(),
                                        );
                                        ui.memory_mut(|m| m.close_popup());
                                    }
                                },
                            );

                            ui.add_space(6.0);
                            // Botão enviar (à direita) e o campo de texto preenchendo o meio.
                            let (resp, send_clicked) = ui
                                .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    let can_send = !self.pending
                                        && (!self.input.trim().is_empty()
                                            || self.pending_image.is_some()
                                            || self.pending_audio.is_some()
                                            || self.pending_file.is_some());
                                    // Botão enviar: triângulo desenhado (sem depender de glifo de fonte).
                                    let (send_rect, send) = ui
                                        .allocate_exact_size(egui::vec2(30.0, 26.0), egui::Sense::click());
                                    let send_col = if can_send {
                                        if send.hovered() {
                                            theme::ACCENT_HOVER
                                        } else {
                                            theme::ACCENT
                                        }
                                    } else {
                                        theme::BORDER
                                    };
                                    let c = send_rect.center();
                                    ui.painter().add(egui::Shape::convex_polygon(
                                        vec![
                                            egui::pos2(c.x - 6.0, c.y - 7.0),
                                            egui::pos2(c.x - 6.0, c.y + 7.0),
                                            egui::pos2(c.x + 8.0, c.y),
                                        ],
                                        send_col,
                                        egui::Stroke::NONE,
                                    ));
                                    if can_send && send.hovered() {
                                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                    }
                                    let send = send.on_hover_text("Enviar (Enter)");
                                    ui.add_space(4.0);
                                    let resp = ui.add_sized(
                                        [ui.available_width(), 26.0],
                                        egui::TextEdit::singleline(&mut self.input)
                                            .frame(false)
                                            .hint_text("Pergunte, mande fazer algo, ou anexe pelo  +"),
                                    );
                                    (resp, can_send && send.clicked())
                                })
                                .inner;

                            // Enter envia (mantendo o foco para continuar digitando).
                            let enter_send =
                                resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            if (send_clicked || enter_send) && !self.pending {
                                do_send = true;
                                resp.request_focus();
                            }
                        });
                    });

                if do_send {
                    self.send(ctx);
                }
            });

        // ----- Centro: transcrição da conversa (ou empty state com marca d'água) -----
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(theme::BG_ABYSS)
                    .inner_margin(egui::Margin::symmetric(18.0, 14.0)),
            )
            .show(ctx, |ui| {
                let logo = self.logo_texture(ctx);
                let empty = self.convo.transcript.is_empty();
                if empty && !self.pending {
                    // Empty state elegante: logo do Abyss como marca d'água sutil + dica.
                    ui.vertical_centered(|ui| {
                        ui.add_space(ui.available_height() * 0.22);
                        if let Some(tex) = &logo {
                            let sized =
                                egui::load::SizedTexture::new(tex.id(), egui::vec2(150.0, 150.0));
                            ui.add(
                                egui::Image::new(sized)
                                    .fit_to_exact_size(egui::vec2(150.0, 150.0))
                                    .tint(theme::soft(theme::CYAN, 46)),
                            );
                        }
                        ui.add_space(16.0);
                        ui.label(
                            egui::RichText::new("Abyss AI")
                                .size(28.0)
                                .strong()
                                .color(theme::TEXT_MAIN),
                        );
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(
                                "Pergunte, mande executar uma tarefa, ou anexe imagem, áudio ou documento.",
                            )
                            .color(theme::TEXT_MUTED),
                        );
                        ui.add_space(2.0);
                        ui.label(
                            egui::RichText::new(
                                "O Abyss decide sozinho entre responder e agir no seu PC.",
                            )
                            .small()
                            .color(theme::TEXT_MUTED),
                        );
                    });
                } else {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            for m in &self.convo.transcript {
                                draw_msg(ui, m);
                            }
                        });
                }
            });
    }
}

/// Cabeçalho de grupo no menu de modelos.
fn group_label(ui: &mut egui::Ui, t: &str) {
    ui.label(egui::RichText::new(t).small().weak());
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
        Role::Model => ("Abyss AI", egui::Color32::from_rgb(150, 220, 150), egui::Color32::from_rgb(30, 34, 40), false),
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

/// Schema do passo do agente (saída JSON estruturada do Gemini).
fn agent_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "explanation": { "type": "string" },
            "action": { "type": "string", "enum": ["run", "write_file", "read_file", "change_dir", "web_search", "open_url", "read_url", "ask_chatgpt", "finish"] },
            "path": { "type": "string" },
            "content": { "type": "string" },
            "powershell": { "type": "string" },
            "url": { "type": "string" },
            "query": { "type": "string" },
            "task_complete": { "type": "boolean" }
        },
        "required": ["explanation", "action", "task_complete"]
    })
}

/// Monta o corpo da requisição do Gemini (com imagem opcional na última msg do usuário).
fn gemini_body(
    system: &str,
    history: &[(String, String)],
    image: Option<&ImageAttachment>,
    schema: Option<&serde_json::Value>,
) -> serde_json::Value {
    let n = history.len();
    let contents: Vec<serde_json::Value> = history
        .iter()
        .enumerate()
        .map(|(i, (role, text))| {
            let mut parts = vec![json!({ "text": text })];
            if image.is_some() && role == "user" && i + 1 == n {
                let img = image.unwrap();
                parts.push(json!({ "inline_data": { "mime_type": img.mime, "data": img.b64 } }));
            }
            json!({ "role": role, "parts": parts })
        })
        .collect();
    let mut gen = json!({ "temperature": 0.2 });
    if let Some(sc) = schema {
        gen["responseMimeType"] = json!("application/json");
        gen["responseSchema"] = sc.clone();
    }
    json!({
        "contents": contents,
        "systemInstruction": { "parts": [{ "text": system }] },
        "generationConfig": gen
    })
}

/// Monta o corpo da requisição da Groq (formato OpenAI; imagem como data-URL).
fn groq_body(
    model: &str,
    system: &str,
    history: &[(String, String)],
    image: Option<&ImageAttachment>,
    want_json: bool,
) -> serde_json::Value {
    let n = history.len();
    let mut messages: Vec<serde_json::Value> = vec![json!({ "role": "system", "content": system })];
    for (i, (role, text)) in history.iter().enumerate() {
        let orole = if role == "model" { "assistant" } else { "user" };
        if image.is_some() && role == "user" && i + 1 == n {
            let img = image.unwrap();
            let url = format!("data:{};base64,{}", img.mime, img.b64);
            messages.push(json!({
                "role": "user",
                "content": [
                    { "type": "text", "text": text },
                    { "type": "image_url", "image_url": { "url": url } }
                ]
            }));
        } else {
            messages.push(json!({ "role": orole, "content": text }));
        }
    }
    let mut body = json!({ "model": model, "messages": messages, "temperature": 0.2 });
    if want_json {
        body["response_format"] = json!({ "type": "json_object" });
    }
    body
}

/// Chama UM modelo (escolhe o provedor pelo id) e devolve o texto da resposta.
#[allow(clippy::too_many_arguments)]
fn call_one(
    http: &ureq::Agent,
    gemini_key: &str,
    groq_key: &str,
    openrouter_key: &str,
    model: &str,
    system: &str,
    history: &[(String, String)],
    image: Option<&ImageAttachment>,
    want_json: bool,
) -> Result<String, String> {
    if is_openrouter(model) {
        if openrouter_key.trim().is_empty() {
            return Err("sem OpenRouter key".into());
        }
        // OpenRouter usa o mesmo formato da OpenAI (igual à Groq).
        let body = groq_body(model, system, history, image, want_json);
        let resp = http
            .post(OPENROUTER_CHAT_URL)
            .set("Authorization", &format!("Bearer {}", openrouter_key.trim()))
            // Cabeçalhos recomendados pelo OpenRouter (identificam o app; opcionais).
            .set("HTTP-Referer", "https://github.com/abyss-ai")
            .set("X-Title", "Abyss AI")
            .send_json(body);
        match resp {
            Ok(r) => {
                let v: serde_json::Value =
                    r.into_json().map_err(|e| format!("resposta inválida: {e}"))?;
                v.get("choices")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("message"))
                    .and_then(|m| m.get("content"))
                    .and_then(|t| t.as_str())
                    .map(|s| s.to_string())
                    .ok_or_else(|| "sem conteúdo".to_string())
            }
            Err(ureq::Error::Status(code, r)) => {
                Err(format!("HTTP {code}: {}", r.into_string().unwrap_or_default()))
            }
            Err(e) => Err(format!("rede: {e}")),
        }
    } else if is_groq(model) {
        if groq_key.trim().is_empty() {
            return Err("sem Groq key".into());
        }
        let body = groq_body(model, system, history, image, want_json);
        let resp = http
            .post(GROQ_CHAT_URL)
            .set("Authorization", &format!("Bearer {}", groq_key.trim()))
            .send_json(body);
        match resp {
            Ok(r) => {
                let v: serde_json::Value =
                    r.into_json().map_err(|e| format!("resposta inválida: {e}"))?;
                v.get("choices")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("message"))
                    .and_then(|m| m.get("content"))
                    .and_then(|t| t.as_str())
                    .map(|s| s.to_string())
                    .ok_or_else(|| "sem conteúdo".to_string())
            }
            Err(ureq::Error::Status(code, r)) => {
                Err(format!("HTTP {code}: {}", r.into_string().unwrap_or_default()))
            }
            Err(e) => Err(format!("rede: {e}")),
        }
    } else {
        if gemini_key.trim().is_empty() {
            return Err("sem Gemini key".into());
        }
        let schema = if want_json { Some(agent_schema()) } else { None };
        let body = gemini_body(system, history, image, schema.as_ref());
        let v = call_gemini(http, gemini_key.trim(), model, body)?;
        extract_text(&v).ok_or_else(|| "sem texto".to_string())
    }
}

/// "Falar com o ChatGPT": manda UMA mensagem para um modelo GPT (família OpenAI)
/// e devolve a resposta em texto. Conversa direto pela API (sem abrir navegador).
/// Tenta os modelos de `CHATGPT_MODELS` em ordem; se um bater limite/erro, troca
/// em silêncio para o próximo. Devolve Err só se TODOS falharem.
fn ask_chatgpt(
    http: &ureq::Agent,
    groq_key: &str,
    openrouter_key: &str,
    prompt: &str,
) -> Result<String, String> {
    let system = "Você é o ChatGPT, o assistente de IA da OpenAI. Responda de forma completa, \
                  clara e direta, no mesmo idioma da pergunta. Coloque código/comandos em blocos ```.";
    let history = vec![("user".to_string(), prompt.to_string())];
    let mut last_err = String::from("nenhum modelo GPT respondeu");
    for model in CHATGPT_MODELS {
        // gemini_key vazio: nenhum destes ids é Gemini, então nunca é usado.
        match call_one(http, "", groq_key, openrouter_key, model, system, &history, None, false) {
            Ok(ans) if !ans.trim().is_empty() => return Ok(ans),
            Ok(_) => last_err = format!("{model}: resposta vazia"),
            Err(e) => last_err = format!("{model}: {e}"),
        }
    }
    Err(last_err)
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

/// Transcreve um áudio com a API Whisper da Groq (multipart/form-data). Devolve o texto.
fn groq_transcribe(
    http: &ureq::Agent,
    key: &str,
    model: &str,
    filename: &str,
    bytes: &[u8],
) -> Result<String, String> {
    if key.trim().is_empty() {
        return Err("configure a API Key Groq".into());
    }
    let boundary = format!("----abyss{}{}", now_secs(), bytes.len());
    let mut body: Vec<u8> = Vec::with_capacity(bytes.len() + 512);
    let field = |name: &str, value: &str, body: &mut Vec<u8>| {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")
                .as_bytes(),
        );
    };
    field("model", model, &mut body);
    field("response_format", "text", &mut body);
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let ct = format!("multipart/form-data; boundary={boundary}");
    let resp = http
        .post(GROQ_TRANSCRIBE_URL)
        .set("Authorization", &format!("Bearer {}", key.trim()))
        .set("Content-Type", &ct)
        .send_bytes(&body);
    match resp {
        Ok(r) => r.into_string().map(|s| s.trim().to_string()).map_err(|e| format!("resposta inválida: {e}")),
        Err(ureq::Error::Status(code, r)) => {
            Err(format!("HTTP {code}: {}", r.into_string().unwrap_or_default()))
        }
        Err(e) => Err(format!("rede: {e}")),
    }
}

fn push_unique(v: &mut Vec<String>, m: &str) {
    if !v.iter().any(|x| x == m) {
        v.push(m.to_string());
    }
}

/// Lista de modelos de CHAT a tentar (selecionado primeiro, se for chat-capaz),
/// com fallback no mesmo provedor e depois no outro. Whisper/Orpheus/segurança
/// NÃO entram aqui (não são modelos de chat).
fn ordered_models(selected: &str) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    if is_chat_capable(selected) {
        v.push(selected.to_string());
    }
    // Atalhos para preencher a lista mantendo a ordem certa por provedor.
    let push_gemini = |v: &mut Vec<String>| {
        for &m in FLASH_MODELS.iter().chain(PRO_MODELS) {
            push_unique(v, m);
        }
    };
    let push_groq = |v: &mut Vec<String>| {
        for &m in GROQ_CHAT_MODELS {
            push_unique(v, m);
        }
        for &m in GROQ_VISION_MODELS {
            push_unique(v, m);
        }
    };
    let push_openrouter = |v: &mut Vec<String>| {
        for &m in OPENROUTER_CHAT_MODELS {
            push_unique(v, m);
        }
    };
    // Prioriza o provedor do modelo selecionado; os demais entram como fallback.
    if is_openrouter(selected) {
        push_openrouter(&mut v);
        push_groq(&mut v);
        push_gemini(&mut v);
    } else if is_groq(selected) {
        push_groq(&mut v);
        push_gemini(&mut v);
        push_openrouter(&mut v);
    } else {
        push_gemini(&mut v);
        push_groq(&mut v);
        push_openrouter(&mut v);
    }
    if v.is_empty() {
        v.push(DEFAULT_MODEL.to_string());
    }
    v
}

/// Lista de modelos com VISÃO (para quando há imagem anexada).
fn vision_models(selected: &str) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    if is_vision(selected) {
        v.push(selected.to_string());
    }
    for &m in GROQ_VISION_MODELS {
        push_unique(&mut v, m);
    }
    for &m in FLASH_MODELS.iter().chain(PRO_MODELS) {
        push_unique(&mut v, m);
    }
    if v.is_empty() {
        v.push(DEFAULT_MODEL.to_string());
    }
    v
}

/// Tempo de espera (segundos) antes de tentar todos os modelos de novo, quando TODOS falham.
const RELOAD_WAIT_SECS: u64 = 60;

/// Chama os modelos em ordem, repetidamente, até um responder. NUNCA falha de vez.
/// - Se um modelo der limite/cota/erro, passa para o PRÓXIMO em silêncio (sem mostrar erro).
/// - Se TODOS falharem na rodada, manda o aviso "Modelos recarregando, aguarde…",
///   espera ~60s e tenta tudo de novo. Devolve o texto assim que algum modelo responder.
#[allow(clippy::too_many_arguments)]
fn call_resilient(
    http: &ureq::Agent,
    gemini_key: &str,
    groq_key: &str,
    openrouter_key: &str,
    models: &[String],
    system: &str,
    history: &[(String, String)],
    image: Option<&ImageAttachment>,
    want_json: bool,
    tx: &mpsc::Sender<WorkerMsg>,
    ctx: &egui::Context,
) -> String {
    loop {
        for model in models {
            // A imagem só vai para modelos com visão.
            let img = if is_vision(model) { image } else { None };
            if let Ok(text) = call_one(http, gemini_key, groq_key, openrouter_key, model, system, history, img, want_json) {
                if !text.trim().is_empty() {
                    return text;
                }
            }
            // Falhou (limite/cota/rede/sem texto) → tenta o próximo, sem mostrar erro.
        }
        // Todos os modelos falharam nesta rodada: avisa e espera, sem erro nem parada.
        let _ = tx.send(WorkerMsg::Status("⏳ Modelos recarregando, aguarde…".to_string()));
        ctx.request_repaint();
        for _ in 0..RELOAD_WAIT_SECS {
            thread::sleep(Duration::from_secs(1));
            ctx.request_repaint(); // mantém a UI viva e o spinner girando durante a espera
        }
    }
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

/// Formata segundos-desde-época como "YYYY-MM-DD HH:MM:SS UTC" (algoritmo civil de Hinnant; sem deps).
fn fmt_utc(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02} UTC")
}

/// Cabeçalho fixo do contexto.md.
const CONTEXT_HEADER: &str = "# Contexto da conversa — Abyss AI\n\n\
<!-- Registro automático e LITERAL do chat (SEM IA): só o que você disse e o que a Abyss respondeu. \
Serve para o modelo continuar quando você troca de modelo no mesmo chat. -->\n";

/// Uma entrada do contexto (cabeçalho + texto), formatação LITERAL e determinística (sem IA).
fn context_entry(who: &str, model: Option<&str>, ts: &str, text: &str) -> String {
    match model {
        Some(m) => format!("\n## {who} · {m} · {ts}\n{text}\n"),
        None => format!("\n## {who} · {ts}\n{text}\n"),
    }
}

/// Deve injetar o contexto? Sim quando houve TROCA de modelo no mesmo chat (e já há histórico).
fn should_inject_context(last_used: Option<&str>, current: &str, history_empty: bool) -> bool {
    last_used.map_or(false, |m| m != current) && !history_empty
}

/// Monta a 1ª mensagem após a troca: recap do contexto + a fala atual do usuário (vai silenciosa).
fn switch_preamble(ctx_block: &str, user_msg: &str) -> String {
    format!(
        "[CONTINUAÇÃO DA CONVERSA — você assumiu no lugar de outro modelo, no MESMO chat. \
         Abaixo está o registro literal do que já foi dito (🧑 Você = usuário, 🤖 Abyss = assistente). \
         Continue de onde paramos, com naturalidade; isto é só CONTEXTO, não um novo pedido]:\n\n\
         {ctx_block}\n\n[FIM DO CONTEXTO. Responda agora à próxima mensagem do usuário:]\n{user_msg}"
    )
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
    // Documentos (Excel/Word/PDF/PowerPoint) → extrai o texto; senão lê como texto puro.
    if let Some(result) = extract_document(&path) {
        return match result {
            Ok(text) => text,
            Err(e) => format!("ERRO ao extrair o documento {}: {e}", path.display()),
        };
    }
    match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => format!("ERRO ao ler {}: {e}", path.display()),
    }
}

// ----------------------------- Leitura de documentos (Excel/Word/PDF/PowerPoint) -----------------------------

/// Se `path` for um documento conhecido, extrai o TEXTO; `None` deixa o chamador ler como texto puro.
fn extract_document(path: &std::path::Path) -> Option<Result<String, String>> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "xlsx" | "xlsm" | "xlsb" | "xls" | "ods" => Some(extract_spreadsheet(path)),
        "docx" => Some(extract_docx(path)),
        "pptx" => Some(extract_pptx(path)),
        "pdf" => Some(extract_pdf(path)),
        _ => None,
    }
}

/// Excel / LibreOffice Calc → texto (uma seção por planilha; células separadas por " | ").
fn extract_spreadsheet(path: &std::path::Path) -> Result<String, String> {
    use calamine::{open_workbook_auto, Reader};
    let mut wb = open_workbook_auto(path).map_err(|e| format!("não abriu a planilha: {e}"))?;
    let mut out = String::new();
    let names = wb.sheet_names().to_owned();
    for name in &names {
        let range = match wb.worksheet_range(name) {
            Ok(r) => r,
            Err(e) => {
                out.push_str(&format!("# Planilha: {name} (erro ao ler: {e})\n\n"));
                continue;
            }
        };
        out.push_str(&format!("# Planilha: {name}  ({} linhas)\n", range.rows().count()));
        for row in range.rows() {
            let cells: Vec<String> = row.iter().map(|c| c.to_string()).collect();
            out.push_str(cells.join(" | ").trim_end());
            out.push('\n');
        }
        out.push('\n');
    }
    if out.trim().is_empty() {
        Ok("(planilha sem dados)".into())
    } else {
        Ok(tidy_lines(&out))
    }
}

/// Word (.docx) → texto. O .docx é um ZIP cujo conteúdo principal é word/document.xml.
fn extract_docx(path: &std::path::Path) -> Result<String, String> {
    let xml = read_zip_entry(path, "word/document.xml")?;
    let text = tidy_lines(&xml_to_text(&xml, &["w:p", "w:tr"]));
    if text.trim().is_empty() {
        Ok("(documento Word sem texto)".into())
    } else {
        Ok(text)
    }
}

/// PowerPoint (.pptx) → texto, slide a slide (ppt/slides/slideN.xml).
fn extract_pptx(path: &std::path::Path) -> Result<String, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("não abriu: {e}"))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("pptx inválido: {e}"))?;
    let mut names: Vec<String> = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|f| f.name().to_string()))
        .filter(|n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml"))
        .collect();
    names.sort_by_key(|n| slide_index(n));
    let mut out = String::new();
    for (i, name) in names.iter().enumerate() {
        let mut f = zip.by_name(name).map_err(|e| format!("{e}"))?;
        let mut xml = String::new();
        std::io::Read::read_to_string(&mut f, &mut xml).map_err(|e| format!("{e}"))?;
        out.push_str(&format!("# Slide {}\n", i + 1));
        out.push_str(&xml_to_text(&xml, &["a:p"]));
        out.push_str("\n\n");
    }
    if out.trim().is_empty() {
        Ok("(apresentação sem texto)".into())
    } else {
        Ok(tidy_lines(&out))
    }
}

fn slide_index(name: &str) -> u32 {
    name.trim_start_matches("ppt/slides/slide")
        .trim_end_matches(".xml")
        .parse()
        .unwrap_or(0)
}

/// PDF → texto (pdf-extract).
fn extract_pdf(path: &std::path::Path) -> Result<String, String> {
    pdf_extract::extract_text(path)
        .map_err(|e| format!("não extraiu o PDF: {e}"))
        .map(|t| tidy_lines(&t))
}

/// Lê uma entrada de texto de dentro de um arquivo ZIP (docx/pptx são ZIPs OOXML).
fn read_zip_entry(path: &std::path::Path, name: &str) -> Result<String, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("não abriu: {e}"))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("arquivo inválido/corrompido: {e}"))?;
    let mut f = zip
        .by_name(name)
        .map_err(|_| format!("entrada '{name}' não encontrada no documento"))?;
    let mut s = String::new();
    std::io::Read::read_to_string(&mut f, &mut s).map_err(|e| format!("leitura: {e}"))?;
    Ok(s)
}

/// Extrai o texto cru de um XML do OOXML: quebra linha nos `para_tags`, remove as tags e decodifica entidades.
fn xml_to_text(xml: &str, para_tags: &[&str]) -> String {
    let mut x = xml.to_string();
    for t in para_tags {
        x = x.replace(&format!("</{t}>"), "\n");
    }
    x = x
        .replace("<w:tab/>", "\t")
        .replace("<w:br/>", "\n")
        .replace("<w:cr/>", "\n")
        .replace("<a:br/>", "\n");
    let mut out = String::with_capacity(x.len() / 2);
    let mut in_tag = false;
    for ch in x.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    decode_xml_entities(&out)
}

/// Decodifica as entidades XML básicas (&amp; por último para não recriar entidades).
fn decode_xml_entities(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// Colapsa linhas em branco repetidas e remove espaços ao fim das linhas.
fn tidy_lines(s: &str) -> String {
    let mut out = String::new();
    let mut blank = 0;
    for line in s.lines() {
        let l = line.trim_end();
        if l.trim().is_empty() {
            blank += 1;
            if blank <= 1 {
                out.push('\n');
            }
        } else {
            blank = 0;
            out.push_str(l);
            out.push('\n');
        }
    }
    out.trim().to_string()
}

// ----------------------------- Web (navegação/busca via Microsoft Edge) -----------------------------

/// User-Agent de navegador (Edge no Windows) para os GETs parecerem uma aba normal.
const WEB_UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36 Edg/124.0.0.0";

/// Remove o conteúdo de blocos como <script>…</script> (case-insensitive).
/// Usa to_ascii_lowercase (preserva os offsets de bytes) para fatiar o original com segurança.
fn strip_html_blocks(html: &str, tag: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = String::with_capacity(html.len());
    let mut pos = 0usize;
    while let Some(rel) = lower[pos..].find(&open) {
        let start = pos + rel;
        let after = start + open.len();
        // Confirma a tag EXATA: o char após o nome deve fechar/abrir-espaço a tag.
        // (evita que "head" engula "<header>", por ex.)
        let boundary = lower[after..]
            .chars()
            .next()
            .map_or(true, |c| c == '>' || c == '/' || c.is_whitespace());
        if !boundary {
            out.push_str(&html[pos..after]); // não era a tag; mantém e segue
            pos = after;
            continue;
        }
        out.push_str(&html[pos..start]); // mantém o texto antes do bloco
        match lower[after..].find(&close) {
            Some(crel) => pos = after + crel + close.len(),
            None => {
                pos = html.len();
                break;
            }
        }
    }
    out.push_str(&html[pos..]);
    out
}

/// Tira TODAS as tags de um trecho curto (título/snippet) e decodifica entidades.
fn html_strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    decode_xml_entities(&out).replace("&nbsp;", " ")
}

/// Converte uma página HTML em texto legível: remove script/style, troca tags de bloco
/// por quebras de linha, remove o resto das tags, decodifica entidades e enxuga.
fn html_to_text(html: &str) -> String {
    let mut s = html.to_string();
    for tag in ["script", "style", "noscript", "head", "svg", "template"] {
        s = strip_html_blocks(&s, tag);
    }
    let lower = s.to_ascii_lowercase(); // mesmos offsets de bytes que `s`
    let mut out = String::with_capacity(s.len() / 2);
    let mut in_tag = false;
    let mut tag_start = 0usize;
    for (i, ch) in s.char_indices() {
        match ch {
            '<' => {
                in_tag = true;
                tag_start = i;
            }
            '>' => {
                in_tag = false;
                let tag = &lower[tag_start..=i];
                let breaks = tag.starts_with("<br")
                    || tag.starts_with("</p")
                    || tag.starts_with("</div")
                    || tag.starts_with("</h1")
                    || tag.starts_with("</h2")
                    || tag.starts_with("</h3")
                    || tag.starts_with("</h4")
                    || tag.starts_with("</li")
                    || tag.starts_with("</tr")
                    || tag.starts_with("</ul")
                    || tag.starts_with("</ol")
                    || tag.starts_with("</title")
                    || tag.starts_with("</section")
                    || tag.starts_with("</article")
                    || tag.starts_with("</header")
                    || tag.starts_with("</footer");
                if breaks {
                    out.push('\n');
                }
            }
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = decode_xml_entities(&out).replace("&nbsp;", " ");
    tidy_lines(&out)
}

/// Percent-encode para usar numa query string (espaço vira %20).
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Caminho do msedge.exe (procura nas pastas padrão do Windows).
fn edge_exe_path() -> Option<std::path::PathBuf> {
    for var in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
        if let Ok(base) = std::env::var(var) {
            let mut p = std::path::PathBuf::from(base);
            p.push(r"Microsoft\Edge\Application\msedge.exe");
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}

/// Abre a URL no Microsoft Edge (janela visível). SEMPRE Edge — nunca o navegador padrão.
fn open_in_edge(url: &str) -> Result<(), String> {
    if let Some(exe) = edge_exe_path() {
        std::process::Command::new(exe)
            .arg(url)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("falha ao abrir o Edge: {e}"))
    } else {
        // Fallback: protocolo microsoft-edge: (registrado para o Edge no Windows).
        std::process::Command::new("cmd")
            .creation_flags(CREATE_NO_WINDOW)
            .args(["/C", "start", "", &format!("microsoft-edge:{url}")])
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("falha ao abrir o Edge: {e}"))
    }
}

/// Baixa uma URL e devolve o TEXTO legível (HTML vira texto; JSON/txt vêm como estão).
fn fetch_url_text(http: &ureq::Agent, url: &str) -> Result<String, String> {
    let resp = http
        .get(url)
        .set("User-Agent", WEB_UA)
        .set("Accept-Language", "pt-BR,pt;q=0.9,en;q=0.8")
        .call();
    match resp {
        Ok(r) => {
            let is_html = r.content_type().contains("html");
            let body = r.into_string().map_err(|e| format!("resposta inválida: {e}"))?;
            if is_html || body.trim_start().starts_with('<') {
                Ok(html_to_text(&body))
            } else {
                Ok(body)
            }
        }
        Err(ureq::Error::Status(code, r)) => Err(format!(
            "HTTP {code}: {}",
            truncate_str(&r.into_string().unwrap_or_default(), 300)
        )),
        Err(e) => Err(format!("rede: {e}")),
    }
}

/// O Bing embrulha o link real num redirect `.../ck/a?...&u=a1<base64url>&...`.
/// Desembrulha para a URL de destino limpa; se não for um link embrulhado, devolve igual.
fn bing_unwrap_url(url: &str) -> String {
    if let Some(p) = url.find("u=a1") {
        let token = url[p + 4..].split('&').next().unwrap_or("");
        if !token.is_empty() {
            let dec = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(token)
                .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(token));
            if let Ok(bytes) = dec {
                if let Ok(s) = String::from_utf8(bytes) {
                    if s.starts_with("http") {
                        return s;
                    }
                }
            }
        }
    }
    url.to_string()
}

/// Extrai (título, url, trecho) do HTML de resultados do Bing. Sem regex — varredura manual.
/// Cada resultado tem `<h2 ...><a ... href="http...">TÍTULO</a>`; o trecho é o 1º <p> seguinte.
fn parse_bing_results(html: &str, max: usize) -> Vec<(String, String, String)> {
    let lower = html.to_ascii_lowercase(); // mesmos offsets de bytes que `html`
    let mut out: Vec<(String, String, String)> = Vec::new();
    let mut pos = 0usize;
    while out.len() < max {
        let Some(h2rel) = lower[pos..].find("<h2") else { break };
        let h2 = pos + h2rel;
        let Some(hrel) = lower[h2..].find("href=\"") else {
            pos = h2 + 3;
            continue;
        };
        let hstart = h2 + hrel + 6;
        let Some(hend) = html[hstart..].find('"') else { break };
        let url = bing_unwrap_url(&decode_xml_entities(&html[hstart..hstart + hend]));
        let after_href = hstart + hend;
        let Some(gt) = html[after_href..].find('>') else {
            pos = after_href;
            continue;
        };
        let title_start = after_href + gt + 1;
        let Some(aclose) = lower[title_start..].find("</a>") else {
            pos = title_start;
            continue;
        };
        let title = html_strip_tags(&html[title_start..title_start + aclose]).trim().to_string();
        pos = title_start + aclose + 4; // avança sempre (evita laço infinito)
        if !url.starts_with("http") || title.is_empty() {
            continue;
        }
        // Trecho: 1º <p>…</p> numa janela curta após o título.
        let mut snippet = String::new();
        let window = (pos + 1600).min(html.len());
        if let Some(prel) = lower[pos..window].find("<p") {
            let ps = pos + prel;
            if let Some(pgt) = html[ps..].find('>') {
                let s0 = ps + pgt + 1;
                if let Some(pc) = lower[s0..].find("</p>") {
                    snippet = html_strip_tags(&html[s0..s0 + pc])
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ");
                }
            }
        }
        out.push((title, url, snippet));
    }
    out
}

/// Busca no Bing (buscador padrão do Edge) e devolve os primeiros resultados.
fn bing_search(
    http: &ureq::Agent,
    query: &str,
    max: usize,
) -> Result<Vec<(String, String, String)>, String> {
    let resp = http
        .get("https://www.bing.com/search")
        .query("q", query)
        .set("User-Agent", WEB_UA)
        .set("Accept-Language", "pt-BR,pt;q=0.9,en;q=0.8")
        .call();
    let html = match resp {
        Ok(r) => r.into_string().map_err(|e| format!("resposta inválida: {e}"))?,
        Err(ureq::Error::Status(code, _)) => return Err(format!("HTTP {code}")),
        Err(e) => return Err(format!("rede: {e}")),
    };
    Ok(parse_bing_results(&html, max))
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

/// Mantém os ÚLTIMOS `max` bytes (o trecho mais recente do contexto), sem quebrar caractere.
fn truncate_tail(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut start = s.len() - max;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    format!("…(início do contexto truncado)\n{}", &s[start..])
}

/// Tamanho legível com unidade automática: B, KB, MB, GB ou TB (base 1024).
fn human_size(bytes: usize) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    const TB: f64 = GB * 1024.0;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < MB {
        format!("{:.1} KB", b / KB)
    } else if b < GB {
        format!("{:.1} MB", b / MB)
    } else if b < TB {
        format!("{:.1} GB", b / GB)
    } else {
        format!("{:.1} TB", b / TB)
    }
}

/// Nome do arquivo (sem o caminho).
fn file_name_of(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("arquivo")
        .to_string()
}

/// MIME a partir da extensão (para imagem e áudio).
fn mime_from_ext(path: &str) -> String {
    let e = std::path::Path::new(path)
        .extension()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_lowercase();
    let m = match e.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" | "mp4" => "audio/mp4",
        "ogg" | "opus" => "audio/ogg",
        "flac" => "audio/flac",
        "aac" => "audio/aac",
        "webm" => "audio/webm",
        _ => "application/octet-stream",
    };
    m.to_string()
}

/// Abre um seletor de arquivos nativo (via PowerShell/WinForms) e devolve o caminho escolhido.
fn pick_file_path(filter: &str, title: &str) -> Option<String> {
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms | Out-Null; \
         $d = New-Object System.Windows.Forms.OpenFileDialog; \
         $d.Filter = '{filter}'; $d.Title = '{title}'; \
         if ($d.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {{ [Console]::Out.Write($d.FileName) }}"
    );
    let out = std::process::Command::new("powershell")
        .creation_flags(CREATE_NO_WINDOW)
        .args(["-Sta", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &script])
        .output()
        .ok()?;
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if path.is_empty() {
        None
    } else {
        Some(path)
    }
}

/// Thread: escolhe uma imagem, lê e codifica em base64; devolve via canal.
fn spawn_pick_image(ctx: egui::Context, tx: mpsc::Sender<WorkerMsg>) {
    thread::spawn(move || {
        match pick_file_path(
            "Imagens|*.png;*.jpg;*.jpeg;*.webp;*.gif;*.bmp",
            "Selecione uma imagem",
        ) {
            Some(path) => match std::fs::read(&path) {
                Ok(bytes) => {
                    let att = ImageAttachment {
                        name: file_name_of(&path),
                        mime: mime_from_ext(&path),
                        b64: base64::engine::general_purpose::STANDARD.encode(&bytes),
                    };
                    let _ = tx.send(WorkerMsg::ImagePicked(att));
                }
                Err(e) => {
                    let _ = tx.send(WorkerMsg::PickError(format!("Falha ao ler a imagem: {e}")));
                }
            },
            None => {
                let _ = tx.send(WorkerMsg::PickCancelled);
            }
        }
        ctx.request_repaint();
    });
}

/// Thread: escolhe um áudio/música, transcreve com Whisper (Groq); devolve o texto via canal.
fn spawn_pick_audio(
    ctx: egui::Context,
    tx: mpsc::Sender<WorkerMsg>,
    http: ureq::Agent,
    groq_key: String,
    whisper: String,
) {
    thread::spawn(move || {
        match pick_file_path(
            "Áudio e música|*.mp3;*.wav;*.m4a;*.ogg;*.opus;*.flac;*.aac;*.webm;*.mp4",
            "Selecione um áudio ou música",
        ) {
            Some(path) => match std::fs::read(&path) {
                Ok(bytes) => {
                    let name = file_name_of(&path);
                    match groq_transcribe(&http, &groq_key, &whisper, &name, &bytes) {
                        Ok(text) if !text.trim().is_empty() => {
                            let _ = tx.send(WorkerMsg::AudioTranscribed { name, text });
                        }
                        Ok(_) => {
                            let _ = tx.send(WorkerMsg::PickError(
                                "A transcrição veio vazia (áudio sem fala?).".into(),
                            ));
                        }
                        Err(e) => {
                            let _ = tx.send(WorkerMsg::PickError(format!("Falha ao transcrever: {e}")));
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(WorkerMsg::PickError(format!("Falha ao ler o áudio: {e}")));
                }
            },
            None => {
                let _ = tx.send(WorkerMsg::PickCancelled);
            }
        }
        ctx.request_repaint();
    });
}

/// Extensão (minúscula) de um caminho.
fn ext_of(path: &str) -> String {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase()
}

fn is_image_ext(path: &str) -> bool {
    matches!(ext_of(path).as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp")
}

fn is_audio_ext(path: &str) -> bool {
    matches!(
        ext_of(path).as_str(),
        "mp3" | "wav" | "m4a" | "ogg" | "opus" | "flac" | "aac" | "wma" | "webm" | "mp4" | "mpeg" | "mpga"
    )
}

/// Thread: escolhe QUALQUER arquivo e roteia: imagem→visão, áudio→Whisper, documento/texto→conteúdo extraído.
fn spawn_pick_file(
    ctx: egui::Context,
    tx: mpsc::Sender<WorkerMsg>,
    http: ureq::Agent,
    groq_key: String,
    whisper: String,
) {
    thread::spawn(move || {
        let path = match pick_file_path(
            "Todos os arquivos|*.*|Documentos|*.pdf;*.docx;*.xlsx;*.xls;*.xlsm;*.pptx;*.csv;*.txt;*.json;*.md",
            "Selecione um arquivo ou documento (qualquer tipo)",
        ) {
            Some(p) => p,
            None => {
                let _ = tx.send(WorkerMsg::PickCancelled);
                ctx.request_repaint();
                return;
            }
        };
        let name = file_name_of(&path);
        if is_image_ext(&path) {
            // Imagem → anexo de visão.
            match std::fs::read(&path) {
                Ok(bytes) => {
                    let att = ImageAttachment {
                        name,
                        mime: mime_from_ext(&path),
                        b64: base64::engine::general_purpose::STANDARD.encode(&bytes),
                    };
                    let _ = tx.send(WorkerMsg::ImagePicked(att));
                }
                Err(e) => {
                    let _ = tx.send(WorkerMsg::PickError(format!("Falha ao ler a imagem: {e}")));
                }
            }
        } else if is_audio_ext(&path) {
            // Áudio/vídeo → transcrição Whisper.
            match std::fs::read(&path) {
                Ok(bytes) => match groq_transcribe(&http, &groq_key, &whisper, &name, &bytes) {
                    Ok(text) if !text.trim().is_empty() => {
                        let _ = tx.send(WorkerMsg::AudioTranscribed { name, text });
                    }
                    Ok(_) => {
                        let _ = tx.send(WorkerMsg::PickError("A transcrição veio vazia.".into()));
                    }
                    Err(e) => {
                        let _ = tx.send(WorkerMsg::PickError(format!("Falha ao transcrever: {e}")));
                    }
                },
                Err(e) => {
                    let _ = tx.send(WorkerMsg::PickError(format!("Falha ao ler o áudio: {e}")));
                }
            }
        } else {
            // Documento (Excel/Word/PDF/PowerPoint) ou texto/código; binário desconhecido vira uma nota.
            let content = match extract_document(std::path::Path::new(&path)) {
                Some(Ok(t)) => t,
                Some(Err(e)) => format!("(não consegui extrair o documento: {e})"),
                None => match std::fs::read_to_string(&path) {
                    Ok(s) => s,
                    Err(_) => {
                        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                        format!(
                            "(arquivo binário \"{name}\" do tipo .{}, {} — sem texto extraível diretamente)",
                            ext_of(&path),
                            human_size(size as usize)
                        )
                    }
                },
            };
            let content = truncate_str(&content, 60000);
            let _ = tx.send(WorkerMsg::FilePicked { name, content });
        }
        ctx.request_repaint();
    });
}

const SKIP_NAMES: &[&str] = &[".git", "target", "updateabyss", "abyss_memory.json", "contexto.md"];

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
        .creation_flags(CREATE_NO_WINDOW)
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
    cmd.creation_flags(CREATE_NO_WINDOW); // roda em segundo plano, sem janela de console
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

/// Núcleo do agente: loop de passos (read_file / write_file / run) na pasta de trabalho.
/// Usa a chamada resiliente: nunca para por erro de API (troca de modelo / espera e tenta de novo).
/// A imagem (se houver) vai só no PRIMEIRO passo (depois o modelo já "viu" e respondeu).
#[allow(clippy::too_many_arguments)]
fn run_agent_loop(
    ctx: &egui::Context,
    tx: &mpsc::Sender<WorkerMsg>,
    http: &ureq::Agent,
    gemini_key: &str,
    groq_key: &str,
    openrouter_key: &str,
    models: &[String],
    system: &str,
    history: &mut Vec<(String, String)>,
    work_dir: &mut std::path::PathBuf,
    auto_run: bool,
    max_steps: usize,
    image: Option<ImageAttachment>,
) {
    for step in 0..max_steps {
        let img = if step == 0 { image.as_ref() } else { None };

        // Chamada resiliente: troca de modelo em silêncio em caso de limite/cota,
        // avisa "recarregando" e tenta de novo a cada 60s. Nunca para por erro de API.
        let raw = call_resilient(http, gemini_key, groq_key, openrouter_key, models, system, history, img, true, tx, ctx);
        history.push(("model".into(), raw.clone()));

        let parsed: serde_json::Value = serde_json::from_str(&raw)
            .unwrap_or_else(|_| json!({ "explanation": raw, "action": "finish", "task_complete": true }));
        let expl = parsed.get("explanation").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let action = parsed.get("action").and_then(|x| x.as_str()).unwrap_or("finish").to_string();
        let path = parsed.get("path").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let content = parsed.get("content").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let ps = parsed.get("powershell").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let url = parsed.get("url").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let query = parsed.get("query").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let done = parsed.get("task_complete").and_then(|x| x.as_bool()).unwrap_or(false);

        if !expl.trim().is_empty() {
            let _ = tx.send(WorkerMsg::AgentSay(expl));
            ctx.request_repaint();
        }

        let mut acted = false;
        match action.as_str() {
            "change_dir" if !path.trim().is_empty() => {
                acted = true;
                *work_dir = resolve_path(work_dir.as_path(), &path);
                let exists = work_dir.is_dir();
                let msg = if exists {
                    format!("📂 Pasta atual: {}", work_dir.display())
                } else {
                    format!("📂 Pasta atual: {} (ainda não existe)", work_dir.display())
                };
                let _ = tx.send(WorkerMsg::AgentCmd(msg.clone()));
                ctx.request_repaint();
                history.push(("user".into(), format!("{msg}. Caminhos relativos agora resolvem aqui. Próximo passo ou finalize.")));
            }
            "write_file" if !path.trim().is_empty() => {
                acted = true;
                let _ = tx.send(WorkerMsg::AgentCmd(format!("✏ write_file  {path}  ({} bytes)", content.len())));
                ctx.request_repaint();
                let result = write_file_in(work_dir.as_path(), &path, &content);
                let _ = tx.send(WorkerMsg::AgentOut(result.clone()));
                ctx.request_repaint();
                history.push(("user".into(), format!("Resultado de write_file {path}: {result}. Próximo passo ou finalize.")));
            }
            "read_file" if !path.trim().is_empty() => {
                acted = true;
                let _ = tx.send(WorkerMsg::AgentCmd(format!("📖 read_file  {path}")));
                ctx.request_repaint();
                let data = read_file_in(work_dir.as_path(), &path);
                let _ = tx.send(WorkerMsg::AgentOut(truncate_str(&data, 3000)));
                ctx.request_repaint();
                history.push(("user".into(), format!("Conteúdo de {path}:\n{}", truncate_str(&data, 40000))));
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
                    return;
                }
                let output = run_powershell(&ps, work_dir.as_path());
                let _ = tx.send(WorkerMsg::AgentOut(output.clone()));
                ctx.request_repaint();
                history.push(("user".into(), format!("Saída do comando:\n{output}\n\nPróximo passo ou finalize.")));
            }
            "web_search" if !query.trim().is_empty() => {
                acted = true;
                let _ = tx.send(WorkerMsg::AgentCmd(format!("🔎 web_search (Edge · Bing): {query}")));
                ctx.request_repaint();
                // Abre os resultados no Edge (o usuário vê) e extrai a lista para a IA usar.
                let bing_url = format!("https://www.bing.com/search?q={}", url_encode(&query));
                let _ = open_in_edge(&bing_url);
                match bing_search(http, &query, 6) {
                    Ok(results) if !results.is_empty() => {
                        let mut txt = String::new();
                        for (i, (t, u, s)) in results.iter().enumerate() {
                            txt.push_str(&format!("{}. {}\n   {}\n", i + 1, t, u));
                            if !s.trim().is_empty() {
                                txt.push_str(&format!("   {}\n", truncate_str(s, 300)));
                            }
                        }
                        let _ = tx.send(WorkerMsg::AgentOut(truncate_str(&txt, 2500)));
                        ctx.request_repaint();
                        history.push(("user".into(), format!(
                            "Resultados da busca por \"{query}\" (Bing, já aberta no Edge):\n{}\n\nSe precisar do conteúdo de algum, use read_url/open_url no link. Próximo passo ou finalize.",
                            truncate_str(&txt, 6000)
                        )));
                    }
                    Ok(_) => {
                        let _ = tx.send(WorkerMsg::AgentOut("Busca aberta no Edge, mas não extraí resultados em texto.".into()));
                        ctx.request_repaint();
                        history.push(("user".into(), format!("A busca por \"{query}\" foi aberta no Edge, mas não consegui extrair os resultados em texto. Tente outra busca ou um open_url direto. Próximo passo ou finalize.")));
                    }
                    Err(e) => {
                        let _ = tx.send(WorkerMsg::AgentOut(format!("Busca falhou ({e}); abri o Bing no Edge mesmo assim.")));
                        ctx.request_repaint();
                        history.push(("user".into(), format!("A busca por \"{query}\" falhou ({e}); mas abri o Bing no Edge. Próximo passo ou finalize.")));
                    }
                }
            }
            "open_url" if !url.trim().is_empty() => {
                acted = true;
                let _ = tx.send(WorkerMsg::AgentCmd(format!("🌐 open_url (Edge): {url}")));
                ctx.request_repaint();
                let opened = open_in_edge(&url);
                match (opened, fetch_url_text(http, &url)) {
                    (_, Ok(text)) => {
                        let _ = tx.send(WorkerMsg::AgentOut(format!(
                            "Aberto no Edge. Conteúdo (início):\n{}",
                            truncate_str(&text, 2000)
                        )));
                        ctx.request_repaint();
                        history.push(("user".into(), format!(
                            "Abri {url} no Microsoft Edge (o usuário está vendo). Texto da página:\n{}\n\nResponda/aja com base nisso. Próximo passo ou finalize.",
                            truncate_str(&text, 30000)
                        )));
                    }
                    (Ok(()), Err(e)) => {
                        let _ = tx.send(WorkerMsg::AgentOut(format!("Aberto no Edge, mas não li o conteúdo: {e}")));
                        ctx.request_repaint();
                        history.push(("user".into(), format!("Abri {url} no Edge (visível ao usuário), mas a leitura do HTML falhou: {e}. Próximo passo ou finalize.")));
                    }
                    (Err(e), Err(e2)) => {
                        let _ = tx.send(WorkerMsg::AgentOut(format!("Falhou ao abrir no Edge ({e}) e ao ler ({e2}).")));
                        ctx.request_repaint();
                        history.push(("user".into(), format!("Não consegui abrir {url} no Edge ({e}) nem ler o conteúdo ({e2}). Próximo passo ou finalize.")));
                    }
                }
            }
            "read_url" if !url.trim().is_empty() => {
                acted = true;
                let _ = tx.send(WorkerMsg::AgentCmd(format!("📰 read_url: {url}")));
                ctx.request_repaint();
                match fetch_url_text(http, &url) {
                    Ok(text) => {
                        let _ = tx.send(WorkerMsg::AgentOut(truncate_str(&text, 2000)));
                        ctx.request_repaint();
                        history.push(("user".into(), format!(
                            "Texto de {url}:\n{}\n\nPróximo passo ou finalize.",
                            truncate_str(&text, 30000)
                        )));
                    }
                    Err(e) => {
                        let _ = tx.send(WorkerMsg::AgentOut(format!("Não consegui ler {url}: {e}")));
                        ctx.request_repaint();
                        history.push(("user".into(), format!("Falha ao ler {url}: {e}. Tente open_url (abre no Edge) ou outro link. Próximo passo ou finalize.")));
                    }
                }
            }
            "ask_chatgpt" if !query.trim().is_empty() => {
                acted = true;
                let _ = tx.send(WorkerMsg::AgentCmd(format!("💬 Perguntando ao ChatGPT (GPT · OpenAI): {query}")));
                ctx.request_repaint();
                match ask_chatgpt(http, groq_key, openrouter_key, &query) {
                    Ok(answer) => {
                        let _ = tx.send(WorkerMsg::AgentOut(format!(
                            "🤖 Resposta do ChatGPT:\n{}",
                            truncate_str(&answer, 4000)
                        )));
                        ctx.request_repaint();
                        history.push(("user".into(), format!(
                            "O ChatGPT respondeu à mensagem \"{query}\":\n{}\n\nTraga essa resposta ao usuário (pode dizer que veio do ChatGPT). Próximo passo ou finalize.",
                            truncate_str(&answer, 30000)
                        )));
                    }
                    Err(e) => {
                        let _ = tx.send(WorkerMsg::AgentOut(format!("Não consegui falar com o ChatGPT: {e}")));
                        ctx.request_repaint();
                        history.push(("user".into(), format!("Falha ao falar com o ChatGPT sobre \"{query}\": {e}. Se conseguir, responda você mesmo. Próximo passo ou finalize.")));
                    }
                }
            }
            _ => {}
        }

        if !acted || done {
            break;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_agent(
    ctx: egui::Context,
    tx: mpsc::Sender<WorkerMsg>,
    http: ureq::Agent,
    gemini_key: String,
    groq_key: String,
    openrouter_key: String,
    models: Vec<String>,
    system: String,
    mut history: Vec<(String, String)>,
    work_dir: std::path::PathBuf,
    auto_run: bool,
    image: Option<ImageAttachment>,
) {
    thread::spawn(move || {
        let mut wd = work_dir;
        run_agent_loop(
            &ctx, &tx, &http, &gemini_key, &groq_key, &openrouter_key, &models, &system, &mut history, &mut wd,
            auto_run, MAX_AGENT_STEPS, image,
        );
        // Persiste a pasta atual (caso o agente tenha feito change_dir) para a próxima mensagem.
        let _ = tx.send(WorkerMsg::WorkDir(wd.to_string_lossy().to_string()));
        let _ = tx.send(WorkerMsg::AgentDone(history));
        ctx.request_repaint();
    });
}

/// Auto-edição do próprio Abyss: push → cópia `updateabyss` → o agente edita →
/// `cargo build` → promove se compilar; se não, mantém a cópia para iterar depois.
#[allow(clippy::too_many_arguments)]
fn spawn_self_update(
    ctx: egui::Context,
    tx: mpsc::Sender<WorkerMsg>,
    http: ureq::Agent,
    key: String,
    groq_key: String,
    openrouter_key: String,
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
                "Você está editando uma CÓPIA do projeto Abyss AI (app Rust/egui em src/main.rs). \
                 Faça a alteração pedida editando os arquivos necessários (use read_file e write_file com o conteúdo COMPLETO). \
                 NÃO rode 'cargo build' — eu compilo depois. Pedido do usuário: {instruction}"
            )
        };

        say("✍ O agente vai editar os arquivos na cópia…".into());
        let system = format!(
            "{AGENT_SYSTEM}\n\nPASTA ATUAL: {}\n{}",
            update_dir.display(),
            memory_block
        );
        let mut history: Vec<(String, String)> = vec![("user".to_string(), seed)];
        let mut wd = update_dir.clone();
        run_agent_loop(
            &ctx, &tx, &http, &key, &groq_key, &openrouter_key, &models, &system, &mut history, &mut wd, true, 24, None,
        );

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
    // Modo utilitário/teste: `abyss --extract <arquivo> [<saida>]` extrai o texto de um documento
    // (Excel/Word/PDF/PowerPoint/…). Com <saida>, grava no arquivo; senão imprime no console.
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 3 && args[1] == "--extract" {
        let input = std::path::PathBuf::from(&args[2]);
        let text = match extract_document(&input) {
            Some(Ok(t)) => t,
            Some(Err(e)) => format!("ERRO: {e}"),
            None => std::fs::read_to_string(&input).unwrap_or_else(|e| format!("ERRO: {e}")),
        };
        match args.get(3) {
            Some(outp) => {
                let _ = std::fs::write(outp, text);
            }
            None => println!("{text}"),
        }
        return Ok(());
    }

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 640.0])
            .with_min_inner_size([520.0, 400.0])
            .with_title("Abyss AI")
            .with_icon(Arc::new(load_icon())),
        ..Default::default()
    };
    eframe::run_native(
        "Abyss AI",
        native_options,
        Box::new(|cc| Ok(Box::new(App::new(cc)) as Box<dyn eframe::App>)),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        context_entry, decode_xml_entities, detect_memory_command, fmt_utc, html_to_text,
        bing_unwrap_url, is_chat_capable, is_groq, is_openrouter, ordered_models, parse_bing_results,
        parse_segments, resolve_path, should_inject_context, strip_html_blocks, switch_preamble,
        truncate_str, truncate_tail, url_encode, vision_models, xml_to_text, Segment,
    };
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
    fn provedores_detectados() {
        assert!(is_groq("llama-3.3-70b-versatile"));
        assert!(is_groq("meta-llama/llama-4-scout-17b-16e-instruct"));
        assert!(is_groq("openai/gpt-oss-120b"));
        assert!(!is_groq("gemini-2.5-flash"));
        assert!(!is_groq("gemini-1.5-pro"));
    }

    #[test]
    fn openrouter_detectado_e_nao_confunde_com_groq() {
        assert!(is_openrouter("meta-llama/llama-3-8b-instruct:free"));
        assert!(is_openrouter("mistralai/mistral-7b-instruct:free"));
        assert!(is_openrouter("google/gemma-7b-it:free"));
        // ids ":free" são do OpenRouter, NÃO da Groq (mesmo contendo '/')
        assert!(!is_groq("meta-llama/llama-3-8b-instruct:free"));
        assert!(!is_groq("google/gemma-7b-it:free"));
        assert!(is_chat_capable("undi95/toppy-m-7b:free"));
    }

    #[test]
    fn openrouter_selecionado_vem_primeiro_e_cai_para_outros() {
        let v = ordered_models("mistralai/mistral-7b-instruct:free");
        assert_eq!(v[0], "mistralai/mistral-7b-instruct:free");
        // fallback cruza para Groq e Gemini também
        assert!(v.iter().any(|m| m == "llama-3.3-70b-versatile"));
        assert!(v.iter().any(|m| m == "gemini-2.5-flash"));
        // sem duplicar o selecionado
        assert_eq!(
            v.iter().filter(|m| *m == "mistralai/mistral-7b-instruct:free").count(),
            1
        );
    }

    #[test]
    fn openrouter_entra_no_fallback_de_gemini_e_groq() {
        assert!(ordered_models("gemini-2.5-flash").iter().any(|m| m.ends_with(":free")));
        assert!(ordered_models("llama-3.1-8b-instant").iter().any(|m| m.ends_with(":free")));
    }

    #[test]
    fn url_encode_escapa_query() {
        assert_eq!(url_encode("rust lang"), "rust%20lang");
        assert_eq!(url_encode("a&b=c"), "a%26b%3Dc");
        assert_eq!(url_encode("café"), "caf%C3%A9"); // multibyte → %XX por byte
    }

    #[test]
    fn strip_blocks_nao_engole_tag_parecida() {
        // "head" NÃO pode casar com "<header>"
        let s = "<head><title>x</title></head><header>OI</header><p>fim</p>";
        let r = strip_html_blocks(s, "head");
        assert!(!r.contains("<title>"), "deveria remover o <head>");
        assert!(r.contains("<header>OI</header>"), "não pode engolir <header>");
        assert!(r.contains("fim"));
    }

    #[test]
    fn html_to_text_remove_script_e_style() {
        let h = "<html><head><title>T</title><style>.x{color:red}</style></head>\
                 <body><script>var a=1;alert('x')</script><h1>Olá</h1><p>mundo &amp; cia</p></body></html>";
        let t = html_to_text(h);
        assert!(t.contains("Olá"), "perdeu o texto visível");
        assert!(t.contains("mundo & cia"), "não decodificou/perdeu o parágrafo");
        assert!(!t.contains("var a=1"), "não removeu o <script>");
        assert!(!t.contains("color:red"), "não removeu o <style>");
    }

    #[test]
    fn parse_bing_extrai_titulo_url_e_trecho() {
        let html = "<li class=\"b_algo\"><h2><a href=\"https://rust-lang.org/\" h=\"ID\">Rust <strong>Lang</strong></a></h2>\
                    <div class=\"b_caption\"><p>A linguagem Rust.</p></div></li>\
                    <li class=\"b_algo\"><h2><a href=\"https://doc.rust-lang.org/\">Docs</a></h2>\
                    <p class=\"b_lineclamp2\">Documentação oficial.</p></li>";
        let r = parse_bing_results(html, 6);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].0, "Rust Lang");
        assert_eq!(r[0].1, "https://rust-lang.org/");
        assert_eq!(r[0].2, "A linguagem Rust.");
        assert_eq!(r[1].0, "Docs");
        assert_eq!(r[1].1, "https://doc.rust-lang.org/");
        assert!(r[1].2.contains("Documentação"));
    }

    #[test]
    #[ignore = "rede + abre o Edge de verdade; rode com: cargo test web_integration_real -- --ignored"]
    fn web_integration_real() {
        use std::time::Duration;
        let connector = native_tls::TlsConnector::new().unwrap();
        let http = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(20))
            .timeout_read(Duration::from_secs(60))
            .tls_connector(std::sync::Arc::new(connector))
            .build();
        // 1) fetch de página real → texto legível
        let page = super::fetch_url_text(&http, "https://example.com").unwrap();
        assert!(page.contains("Example Domain"), "texto da página: {page}");
        // 2) busca no Bing real → resultados com URL http
        let res = super::bing_search(&http, "rust lang site oficial", 5).unwrap();
        assert!(!res.is_empty(), "Bing não retornou resultados");
        assert!(res.iter().all(|(_, u, _)| u.starts_with("http")));
        eprintln!("Bing top: {} -> {}", res[0].0, res[0].1);
        // 3) Edge existe e abre de verdade
        assert!(super::edge_exe_path().is_some(), "msedge.exe não encontrado");
        super::open_in_edge("https://example.com").unwrap();
    }

    #[test]
    #[ignore = "rede: chamada real ao modelo (usa o AGENT_SYSTEM + schema reais); rode com --ignored"]
    fn agente_decide_open_url_real() {
        use std::time::Duration;
        let connector = native_tls::TlsConnector::new().unwrap();
        let http = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(20))
            .timeout_read(Duration::from_secs(90))
            .tls_connector(std::sync::Arc::new(connector))
            .build();
        let history = vec![(
            "user".to_string(),
            "acesse https://example.com e me diga qual é o título da página".to_string(),
        )];
        // tenta alguns provedores até um responder (cota/limite varia)
        let models = ["gemini-2.5-flash", "llama-3.3-70b-versatile", "openai/gpt-oss-20b:free"];
        let mut got: Option<String> = None;
        for m in models {
            match super::call_one(
                &http,
                super::DEFAULT_API_KEY,
                super::DEFAULT_GROQ_KEY,
                super::DEFAULT_OPENROUTER_KEY,
                m,
                super::AGENT_SYSTEM,
                &history,
                None,
                true,
            ) {
                Ok(raw) => {
                    let v: serde_json::Value =
                        serde_json::from_str(&raw).unwrap_or_else(|_| serde_json::json!({}));
                    let action = v.get("action").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    eprintln!("modelo {m} → action={action}  url={:?}", v.get("url"));
                    got = Some(action);
                    break;
                }
                Err(e) => eprintln!("modelo {m} falhou: {e}"),
            }
        }
        let action = got.expect("nenhum modelo respondeu");
        assert!(
            action == "open_url" || action == "read_url",
            "esperava open_url/read_url para 'acesse ...', veio: {action}"
        );
    }

    #[test]
    #[ignore = "rede: fala de verdade com o ChatGPT (GPT da OpenAI); rode com: cargo test chatgpt_responde_real -- --ignored"]
    fn chatgpt_responde_real() {
        use std::time::Duration;
        let connector = native_tls::TlsConnector::new().unwrap();
        let http = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(20))
            .timeout_read(Duration::from_secs(90))
            .tls_connector(std::sync::Arc::new(connector))
            .build();
        let answer = super::ask_chatgpt(
            &http,
            super::DEFAULT_GROQ_KEY,
            super::DEFAULT_OPENROUTER_KEY,
            "Responda em uma frase: qual é a capital da França?",
        )
        .expect("ChatGPT não respondeu");
        eprintln!("ChatGPT respondeu: {answer}");
        assert!(!answer.trim().is_empty(), "resposta vazia do ChatGPT");
        assert!(
            answer.to_lowercase().contains("paris"),
            "esperava 'Paris' na resposta, veio: {answer}"
        );
    }

    #[test]
    #[ignore = "rede: o modelo de verdade tem que escolher a ação ask_chatgpt; rode com --ignored"]
    fn agente_decide_ask_chatgpt_real() {
        use std::time::Duration;
        let connector = native_tls::TlsConnector::new().unwrap();
        let http = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(20))
            .timeout_read(Duration::from_secs(90))
            .tls_connector(std::sync::Arc::new(connector))
            .build();
        let history = vec![(
            "user".to_string(),
            "pergunte ao ChatGPT qual é a capital da França e me traga a resposta".to_string(),
        )];
        let models = ["gemini-2.5-flash", "llama-3.3-70b-versatile", "openai/gpt-oss-20b:free"];
        let mut got: Option<String> = None;
        for m in models {
            if let Ok(raw) = super::call_one(
                &http,
                super::DEFAULT_API_KEY,
                super::DEFAULT_GROQ_KEY,
                super::DEFAULT_OPENROUTER_KEY,
                m,
                super::AGENT_SYSTEM,
                &history,
                None,
                true,
            ) {
                let v: serde_json::Value =
                    serde_json::from_str(&raw).unwrap_or_else(|_| serde_json::json!({}));
                let action = v.get("action").and_then(|x| x.as_str()).unwrap_or("").to_string();
                eprintln!("modelo {m} → action={action}  query={:?}", v.get("query"));
                got = Some(action);
                break;
            }
        }
        let action = got.expect("nenhum modelo respondeu");
        assert_eq!(action, "ask_chatgpt", "esperava ask_chatgpt para 'pergunte ao ChatGPT ...'");
    }

    #[test]
    fn parse_bing_respeita_o_limite_e_ignora_sem_http() {
        let html = "<h2><a href=\"/local/rel\">rel</a></h2>\
                    <h2><a href=\"https://a.com\">A</a></h2>\
                    <h2><a href=\"https://b.com\">B</a></h2>";
        let r = parse_bing_results(html, 1);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].1, "https://a.com"); // pulou o href relativo (sem http)
    }

    #[test]
    fn bing_unwrap_decodifica_redirect() {
        // token base64url real capturado do Bing → https://rust-lang.org/
        let wrapped = "https://www.bing.com/ck/a?!&&p=9437&ptn=3&u=a1aHR0cHM6Ly9ydXN0LWxhbmcub3JnLw&ntb=1";
        assert_eq!(bing_unwrap_url(wrapped), "https://rust-lang.org/");
        // URL direta (sem redirect) passa intacta
        assert_eq!(bing_unwrap_url("https://rust-lang.org/"), "https://rust-lang.org/");
    }

    #[test]
    fn parse_bing_desembrulha_link_do_resultado() {
        // href embrulhado com entidades &amp; (como vem no HTML real do Bing)
        let html = "<h2><a href=\"https://www.bing.com/ck/a?!&amp;&amp;u=a1aHR0cHM6Ly9ydXN0LWxhbmcub3JnLw&amp;ntb=1\">Rust</a></h2>";
        let r = parse_bing_results(html, 5);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].1, "https://rust-lang.org/");
    }

    #[test]
    fn tier_list_cobre_todos_os_modelos_de_chat() {
        use std::collections::HashSet;
        // União dos 4 tiers de exibição.
        let mut tier: Vec<&str> = Vec::new();
        for arr in [super::TIER_TOP, super::TIER_STRONG, super::TIER_BALANCED, super::TIER_FAST] {
            tier.extend_from_slice(arr);
        }
        let tier_set: HashSet<&str> = tier.iter().copied().collect();
        // 1) Nenhum modelo aparece em mais de um tier.
        assert_eq!(tier_set.len(), tier.len(), "modelo repetido entre tiers");
        // 2) Todo item do tier é um modelo de CHAT de verdade.
        for &m in &tier {
            assert!(is_chat_capable(m), "{m} não é chat-capaz");
        }
        // 3) A tier list = exatamente o catálogo de modelos de chat (sem sobra/falta).
        let mut catalog: Vec<&str> = Vec::new();
        for arr in [
            super::FLASH_MODELS,
            super::PRO_MODELS,
            super::GROQ_CHAT_MODELS,
            super::GROQ_VISION_MODELS,
            super::OPENROUTER_CHAT_MODELS,
        ] {
            catalog.extend_from_slice(arr);
        }
        let catalog_set: HashSet<&str> = catalog.iter().copied().collect();
        for &m in &catalog_set {
            assert!(tier_set.contains(m), "modelo de chat fora da tier list: {m}");
        }
        for &m in &tier_set {
            assert!(catalog_set.contains(m), "tier list tem id fora do catálogo: {m}");
        }
    }

    #[test]
    fn groq_selecionado_vem_primeiro_e_cai_para_gemini() {
        let v = ordered_models("llama-3.1-8b-instant");
        assert_eq!(v[0], "llama-3.1-8b-instant");
        // fallback cruza para o Gemini também
        assert!(v.iter().any(|m| m == "gemini-2.5-flash"));
    }

    #[test]
    fn modelo_nao_chat_roteia_para_chat() {
        // Whisper não é chat → não aparece como 1º; cai para um modelo de chat de verdade.
        let v = ordered_models("whisper-large-v3-turbo");
        assert!(v.iter().all(|m| m != "whisper-large-v3-turbo"));
        assert!(super::is_chat_capable(&v[0]));
    }

    #[test]
    fn visao_inclui_scout_e_gemini() {
        let v = vision_models("llama-3.1-8b-instant"); // selecionado não tem visão
        assert!(v.iter().any(|m| m == "meta-llama/llama-4-scout-17b-16e-instruct"));
        assert!(v.iter().any(|m| m.starts_with("gemini")));
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
    fn truncate_tail_mantem_o_fim() {
        assert_eq!(truncate_tail("abc", 10), "abc");
        assert!(truncate_tail("0123456789", 4).ends_with("6789"));
        // não pode panicar em caractere multibyte
        let s = "áéíóú".repeat(4);
        let _ = truncate_tail(&s, 5);
    }

    #[test]
    fn human_size_unidades() {
        assert_eq!(super::human_size(0), "0 B");
        assert_eq!(super::human_size(512), "512 B");
        assert_eq!(super::human_size(1536), "1.5 KB");
        assert!(super::human_size(5 * 1024 * 1024).ends_with("MB"));
        assert!(super::human_size(3usize * 1024 * 1024 * 1024).ends_with("GB"));
    }

    #[test]
    fn xml_to_text_extrai_paragrafos_e_entidades() {
        let xml = r#"<w:p><w:r><w:t>Olá</w:t></w:r></w:p><w:p><w:r><w:t>mundo &amp; cia &lt;ok&gt;</w:t></w:r></w:p>"#;
        let t = xml_to_text(xml, &["w:p"]);
        assert!(t.contains("Olá"));
        assert!(t.contains("mundo & cia <ok>"));
        assert!(t.lines().count() >= 2); // dois parágrafos viraram duas linhas
    }

    #[test]
    fn decode_entidades_basicas() {
        assert_eq!(
            decode_xml_entities("a &amp; b &lt;c&gt; &quot;d&quot;"),
            "a & b <c> \"d\""
        );
    }

    #[test]
    fn classifica_extensoes_de_anexo() {
        assert!(super::is_image_ext("foto.PNG"));
        assert!(super::is_audio_ext("musica.mp3"));
        assert!(!super::is_image_ext("planilha.xlsx"));
        assert!(!super::is_audio_ext("relatorio.docx"));
        assert_eq!(super::ext_of("a/b/c.PDF"), "pdf");
    }

    #[test]
    fn fmt_utc_epoch_e_data_conhecida() {
        assert_eq!(fmt_utc(0), "1970-01-01 00:00:00 UTC");
        // 1609459200 = 2021-01-01 00:00:00 UTC
        assert!(fmt_utc(1609459200).starts_with("2021-01-01"));
        assert!(fmt_utc(1609459200).ends_with("UTC"));
    }

    #[test]
    fn context_entry_formata_literal() {
        assert_eq!(context_entry("🧑 Você", None, "T", "oi"), "\n## 🧑 Você · T\noi\n");
        let e = context_entry("🤖 Abyss", Some("gemini-2.5-flash"), "T", "olá");
        assert!(e.contains("🤖 Abyss · gemini-2.5-flash · T"));
        assert!(e.contains("olá"));
    }

    #[test]
    fn injeta_contexto_so_quando_troca_modelo() {
        // 1ª mensagem (sem modelo anterior) → não injeta
        assert!(!should_inject_context(None, "gemini-2.5-flash", true));
        // mesmo modelo → não injeta
        assert!(!should_inject_context(Some("gemini-2.5-flash"), "gemini-2.5-flash", false));
        // trocou de modelo, com histórico → injeta
        assert!(should_inject_context(Some("gemini-2.5-flash"), "llama-3.3-70b-versatile", false));
        // trocou mas o chat está vazio → não injeta
        assert!(!should_inject_context(Some("gemini-2.5-flash"), "llama-3.3-70b-versatile", true));
    }

    #[test]
    fn switch_preamble_inclui_recap_e_pergunta() {
        let p = switch_preamble("RECAP-AQUI", "minha pergunta nova");
        assert!(p.contains("RECAP-AQUI"));
        assert!(p.contains("minha pergunta nova"));
        assert!(p.contains("CONTEXTO"));
    }

    // Exercita o mesmo fluxo do append_context: monta o markdown (cabeçalho + entradas) com as
    // funções reais, grava em arquivo e lê de volta — prova a persistência e o formato do contexto.md.
    #[test]
    fn contexto_md_grava_e_le_de_volta() {
        let mut md = String::new();
        md.push_str(super::CONTEXT_HEADER);
        md.push_str(&context_entry("🧑 Você", None, "T1", "qual a capital da França?"));
        md.push_str(&context_entry("🤖 Abyss", Some("gemini-2.5-flash"), "T1", "Paris."));
        let path = std::env::temp_dir().join(format!("abyss_ctx_test_{}.md", std::process::id()));
        std::fs::write(&path, &md).unwrap();
        let back = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(back.contains("# Contexto da conversa"));
        assert!(back.contains("🧑 Você · T1"));
        assert!(back.contains("qual a capital da França?"));
        assert!(back.contains("🤖 Abyss · gemini-2.5-flash · T1"));
        assert!(back.contains("Paris."));
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
