// Hide the console window on Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
//! `claudio-gui` — desktop interface (egui) over the same core as the CLI.

mod admin;
mod friendly;
mod onboarding;
mod settings;
mod theme;
mod voice_ui;

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use claudio_core::config::Config;
use claudio_core::events::{
    AgentEvent, ApprovalDecision, ApprovalRequest, Approver, ElevationRequest, ElevationResponse,
    EventSink,
};
use claudio_core::plan::{Plan, StepStatus};
use claudio_core::secrets::Secrets;
use claudio_core::store::{MemoryRecord, SessionRecord, SessionStatus};
use claudio_core::RunOutcome;
use claudio_llm::router::Availability;
use claudio_llm::{Part, Role};
use claudio_tools::runtime::Runtime;
use claudio_tools::setup;
use eframe::egui::{self, Color32, CornerRadius, Margin, RichText, Stroke, Vec2};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

/// Messages from background tasks to the UI thread.
enum UiMsg {
    Event(AgentEvent),
    Approval(ApprovalRequest, oneshot::Sender<ApprovalDecision>),
    Elevation(ElevationRequest, oneshot::Sender<ElevationResponse>),
    Done(Result<RunOutcome, String>),
    Availability(Vec<(String, Availability)>),
}

struct UiSink {
    tx: mpsc::UnboundedSender<UiMsg>,
    ctx: egui::Context,
}

impl EventSink for UiSink {
    fn emit(&self, e: AgentEvent) {
        let _ = self.tx.send(UiMsg::Event(e));
        self.ctx.request_repaint();
    }
}

struct GuiApprover {
    tx: mpsc::UnboundedSender<UiMsg>,
    ctx: egui::Context,
}

#[async_trait]
impl Approver for GuiApprover {
    async fn approve(&self, req: ApprovalRequest) -> ApprovalDecision {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(UiMsg::Approval(req, tx)).is_err() {
            return ApprovalDecision::Deny;
        }
        self.ctx.request_repaint();
        rx.await.unwrap_or(ApprovalDecision::Deny)
    }

    async fn elevate(&self, req: ElevationRequest) -> ElevationResponse {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(UiMsg::Elevation(req, tx)).is_err() {
            return ElevationResponse::Denied;
        }
        self.ctx.request_repaint();
        rx.await.unwrap_or(ElevationResponse::Denied)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Tone {
    Muted,
    Success,
    Warning,
    Error,
}

#[derive(Clone)]
enum ChatItem {
    User(String),
    Assistant(String),
    Tool {
        name: String,
        summary: String,
        ok: Option<bool>,
        preview: String,
    },
    Note(String, Tone),
    /// The task stopped early and can be continued.
    Continue(String),
    /// No AI is configured: offer the setup assistant.
    SetupNeeded,
}

#[derive(PartialEq)]
enum RightTab {
    Plan,
    Memory,
    Log,
}

struct App {
    tokio: tokio::runtime::Runtime,
    rt: Arc<Runtime>,
    tx: mpsc::UnboundedSender<UiMsg>,
    rx: mpsc::UnboundedReceiver<UiMsg>,
    input: String,
    items: Vec<ChatItem>,
    activity: Vec<String>,
    plan: Plan,
    session_id: Option<String>,
    sessions: Vec<SessionRecord>,
    memories: Vec<MemoryRecord>,
    new_memory: String,
    model: String,
    availability: Vec<(String, Availability)>,
    running: Option<CancellationToken>,
    approvals: Vec<(ApprovalRequest, Option<oneshot::Sender<ApprovalDecision>>)>,
    admin_prompts: Vec<admin::AdminPrompt>,
    tab: RightTab,
    show_details: bool,
    /// The agent is checking its own result: keep that out of the chat.
    verifying: bool,
    /// The current task passed the self-check.
    checked: bool,
    status: String,
    search: String,
    settings: settings::Settings,
    onboarding: onboarding::Onboarding,
    logo: Option<egui::TextureHandle>,
    voice: voice_ui::VoiceUi,
    /// The last message came from the microphone: answer aloud.
    voice_turn: bool,
    /// Answer to read aloud once the task finishes.
    pending_speak: Option<String>,
}

impl App {
    fn new(cc: &eframe::CreationContext, tokio: tokio::runtime::Runtime, rt: Arc<Runtime>) -> Self {
        theme::apply(&cc.egui_ctx, &rt.cfg.ui.theme);
        let (tx, rx) = mpsc::unbounded_channel();
        let model = rt.cfg.default_model.clone().unwrap_or_else(|| {
            rt.cfg
                .models
                .first()
                .map(|m| m.name.clone())
                .unwrap_or_default()
        });
        let settings = settings::Settings::new(&rt.cfg);
        let first_run = !rt.cfg.ui.onboarded && !setup::ai_ready(&rt.cfg);
        let logo = image::load_from_memory(include_bytes!(
            "../../../packaging/icons/claudio-code-256.png"
        ))
        .ok()
        .map(|i| {
            let i = i.to_rgba8();
            let img = egui::ColorImage::from_rgba_unmultiplied(
                [i.width() as usize, i.height() as usize],
                i.as_raw(),
            );
            cc.egui_ctx.load_texture("logo", img, Default::default())
        });
        let mut app = Self {
            settings,
            onboarding: onboarding::Onboarding::new(first_run),
            tokio,
            rt,
            tx,
            rx,
            input: String::new(),
            items: Vec::new(),
            activity: Vec::new(),
            plan: Plan::default(),
            session_id: None,
            sessions: Vec::new(),
            memories: Vec::new(),
            new_memory: String::new(),
            model,
            availability: Vec::new(),
            running: None,
            approvals: Vec::new(),
            admin_prompts: Vec::new(),
            tab: RightTab::Plan,
            show_details: false,
            verifying: false,
            checked: false,
            status: String::new(),
            search: String::new(),
            logo,
            voice: voice_ui::VoiceUi::default(),
            voice_turn: false,
            pending_speak: None,
        };
        for w in app.rt.warnings.clone() {
            app.activity.push(format!("aviso: {w}"));
        }
        app.refresh_lists();
        app.refresh_availability(&cc.egui_ctx);
        app
    }

    fn refresh_lists(&mut self) {
        self.sessions = self.rt.store.list_sessions(200).unwrap_or_default();
        self.memories = self.rt.store.list_memories().unwrap_or_default();
    }

    fn refresh_availability(&self, ctx: &egui::Context) {
        let rt = self.rt.clone();
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        self.tokio.spawn(async move {
            let a = rt.availability().await;
            let mut v: Vec<_> = rt
                .cfg
                .models
                .iter()
                .map(|m| {
                    (
                        m.name.clone(),
                        a.get(&m.name).cloned().unwrap_or(Availability::Ready),
                    )
                })
                .collect();
            v.sort_by_key(|(n, _)| rt.cfg.models.iter().position(|m| &m.name == n));
            let _ = tx.send(UiMsg::Availability(v));
            ctx.request_repaint();
        });
    }

    /// Whether the selected AI still needs to be set up.
    fn needs_setup(&self) -> bool {
        !setup::ai_ready(&self.rt.cfg)
            && self.rt.cfg.model(&self.model).is_none_or(|m| m.is_local())
    }

    fn new_chat(&mut self) {
        if self.running.is_some() {
            return;
        }
        self.session_id = None;
        self.items.clear();
        self.activity.clear();
        self.plan = Plan::default();
        self.status.clear();
    }

    fn load_session(&mut self, id: &str) {
        self.items.clear();
        self.activity.clear();
        let Ok(Some(rec)) = self.rt.store.get_session(id) else {
            return;
        };
        self.plan = rec.plan.clone();
        self.session_id = Some(rec.id.clone());
        // The self-check phase is internal: keep it out of the conversation.
        let mut verifying = false;
        for m in self.rt.store.messages(&rec.id).unwrap_or_default() {
            match m.role {
                Role::System => {}
                Role::User => {
                    let t = m.text_content();
                    if t.starts_with(claudio_core::agent::SYS_PREFIX) {
                        verifying = t.contains("FASE DE VERIFICAÇÃO");
                        self.activity.push(t);
                    } else {
                        verifying = false;
                        self.items.push(ChatItem::User(t));
                    }
                }
                Role::Assistant if verifying => {}
                Role::Tool if verifying => {}
                Role::Assistant => {
                    let t = m.text_content();
                    if !t.trim().is_empty() {
                        self.items.push(ChatItem::Assistant(t));
                    }
                    for c in m
                        .tool_calls()
                        .into_iter()
                        .filter(|c| c.name != "update_plan")
                    {
                        self.items.push(ChatItem::Tool {
                            name: c.name,
                            summary: c.arguments.to_string().chars().take(160).collect(),
                            ok: None,
                            preview: String::new(),
                        });
                    }
                }
                Role::Tool => {
                    for p in &m.parts {
                        if let Part::ToolResult {
                            name,
                            content,
                            is_error,
                            ..
                        } = p
                        {
                            if let Some(ChatItem::Tool { ok, preview, .. }) = self.items.iter_mut().rev().find(|i| matches!(i, ChatItem::Tool { name: n, ok: None, .. } if n == name)) {
                                *ok = Some(!is_error);
                                *preview = content.chars().take(300).collect();
                            }
                        }
                    }
                }
            }
        }
        if rec.status.resumable() {
            self.items.push(ChatItem::Continue(rec.id.clone()));
        }
        self.status.clear();
    }

    fn send(&mut self, ctx: &egui::Context) {
        let text = self.input.trim().to_string();
        if text.is_empty() || self.running.is_some() {
            return;
        }
        self.input.clear();
        self.items.retain(|i| !matches!(i, ChatItem::Continue(_)));
        self.items.push(ChatItem::User(text.clone()));
        let cancel = CancellationToken::new();
        self.running = Some(cancel.clone());
        self.status = "Pensando…".into();
        self.checked = false;
        let rt = self.rt.clone();
        let tx = self.tx.clone();
        let model = self.model.clone();
        let session = self.session_id.clone();
        let ctx2 = ctx.clone();
        self.tokio.spawn(async move {
            let approver: Arc<dyn Approver> = Arc::new(GuiApprover {
                tx: tx.clone(),
                ctx: ctx2.clone(),
            });
            let sink: Arc<dyn EventSink> = Arc::new(UiSink {
                tx: tx.clone(),
                ctx: ctx2.clone(),
            });
            let result = async {
                let sel = rt
                    .select_model(Some(&model), &text, approver.as_ref())
                    .await?;
                sink.emit(AgentEvent::ModelSelected {
                    model: sel.provider.id(),
                    reason: sel.reason.clone(),
                });
                let agent = rt.agent(&sel, approver, sink);
                match session {
                    Some(id) => agent.send(&id, &text, cancel).await,
                    None => agent.start(&text, cancel).await,
                }
            }
            .await
            .map_err(|e| format!("{e:#}"));
            let _ = tx.send(UiMsg::Done(result));
            ctx2.request_repaint();
        });
    }

    fn handle(&mut self, msg: UiMsg) {
        match msg {
            UiMsg::Availability(a) => self.availability = a,
            UiMsg::Approval(req, tx) => self.approvals.push((req, Some(tx))),
            UiMsg::Elevation(req, tx) => self.admin_prompts.push(admin::AdminPrompt::new(req, tx)),
            UiMsg::Done(r) => {
                self.running = None;
                self.verifying = false;
                self.status.clear();
                if (self.voice.conversation || self.voice_turn) && self.rt.cfg.voice.speak_replies {
                    let last = self.items.iter().rev().find_map(|i| match i {
                        ChatItem::Assistant(t) => Some(t.clone()),
                        ChatItem::User(_) => Some(String::new()),
                        _ => None,
                    });
                    self.pending_speak = Some(last.unwrap_or_default());
                }
                match r {
                    Ok(out) => {
                        self.session_id = Some(out.session_id.clone());
                        match out.status {
                            SessionStatus::Completed if self.checked => {
                                self.items.push(ChatItem::Note(
                                    "✔ Pronto — o resultado foi conferido.".into(),
                                    Tone::Success,
                                ))
                            }
                            SessionStatus::Completed => {}
                            SessionStatus::CompletedUnverified => {}
                            SessionStatus::Cancelled => self
                                .items
                                .push(ChatItem::Note("Tarefa interrompida.".into(), Tone::Muted)),
                            SessionStatus::LimitReached | SessionStatus::Interrupted => {
                                self.items.push(ChatItem::Note(
                                    "A tarefa ficou longa e eu fiz uma pausa.".into(),
                                    Tone::Warning,
                                ));
                                self.items.push(ChatItem::Continue(out.session_id.clone()));
                            }
                            _ => self.items.push(ChatItem::Note(
                                format!("Não consegui terminar: {}", out.summary),
                                Tone::Error,
                            )),
                        }
                    }
                    Err(e) => {
                        self.activity.push(format!("erro: {e}"));
                        let msg = setup::friendly_error(&e);
                        if msg.contains("Configurar IA") {
                            self.items.push(ChatItem::SetupNeeded);
                        } else {
                            self.items.push(ChatItem::Note(msg, Tone::Error));
                        }
                    }
                }
                self.refresh_lists();
            }
            UiMsg::Event(e) => {
                let ts = chrono::Local::now().format("%H:%M:%S").to_string();
                match e {
                    AgentEvent::SessionStarted { session_id, .. } => {
                        self.session_id = Some(session_id)
                    }
                    AgentEvent::ModelSelected { model, reason } => self
                        .activity
                        .push(format!("{ts} modelo {model} ({reason})")),
                    AgentEvent::ModelCall { n, phase } => {
                        self.activity
                            .push(format!("{ts} chamada ao modelo #{n} ({phase})"));
                    }
                    AgentEvent::AssistantText { text } => {
                        if self.verifying {
                            self.activity.push(format!("{ts} verificação: {text}"));
                        } else {
                            self.items.push(ChatItem::Assistant(text))
                        }
                    }
                    AgentEvent::PlanUpdated { plan } => self.plan = plan,
                    AgentEvent::ToolCall {
                        name,
                        summary,
                        risk,
                        ..
                    } => {
                        self.activity
                            .push(format!("{ts} ▶ {name} [{}] {summary}", risk.label()));
                        if self.verifying {
                            return;
                        }
                        self.status = format!("{}…", friendly::tool(&name).1);
                        self.items.push(ChatItem::Tool {
                            name,
                            summary,
                            ok: None,
                            preview: String::new(),
                        });
                    }
                    AgentEvent::ToolDecision { name, decision, .. } => {
                        self.activity.push(format!("{ts}   {name}: {decision}"))
                    }
                    AgentEvent::ToolResult {
                        name,
                        ok,
                        preview,
                        ms,
                        ..
                    } => {
                        self.activity.push(format!(
                            "{ts}   {} {name} ({ms} ms)",
                            if ok { "✔" } else { "✖" }
                        ));
                        if !self.verifying {
                            self.status = "Pensando…".into();
                        }
                        if let Some(ChatItem::Tool {
                            ok: o, preview: p, ..
                        }) = self.items.iter_mut().rev().find(
                            |i| matches!(i, ChatItem::Tool { name: n, ok: None, .. } if *n == name),
                        ) {
                            *o = Some(ok);
                            *p = preview;
                        }
                    }
                    AgentEvent::Warning { message } => {
                        self.activity.push(format!("{ts} ! {message}"));
                    }
                    AgentEvent::VerificationStarted { round } => {
                        self.verifying = true;
                        self.status = "Conferindo se ficou tudo certo…".into();
                        self.activity
                            .push(format!("{ts} verificação (rodada {round})"));
                    }
                    AgentEvent::VerificationResult { passed, notes } => {
                        self.verifying = false;
                        self.checked = passed;
                        self.activity.push(format!(
                            "{ts} verificação: {}",
                            if passed {
                                "PASS".into()
                            } else {
                                format!("FAIL — {notes}")
                            }
                        ));
                        if !passed {
                            self.items.push(ChatItem::Note(
                                "Encontrei um problema no resultado e vou tentar corrigir.".into(),
                                Tone::Warning,
                            ));
                        }
                    }
                    AgentEvent::LimitReached { limit } => {
                        self.activity.push(format!("{ts} limite: {limit}"))
                    }
                    AgentEvent::Finished { status, .. } => {
                        self.activity.push(format!("{ts} fim: {}", status.as_str()))
                    }
                    AgentEvent::Log { message } => self.activity.push(format!("{ts} {message}")),
                }
            }
        }
    }

    fn resume(&mut self, ctx: &egui::Context, id: String) {
        if self.running.is_some() {
            return;
        }
        self.items.retain(|i| !matches!(i, ChatItem::Continue(_)));
        let cancel = CancellationToken::new();
        self.running = Some(cancel.clone());
        self.status = "Continuando…".into();
        self.checked = false;
        let rt = self.rt.clone();
        let tx = self.tx.clone();
        let model = self.model.clone();
        let ctx2 = ctx.clone();
        self.tokio.spawn(async move {
            let approver: Arc<dyn Approver> = Arc::new(GuiApprover {
                tx: tx.clone(),
                ctx: ctx2.clone(),
            });
            let sink: Arc<dyn EventSink> = Arc::new(UiSink {
                tx: tx.clone(),
                ctx: ctx2.clone(),
            });
            let result = async {
                let sel = rt.select_model(Some(&model), "", approver.as_ref()).await?;
                rt.agent(&sel, approver, sink).resume(&id, cancel).await
            }
            .await
            .map_err(|e| format!("{e:#}"));
            let _ = tx.send(UiMsg::Done(result));
            ctx2.request_repaint();
        });
    }

    /// Save `cfg` (and the keys), then rebuild the runtime with it.
    fn apply_config(&mut self, ctx: &egui::Context, cfg: &Config, secrets: &Secrets) -> bool {
        let result = (|| -> anyhow::Result<Config> {
            let path = cfg.save()?;
            secrets.save_to(&claudio_core::secrets::secrets_path_for(cfg))?;
            let (cfg, _) = Config::load_with_secrets(Some(&path))?;
            Ok(cfg)
        })();
        let cfg = match result {
            Ok(c) => c,
            Err(e) => {
                self.items.push(ChatItem::Note(
                    format!("Não consegui salvar as configurações: {e:#}"),
                    Tone::Error,
                ));
                return false;
            }
        };
        self.tokio.block_on(self.rt.shutdown());
        match self.tokio.block_on(Runtime::init(cfg)) {
            Ok(rt) => {
                self.rt = Arc::new(rt);
                if let Some(d) = self.rt.cfg.default_model.clone() {
                    self.model = d;
                }
                theme::apply(ctx, &self.rt.cfg.ui.theme);
                self.settings.reset(&self.rt.cfg);
                self.refresh_availability(ctx);
                self.refresh_lists();
                true
            }
            Err(e) => {
                self.items.push(ChatItem::Note(
                    format!("Não consegui aplicar as configurações: {e:#}"),
                    Tone::Error,
                ));
                false
            }
        }
    }

    fn apply_settings(&mut self, ctx: &egui::Context) {
        let cfg = self.settings.draft.clone();
        let secrets = self.settings.secrets.clone();
        if self.apply_config(ctx, &cfg, &secrets) {
            self.settings.saved_message();
        }
    }

    fn finish_onboarding(&mut self, ctx: &egui::Context, outcome: onboarding::Outcome) {
        match outcome {
            onboarding::Outcome::None => {}
            onboarding::Outcome::Skipped => {
                let mut cfg = (*self.rt.cfg).clone();
                cfg.ui.onboarded = true;
                let _ = cfg.save();
            }
            onboarding::Outcome::Finished { cfg, key, cloud } => {
                let mut cfg = *cfg;
                cfg.ui.onboarded = true;
                // Default work folder in Documents, easy to find.
                if cfg.permissions.workspace_roots == ["~/claudio-work"] {
                    let ws = setup::default_workspace();
                    let _ = std::fs::create_dir_all(&ws);
                    cfg.permissions.workspace_roots = vec![ws.to_string_lossy().into_owned()];
                }
                let path = claudio_core::secrets::secrets_path_for(&cfg);
                let mut secrets = Secrets::load_from(&path).unwrap_or_default();
                if let Some((name, k)) = key {
                    secrets.set(&name, &k);
                }
                if !self.apply_config(ctx, &cfg, &secrets) {
                    return;
                }
                // Choosing a cloud service in the assistant is the user's explicit consent.
                if let Some(entry) = cloud {
                    let _ = self
                        .rt
                        .store
                        .grant_consent(&claudio_core::consent::consent_key(&entry));
                }
                self.items.retain(|i| !matches!(i, ChatItem::SetupNeeded));
                self.items.push(ChatItem::Note(
                    format!(
                        "✔ Tudo pronto! Sua IA: {}. Os arquivos que eu criar ficam em {}.",
                        setup::ai_label(&self.rt.cfg, &self.model),
                        self.rt.guard.cwd().display()
                    ),
                    Tone::Success,
                ));
                // Warm up the local AI so the first answer comes faster.
                if let Some(m) = self.rt.cfg.model(&self.model).cloned() {
                    if m.gguf_path.is_some() {
                        let rt = self.rt.clone();
                        self.tokio.spawn(async move {
                            let _ = rt.start_local(&m).await;
                        });
                    }
                }
            }
        }
    }

    fn voice_events(&mut self, ctx: &egui::Context) {
        if let Some(text) = self.pending_speak.take() {
            if text.trim().is_empty() {
                if self.voice.conversation {
                    self.voice.listen(&self.rt.cfg, &self.tokio, ctx);
                }
            } else {
                self.voice.speak(&self.rt.cfg, text, &self.tokio, ctx);
            }
        }
        let cfg = self.rt.cfg.clone();
        for ev in self.voice.poll(&cfg, &self.tokio, ctx) {
            match ev {
                voice_ui::VoiceEvent::Heard(t) => {
                    if self.running.is_none() {
                        self.input = t;
                        self.voice_turn = true;
                        self.send(ctx);
                    }
                }
                voice_ui::VoiceEvent::Silence => {
                    if self.voice.conversation {
                        self.voice.conversation = false;
                        self.items.push(ChatItem::Note(
                            "Conversa por voz pausada: não ouvi nada. Clique em “Conversa por voz” para continuar.".into(),
                            Tone::Muted,
                        ));
                    }
                }
                voice_ui::VoiceEvent::Error(e) => self.items.push(ChatItem::Note(
                    format!(
                        "Voz: {}",
                        setup::friendly_error(&e)
                            .replace("Algo deu errado. Detalhes técnicos: ", "")
                    ),
                    Tone::Warning,
                )),
                voice_ui::VoiceEvent::Configured(new_cfg) => {
                    let secrets =
                        Secrets::load_from(&claudio_core::secrets::secrets_path_for(&new_cfg))
                            .unwrap_or_default();
                    if self.apply_config(ctx, &new_cfg, &secrets) {
                        self.items.push(ChatItem::Note(
                            "✔ Voz ativada. Clique no 🎤 e fale; eu respondo em voz alta.".into(),
                            Tone::Success,
                        ));
                        let cfg = self.rt.cfg.clone();
                        self.voice.after_configured(&cfg, &self.tokio, ctx);
                    }
                }
            }
        }
    }

    fn approval_window(&mut self, ctx: &egui::Context) {
        let Some((req, _)) = self.approvals.first() else {
            return;
        };
        let req = req.clone();
        let p = theme::current(ctx);
        let mut decision = None;
        let remote = req.tool == "remote_model";
        let title = if remote {
            "Enviar sua conversa para a nuvem?"
        } else {
            friendly::approval_title(&req.tool, req.risk)
        };
        egui::Modal::new(egui::Id::new("approval"))
            .frame(theme::modal_frame(ctx))
            .show(ctx, |ui| {
                ui.set_width(460.0_f32.min(ctx.screen_rect().width() - 80.0));
                let icon = if remote { "☁" } else { friendly::tool(&req.tool).0 };
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icon).size(28.0));
                    ui.add(egui::Label::new(RichText::new(title).size(19.0).strong()).wrap());
                });
                ui.label(
                    RichText::new(if remote {
                        "Para responder, a IA na nuvem recebe o texto da conversa e dos arquivos usados."
                    } else {
                        friendly::risk_hint(req.risk)
                    })
                    .color(p.muted),
                );
                theme::card(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new("O que vai acontecer:").small().color(p.muted));
                    ui.add(egui::Label::new(RichText::new(&req.summary).monospace()).wrap());
                });
                egui::CollapsingHeader::new(
                    RichText::new("Detalhes técnicos").small().color(p.muted),
                )
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(format!(
                            "{} · {} · {}",
                            req.tool,
                            req.risk.label(),
                            req.reason
                        ))
                        .small(),
                    );
                    egui::ScrollArea::vertical()
                        .max_height(160.0)
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(
                                    serde_json::to_string_pretty(&req.args).unwrap_or_default(),
                                )
                                .monospace()
                                .small(),
                            );
                        });
                });
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    if theme::primary_button(ui, "  Permitir  ").clicked() {
                        decision = Some(ApprovalDecision::AllowOnce);
                    }
                    let session_label = if remote {
                        "Sempre permitir"
                    } else {
                        "Permitir nesta conversa"
                    };
                    if req.allow_session && theme::pill_button(ui, session_label).clicked() {
                        decision = Some(ApprovalDecision::AllowSession);
                    }
                    if theme::pill_button(ui, RichText::new("Não permitir").color(p.danger))
                        .clicked()
                    {
                        decision = Some(ApprovalDecision::Deny);
                    }
                });
            });
        if let Some(d) = decision {
            let (_, tx) = self.approvals.remove(0);
            if let Some(tx) = tx {
                let _ = tx.send(d);
            }
        }
    }

    fn sidebar(&mut self, ctx: &egui::Context) {
        let p = theme::current(ctx);
        egui::SidePanel::left("sessions")
            .exact_width(250.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(p.sidebar)
                    .inner_margin(Margin::symmetric(14, 16)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if let Some(t) = &self.logo {
                        ui.add(
                            egui::Image::new(t)
                                .fit_to_exact_size(Vec2::splat(30.0))
                                .corner_radius(8),
                        );
                    }
                    ui.label(RichText::new("Claudio").size(21.0).strong());
                });
                ui.add_space(8.0);
                let w = ui.available_width();
                if ui
                    .add_enabled(
                        self.running.is_none(),
                        egui::Button::new(
                            RichText::new("+   Nova conversa")
                                .strong()
                                .color(p.on_accent),
                        )
                        .fill(p.accent)
                        .corner_radius(CornerRadius::same(14))
                        .min_size(Vec2::new(w, 42.0)),
                    )
                    .clicked()
                {
                    self.new_chat();
                }
                ui.add_space(6.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text("🔍 Procurar conversas")
                        .desired_width(w)
                        .margin(Vec2::new(10.0, 8.0)),
                );
                ui.add_space(6.0);
                ui.label(RichText::new("Conversas").small().color(p.muted));
                let mut open = None;
                let mut resume = None;
                egui::ScrollArea::vertical()
                    .max_height((ui.available_height() - 54.0).max(60.0))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let q = self.search.to_lowercase();
                        let mut any = false;
                        for s in self
                            .sessions
                            .iter()
                            .filter(|s| s.parent_id.is_none())
                            .filter(|s| q.is_empty() || s.title.to_lowercase().contains(&q))
                        {
                            any = true;
                            let selected = self.session_id.as_deref() == Some(&s.id);
                            let title: String = s.title.chars().take(30).collect();
                            let r = ui.add(
                                egui::Button::new(RichText::new(title).color(p.text))
                                    .fill(if selected {
                                        p.accent_soft
                                    } else {
                                        Color32::TRANSPARENT
                                    })
                                    .stroke(Stroke::NONE)
                                    .corner_radius(CornerRadius::same(10))
                                    .min_size(Vec2::new(w, 34.0)),
                            );
                            if r.clicked() {
                                open = Some(s.id.clone());
                            }
                            r.context_menu(|ui| {
                                if s.status.resumable() && ui.button("Continuar a tarefa").clicked()
                                {
                                    resume = Some(s.id.clone());
                                    ui.close_menu();
                                }
                            });
                        }
                        if !any {
                            ui.label(
                                RichText::new("Suas conversas aparecem aqui.")
                                    .small()
                                    .color(p.muted),
                            );
                        }
                    });
                if let Some(id) = open {
                    if self.running.is_none() {
                        self.load_session(&id);
                    }
                }
                if let Some(id) = resume {
                    self.load_session(&id);
                    self.resume(ctx, id);
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    if ui
                        .add(
                            egui::Button::new("⚙  Configurações")
                                .fill(Color32::TRANSPARENT)
                                .corner_radius(CornerRadius::same(10))
                                .min_size(Vec2::new(w, 36.0)),
                        )
                        .clicked()
                    {
                        self.settings.reset(&self.rt.cfg);
                        self.settings.open = true;
                    }
                });
            });
    }

    fn top_bar(&mut self, ctx: &egui::Context) {
        let p = theme::current(ctx);
        egui::TopBottomPanel::top("top")
            .frame(
                egui::Frame::new()
                    .fill(p.bg)
                    .inner_margin(Margin::symmetric(18, 10)),
            )
            .show_separator_line(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let cfg = self.rt.cfg.clone();
                    let current = setup::ai_label(&cfg, &self.model);
                    let ready = !self.needs_setup();
                    let dot_color = if ready { p.success } else { p.warning };
                    egui::Frame::new()
                        .fill(p.surface)
                        .stroke(Stroke::new(1.0_f32, p.border))
                        .corner_radius(20)
                        .inner_margin(Margin::symmetric(12, 4))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                status_dot(ui, dot_color);
                                egui::ComboBox::from_id_salt("model")
                                    .selected_text(RichText::new(current).strong())
                                    .width(240.0)
                                    .show_ui(ui, |ui| {
                                        for m in cfg.models.iter() {
                                            let usable = setup::ai_ready(&Config {
                                                default_model: Some(m.name.clone()),
                                                ..(*cfg).clone()
                                            });
                                            if !usable && m.name != self.model {
                                                continue;
                                            }
                                            let label = setup::ai_label(&cfg, &m.name);
                                            ui.selectable_value(
                                                &mut self.model,
                                                m.name.clone(),
                                                label,
                                            );
                                        }
                                        ui.separator();
                                        if ui.button("+ Configurar outra IA…").clicked() {
                                            self.onboarding.start();
                                        }
                                    });
                            });
                        });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let label = if self.show_details {
                            "Ocultar detalhes"
                        } else {
                            "Detalhes"
                        };
                        if theme::pill_button(ui, format!("ℹ  {label}")).clicked() {
                            self.show_details = !self.show_details;
                        }
                        let on = self.voice.conversation;
                        let b = egui::Button::new(
                            RichText::new("🎤  Conversa por voz").color(if on { p.on_accent } else { p.text }),
                        )
                        .fill(if on { p.accent } else { p.surface_hover })
                        .corner_radius(CornerRadius::same(22))
                        .min_size(Vec2::new(0.0, 34.0));
                        if ui
                            .add(b)
                            .on_hover_text("Fale e ouça as respostas; o Claudio volta a ouvir depois de responder")
                            .clicked()
                        {
                            if on {
                                self.voice.conversation = false;
                                self.voice.stop();
                            } else {
                                self.voice.conversation = true;
                                if self.running.is_none() {
                                    self.voice.listen(&self.rt.cfg, &self.tokio, ctx);
                                }
                            }
                        }
                    });
                });
            });
    }

    fn details_panel(&mut self, ctx: &egui::Context) {
        if !self.show_details {
            return;
        }
        let p = theme::current(ctx);
        egui::SidePanel::right("details")
            .default_width(320.0)
            .frame(
                egui::Frame::new()
                    .fill(p.sidebar)
                    .inner_margin(Margin::same(14)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.tab, RightTab::Plan, "Passos");
                    ui.selectable_value(&mut self.tab, RightTab::Memory, "Lembranças");
                    ui.selectable_value(&mut self.tab, RightTab::Log, "Registro técnico");
                });
                ui.add_space(6.0);
                match self.tab {
                    RightTab::Plan => {
                        let (done, total) = self.plan.progress();
                        if total == 0 {
                            ui.label(
                                RichText::new(
                                    "Quando eu montar um plano, os passos aparecem aqui.",
                                )
                                .color(p.muted),
                            );
                        } else {
                            ui.add(
                                egui::ProgressBar::new(done as f32 / total as f32)
                                    .corner_radius(6)
                                    .fill(p.accent)
                                    .text(format!("{done} de {total}")),
                            );
                            for s in &self.plan.steps {
                                let (icon, color) = match s.status {
                                    StepStatus::Completed => ("✔", p.success),
                                    StepStatus::InProgress => ("▶", p.accent),
                                    StepStatus::Failed => ("✖", p.danger),
                                    StepStatus::Skipped => ("–", p.muted),
                                    StepStatus::Pending => ("○", p.muted),
                                };
                                ui.label(
                                    RichText::new(format!("{icon}  {}", s.title)).color(color),
                                );
                                if let Some(n) = &s.note {
                                    ui.label(RichText::new(n).small().color(p.muted));
                                }
                            }
                        }
                    }
                    RightTab::Log => {
                        egui::ScrollArea::vertical()
                            .stick_to_bottom(true)
                            .show(ui, |ui| {
                                for l in &self.activity {
                                    ui.label(RichText::new(l).monospace().small());
                                }
                            });
                    }
                    RightTab::Memory => {
                        ui.label(
                            RichText::new("Coisas que o Claudio deve lembrar sobre você.")
                                .small()
                                .color(p.muted),
                        );
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.new_memory)
                                    .hint_text("ex.: Meu nome é Ana")
                                    .desired_width(ui.available_width() - 90.0),
                            );
                            if theme::pill_button(ui, "Salvar").clicked()
                                && !self.new_memory.trim().is_empty()
                            {
                                let mem = self.rt.memory.clone();
                                let text = std::mem::take(&mut self.new_memory);
                                let _ =
                                    self.tokio
                                        .block_on(mem.save("preference", &text, "", "user"));
                                self.refresh_lists();
                            }
                        });
                        let mut delete = None;
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            for m in &self.memories {
                                theme::card(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.horizontal(|ui| {
                                        if ui.small_button("🗑").on_hover_text("esquecer").clicked()
                                        {
                                            delete = Some(m.id);
                                        }
                                        ui.add(egui::Label::new(&m.content).wrap());
                                    });
                                });
                            }
                        });
                        if let Some(id) = delete {
                            let _ = self.rt.store.delete_memory(id);
                            self.refresh_lists();
                        }
                    }
                }
            });
    }

    fn input_panel(&mut self, ctx: &egui::Context) {
        let p = theme::current(ctx);
        egui::TopBottomPanel::bottom("input")
            .frame(egui::Frame::new().fill(p.bg).inner_margin(Margin {
                left: 24,
                right: 24,
                top: 6,
                bottom: 14,
            }))
            .show_separator_line(false)
            .show(ctx, |ui| {
                // Live progress while working.
                if self.running.is_some() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new(&self.status).color(p.muted));
                        let (done, total) = self.plan.progress();
                        if total > 0 {
                            ui.add(
                                egui::ProgressBar::new(done as f32 / total as f32)
                                    .desired_width(160.0)
                                    .desired_height(8.0)
                                    .corner_radius(4)
                                    .fill(p.accent),
                            );
                            ui.label(
                                RichText::new(format!(
                                    "passo {} de {total}",
                                    (done + 1).min(total)
                                ))
                                .small()
                                .color(p.muted),
                            );
                        }
                    });
                    ui.add_space(4.0);
                }
                let voice_line = match self.voice.state {
                    voice_ui::VoiceState::Waiting => Some("🎤 Pode falar, estou ouvindo…"),
                    voice_ui::VoiceState::Hearing => Some("🎤 Ouvindo…"),
                    voice_ui::VoiceState::Transcribing => Some("⏳ Entendendo o que você disse…"),
                    voice_ui::VoiceState::Speaking => {
                        Some("🔊 Falando… (clique no microfone para interromper)")
                    }
                    voice_ui::VoiceState::Idle => None,
                };
                if let Some(t) = voice_line {
                    ui.horizontal(|ui| {
                        if self.voice.state != voice_ui::VoiceState::Speaking {
                            ui.spinner();
                        }
                        ui.label(RichText::new(t).color(p.accent));
                    });
                    ui.add_space(4.0);
                }
                egui::Frame::new()
                    .fill(p.surface)
                    .stroke(Stroke::new(1.0_f32, p.border))
                    .corner_radius(24)
                    .inner_margin(Margin {
                        left: 18,
                        right: 8,
                        top: 8,
                        bottom: 8,
                    })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let enter = ui.input(|i| {
                                i.key_pressed(egui::Key::Enter) && !i.modifiers.shift
                            });
                            let edit = egui::TextEdit::multiline(&mut self.input)
                                .id_salt("chat_input")
                                .hint_text("Peça algo ao Claudio…")
                                .frame(false)
                                .desired_rows(1)
                                .desired_width(ui.available_width() - 104.0)
                                .return_key(Some(egui::KeyboardShortcut::new(
                                    egui::Modifiers::SHIFT,
                                    egui::Key::Enter,
                                )));
                            let r = ui.add(edit);
                            let send_key = r.has_focus() && enter;
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if self.running.is_some() {
                                        let b = egui::Button::new(
                                            RichText::new("⏹").size(18.0).color(p.on_accent),
                                        )
                                        .fill(p.danger)
                                        .corner_radius(CornerRadius::same(20))
                                        .min_size(Vec2::splat(40.0));
                                        if ui.add(b).on_hover_text("Parar").clicked() {
                                            if let Some(c) = &self.running {
                                                c.cancel();
                                            }
                                        }
                                    } else {
                                        let has_text = !self.input.trim().is_empty();
                                        let b = egui::Button::new(
                                            RichText::new("⬆").size(18.0).color(p.on_accent),
                                        )
                                        .fill(if has_text { p.accent } else { p.border })
                                        .corner_radius(CornerRadius::same(20))
                                        .min_size(Vec2::splat(40.0));
                                        if ui.add(b).on_hover_text("Enviar (Enter)").clicked()
                                            || send_key
                                        {
                                            self.voice_turn = false;
                                            self.send(ctx);
                                            r.request_focus();
                                        }
                                    }
                                    let listening = matches!(
                                        self.voice.state,
                                        voice_ui::VoiceState::Waiting | voice_ui::VoiceState::Hearing
                                    );
                                    let mic = egui::Button::new(
                                        RichText::new("🎤").size(18.0).color(if listening {
                                            p.on_accent
                                        } else {
                                            p.text
                                        }),
                                    )
                                    .fill(if listening { p.danger } else { p.surface_hover })
                                    .corner_radius(CornerRadius::same(20))
                                    .min_size(Vec2::splat(40.0));
                                    let tip = if listening { "Parar de ouvir" } else { "Falar (microfone)" };
                                    if ui
                                        .add_enabled(self.running.is_none(), mic)
                                        .on_hover_text(tip)
                                        .clicked()
                                    {
                                        if listening {
                                            self.voice.stop();
                                        } else {
                                            self.voice.listen(&self.rt.cfg, &self.tokio, ctx);
                                        }
                                    }
                                },
                            );
                        });
                    });
                ui.add_space(2.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new(
                            "Enter envia · Shift+Enter pula linha · O Claudio pede permissão antes de mudar algo",
                        )
                        .small()
                        .color(p.muted),
                    );
                });
            });
    }

    fn chat(&mut self, ctx: &egui::Context) {
        let p = theme::current(ctx);
        let mut resume = None;
        let mut open_setup = false;
        let mut suggestion = None;
        let needs_setup = self.needs_setup();
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(p.bg)
                    .inner_margin(Margin::symmetric(24, 8)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let max_w = ui.available_width().min(820.0);
                        let side = ((ui.available_width() - max_w) / 2.0).max(0.0);
                        ui.horizontal(|ui| {
                            ui.add_space(side);
                            ui.vertical(|ui| {
                                ui.set_width(max_w);
                                let fresh = !self.items.iter().any(|i| {
                                    matches!(i, ChatItem::User(_) | ChatItem::SetupNeeded)
                                });
                                if fresh {
                                    empty_state(
                                        ui,
                                        max_w,
                                        &self.logo,
                                        needs_setup,
                                        &mut open_setup,
                                        &mut suggestion,
                                    );
                                }
                                for item in &self.items {
                                    chat_item(
                                        ui,
                                        item,
                                        max_w,
                                        &self.logo,
                                        &mut resume,
                                        &mut open_setup,
                                    );
                                }
                                ui.add_space(12.0);
                            });
                        });
                    });
            });
        if let Some(s) = suggestion {
            self.input = s;
        }
        if let Some(id) = resume {
            self.resume(ctx, id);
        }
        if open_setup {
            self.onboarding.start();
        }
    }
}

fn chat_item(
    ui: &mut egui::Ui,
    item: &ChatItem,
    max_w: f32,
    logo: &Option<egui::TextureHandle>,
    resume: &mut Option<String>,
    open_setup: &mut bool,
) {
    let p = theme::current(ui.ctx());
    match item {
        ChatItem::User(t) => {
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                egui::Frame::new()
                    .fill(p.user_bubble)
                    .corner_radius(CornerRadius {
                        nw: 18,
                        ne: 18,
                        sw: 18,
                        se: 6,
                    })
                    .inner_margin(Margin::symmetric(16, 10))
                    .show(ui, |ui| {
                        ui.set_max_width(max_w * 0.75);
                        ui.add(egui::Label::new(RichText::new(t).color(Color32::WHITE)).wrap());
                    });
            });
        }
        ChatItem::Assistant(t) => {
            ui.add_space(8.0);
            ui.horizontal_top(|ui| {
                avatar(ui, logo);
                egui::Frame::new()
                    .fill(p.surface)
                    .stroke(Stroke::new(1.0_f32, p.border))
                    .corner_radius(CornerRadius {
                        nw: 6,
                        ne: 18,
                        sw: 18,
                        se: 18,
                    })
                    .inner_margin(Margin::symmetric(16, 12))
                    .show(ui, |ui| {
                        ui.set_max_width(max_w - 60.0);
                        ui.add(egui::Label::new(t.as_str()).wrap().selectable(true));
                    });
            });
        }
        ChatItem::Tool {
            name,
            summary,
            ok,
            preview,
        } => {
            let (icon, what) = friendly::tool(name);
            let (mark, color) = match ok {
                None => ("…", p.muted),
                Some(true) => ("✔", p.success),
                Some(false) => ("✖", p.warning),
            };
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.add_space(48.0);
                egui::Frame::new()
                    .fill(p.accent_soft)
                    .corner_radius(14)
                    .inner_margin(Margin::symmetric(10, 2))
                    .show(ui, |ui| {
                        egui::CollapsingHeader::new(
                            RichText::new(format!("{icon}  {what}  {mark}"))
                                .color(color)
                                .small(),
                        )
                        .id_salt(format!("{name}{summary}{}", preview.len()))
                        .show(ui, |ui| {
                            ui.set_max_width(max_w - 120.0);
                            ui.add(
                                egui::Label::new(RichText::new(summary).small().color(p.muted))
                                    .wrap(),
                            );
                            if !preview.is_empty() {
                                ui.add(
                                    egui::Label::new(RichText::new(preview).monospace().small())
                                        .wrap(),
                                );
                            }
                        });
                    });
            });
        }
        ChatItem::Note(t, tone) => {
            let c = match tone {
                Tone::Muted => p.muted,
                Tone::Success => p.success,
                Tone::Warning => p.warning,
                Tone::Error => p.danger,
            };
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(48.0);
                ui.add(egui::Label::new(RichText::new(t).color(c)).wrap());
            });
        }
        ChatItem::Continue(id) => {
            ui.horizontal(|ui| {
                ui.add_space(48.0);
                if theme::primary_button(ui, "  ▶  Continuar de onde parou  ").clicked() {
                    *resume = Some(id.clone());
                }
            });
        }
        ChatItem::SetupNeeded => {
            ui.add_space(8.0);
            setup_card(ui, max_w, open_setup);
        }
    }
}

fn setup_card(ui: &mut egui::Ui, width: f32, open_setup: &mut bool) {
    let p = theme::current(ui.ctx());
    theme::card(ui, |ui| {
        ui.set_width(width - 34.0);
        ui.label(
            RichText::new("Falta só um passo: escolher a IA")
                .size(17.0)
                .strong(),
        );
        ui.label(
            RichText::new(
                "Escolha se a IA roda no seu computador (grátis) ou na nuvem. Eu cuido da instalação.",
            )
            .color(p.muted),
        );
        if theme::primary_button(ui, "  Configurar IA  ").clicked() {
            *open_setup = true;
        }
    });
}

fn empty_state(
    ui: &mut egui::Ui,
    width: f32,
    logo: &Option<egui::TextureHandle>,
    needs_setup: bool,
    open_setup: &mut bool,
    suggestion: &mut Option<String>,
) {
    let p = theme::current(ui.ctx());
    ui.add_space((ui.available_height() * 0.10).max(20.0));
    ui.vertical_centered(|ui| {
        if let Some(t) = logo {
            ui.add(
                egui::Image::new(t)
                    .fit_to_exact_size(Vec2::splat(64.0))
                    .corner_radius(18),
            );
        }
        ui.label(RichText::new(greeting()).size(30.0).strong());
        ui.label(
            RichText::new("O que você quer que eu faça hoje?")
                .size(17.0)
                .color(p.muted),
        );
    });
    ui.add_space(18.0);
    if needs_setup {
        setup_card(ui, width, open_setup);
        ui.add_space(12.0);
    }
    let cols = if width > 600.0 { 2 } else { 1 };
    let spacing = ui.spacing().item_spacing.x;
    let cw = (width - spacing * (cols as f32 - 1.0)) / cols as f32;
    for row in friendly::SUGGESTIONS.chunks(cols) {
        ui.horizontal(|ui| {
            for (icon, text) in row {
                let r = egui::Frame::new()
                    .fill(p.surface)
                    .stroke(Stroke::new(1.0_f32, p.border))
                    .corner_radius(16)
                    .inner_margin(Margin::symmetric(14, 12))
                    .show(ui, |ui| {
                        ui.set_width(cw - 30.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(*icon).size(18.0));
                            ui.add(egui::Label::new(*text).wrap());
                        });
                    })
                    .response;
                let r = ui.interact(r.rect, r.id.with("sugg"), egui::Sense::click());
                if r.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if r.clicked() {
                    *suggestion = Some(text.to_string());
                }
            }
        });
    }
}

fn avatar(ui: &mut egui::Ui, logo: &Option<egui::TextureHandle>) {
    match logo {
        Some(t) => {
            ui.add(
                egui::Image::new(t)
                    .fit_to_exact_size(Vec2::splat(36.0))
                    .corner_radius(18),
            );
        }
        None => {
            ui.label(RichText::new("✨").size(24.0));
        }
    }
    ui.add_space(4.0);
}

fn status_dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.5, color);
}

fn greeting() -> &'static str {
    use chrono::Timelike;
    match chrono::Local::now().hour() {
        5..=11 => "Bom dia!",
        12..=17 => "Boa tarde!",
        _ => "Boa noite!",
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(m) = self.rx.try_recv() {
            self.handle(m);
        }
        self.voice_events(ctx);
        self.sidebar(ctx);
        self.top_bar(ctx);
        self.details_panel(ctx);
        self.input_panel(ctx);
        self.chat(ctx);

        self.approval_window(ctx);
        if let Some(prompt) = self.admin_prompts.first_mut() {
            if prompt.show(ctx) {
                self.admin_prompts.remove(0);
            }
        }
        match self.settings.show(ctx, &self.tokio, self.running.is_some()) {
            settings::Action::Save => self.apply_settings(ctx),
            settings::Action::Setup => {
                self.settings.open = false;
                self.onboarding.start();
            }
            settings::Action::None => {}
        }
        if std::mem::take(&mut self.settings.want_voice_setup) {
            self.settings.open = false;
            self.voice.open_setup();
        }
        let cfg = self.rt.cfg.clone();
        self.voice.setup_window(ctx, &cfg, &self.tokio);
        let outcome = self.onboarding.show(ctx, &cfg, &self.tokio);
        self.finish_onboarding(ctx, outcome);
    }
}

fn window_icon() -> egui::IconData {
    let img = image::load_from_memory(include_bytes!(
        "../../../packaging/icons/claudio-code-256.png"
    ))
    .map(|i| i.to_rgba8())
    .unwrap_or_else(|_| image::RgbaImage::new(1, 1));
    egui::IconData {
        width: img.width(),
        height: img.height(),
        rgba: img.into_raw(),
    }
}

fn main() -> eframe::Result<()> {
    let tokio = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio");
    let cfg = match Config::load_with_secrets(None) {
        Ok((c, _)) => c,
        Err(e) => {
            eprintln!("configuração inválida: {e:#}");
            std::process::exit(2);
        }
    };
    let rt = match tokio.block_on(Runtime::init(cfg)) {
        Ok(r) => Arc::new(r),
        Err(e) => {
            eprintln!("falha ao iniciar: {e:#}");
            std::process::exit(2);
        }
    };
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Claudio Code")
            .with_inner_size([1200.0, 780.0])
            .with_min_inner_size([760.0, 520.0])
            .with_app_id("claudio-code")
            .with_icon(window_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "Claudio Code",
        opts,
        Box::new(move |cc| Ok(Box::new(App::new(cc, tokio, rt)))),
    )
}
