// Hide the console window on Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
//! `claudio-gui` — desktop interface (egui) over the same core as the CLI.

mod settings;

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use claudio_core::config::Config;
use claudio_core::events::{AgentEvent, ApprovalDecision, ApprovalRequest, Approver, EventSink};
use claudio_core::permissions::Risk;
use claudio_core::plan::{Plan, StepStatus};
use claudio_core::store::{MemoryRecord, SessionRecord, SessionStatus};
use claudio_core::RunOutcome;
use claudio_llm::router::Availability;
use claudio_llm::{Part, Role};
use claudio_tools::runtime::Runtime;
use eframe::egui::{self, Color32, RichText};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

/// Messages from background tasks to the UI thread.
enum UiMsg {
    Event(AgentEvent),
    Approval(ApprovalRequest, oneshot::Sender<ApprovalDecision>),
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
    Note(String, Color32),
}

#[derive(PartialEq)]
enum RightTab {
    Plan,
    Activity,
    Memory,
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
    tab: RightTab,
    status: String,
    search: String,
    settings: settings::Settings,
}

impl App {
    fn new(cc: &eframe::CreationContext, tokio: tokio::runtime::Runtime, rt: Arc<Runtime>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let (tx, rx) = mpsc::unbounded_channel();
        let model = rt.cfg.default_model.clone().unwrap_or_else(|| {
            rt.cfg
                .models
                .first()
                .map(|m| m.name.clone())
                .unwrap_or_default()
        });
        let settings = settings::Settings::new(&rt.cfg);
        let mut app = Self {
            settings,
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
            tab: RightTab::Plan,
            status: "pronto".into(),
            search: String::new(),
        };
        for w in app.rt.warnings.clone() {
            app.items.push(ChatItem::Note(w, Color32::YELLOW));
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

    fn load_session(&mut self, id: &str) {
        self.items.clear();
        self.activity.clear();
        let Ok(Some(rec)) = self.rt.store.get_session(id) else {
            return;
        };
        self.plan = rec.plan.clone();
        self.session_id = Some(rec.id.clone());
        for m in self.rt.store.messages(&rec.id).unwrap_or_default() {
            match m.role {
                Role::System => {}
                Role::User => {
                    let t = m.text_content();
                    if t.starts_with(claudio_core::agent::SYS_PREFIX) {
                        self.items.push(ChatItem::Note(t, Color32::GRAY));
                    } else {
                        self.items.push(ChatItem::User(t));
                    }
                }
                Role::Assistant => {
                    let t = m.text_content();
                    if !t.trim().is_empty() {
                        self.items.push(ChatItem::Assistant(t));
                    }
                    for c in m.tool_calls() {
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
        self.status = format!("sessão {} — {}", &rec.id[..8], rec.status.as_str());
    }

    fn send(&mut self, ctx: &egui::Context) {
        let text = self.input.trim().to_string();
        if text.is_empty() || self.running.is_some() {
            return;
        }
        self.input.clear();
        self.items.push(ChatItem::User(text.clone()));
        let cancel = CancellationToken::new();
        self.running = Some(cancel.clone());
        self.status = "trabalhando…".into();
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
            UiMsg::Done(r) => {
                self.running = None;
                match r {
                    Ok(out) => {
                        self.session_id = Some(out.session_id.clone());
                        let (label, color) = match out.status {
                            SessionStatus::Completed => {
                                ("concluído e verificado", Color32::LIGHT_GREEN)
                            }
                            SessionStatus::CompletedUnverified => {
                                ("concluído (sem verificação)", Color32::LIGHT_YELLOW)
                            }
                            SessionStatus::Cancelled => ("cancelado", Color32::LIGHT_YELLOW),
                            SessionStatus::LimitReached => {
                                ("parado por limite — use Retomar", Color32::LIGHT_YELLOW)
                            }
                            _ => ("falhou", Color32::LIGHT_RED),
                        };
                        if !matches!(
                            out.status,
                            SessionStatus::Completed | SessionStatus::CompletedUnverified
                        ) {
                            self.items
                                .push(ChatItem::Note(format!("{label}: {}", out.summary), color));
                        }
                        self.status = label.into();
                    }
                    Err(e) => {
                        self.items
                            .push(ChatItem::Note(format!("erro: {e}"), Color32::LIGHT_RED));
                        self.status = "erro".into();
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
                        self.status = format!("pensando ({phase}, chamada {n})…");
                        self.activity
                            .push(format!("{ts} chamada ao modelo #{n} ({phase})"));
                    }
                    AgentEvent::AssistantText { text } => {
                        self.items.push(ChatItem::Assistant(text))
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
                        self.items
                            .push(ChatItem::Note(format!("! {message}"), Color32::YELLOW));
                    }
                    AgentEvent::VerificationStarted { round } => self.items.push(ChatItem::Note(
                        format!("» verificando o resultado (rodada {round})"),
                        Color32::LIGHT_BLUE,
                    )),
                    AgentEvent::VerificationResult { passed, notes } => {
                        self.items.push(ChatItem::Note(
                            if passed {
                                "» verificação: PASS".into()
                            } else {
                                format!("» verificação: FAIL — {notes}")
                            },
                            if passed {
                                Color32::LIGHT_GREEN
                            } else {
                                Color32::LIGHT_RED
                            },
                        ))
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
        let cancel = CancellationToken::new();
        self.running = Some(cancel.clone());
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

    /// Save the settings, then rebuild the runtime with the new configuration.
    fn apply_settings(&mut self, ctx: &egui::Context) {
        let result = (|| -> anyhow::Result<Config> {
            let path = self.settings.draft.save()?;
            self.settings.secrets.save_to(&self.settings.secrets_path)?;
            let (cfg, _) = Config::load_with_secrets(Some(&path))?;
            Ok(cfg)
        })();
        let cfg = match result {
            Ok(c) => c,
            Err(e) => {
                self.items.push(ChatItem::Note(
                    format!("erro ao salvar: {e:#}"),
                    Color32::LIGHT_RED,
                ));
                return;
            }
        };
        self.tokio.block_on(self.rt.shutdown());
        match self.tokio.block_on(Runtime::init(cfg)) {
            Ok(rt) => {
                self.rt = Arc::new(rt);
                if let Some(d) = self.rt.cfg.default_model.clone() {
                    self.model = d;
                }
                self.settings.reset(&self.rt.cfg);
                self.items.push(ChatItem::Note(
                    "Configurações salvas.".into(),
                    Color32::LIGHT_GREEN,
                ));
                self.refresh_availability(ctx);
                self.refresh_lists();
            }
            Err(e) => self.items.push(ChatItem::Note(
                format!("erro ao aplicar: {e:#}"),
                Color32::LIGHT_RED,
            )),
        }
    }

    fn approval_window(&mut self, ctx: &egui::Context) {
        let Some((req, _)) = self.approvals.first() else {
            return;
        };
        let req = req.clone();
        let mut decision = None;
        egui::Window::new(RichText::new("Aprovação necessária").strong())
            .collapsible(false)
            .resizable(true)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                let color = match req.risk {
                    Risk::Admin | Risk::Destructive => Color32::LIGHT_RED,
                    Risk::Write | Risk::Execute => Color32::LIGHT_YELLOW,
                    _ => Color32::LIGHT_BLUE,
                };
                ui.label(
                    RichText::new(format!("{} — ação de {}", req.tool, req.risk.label()))
                        .color(color)
                        .strong(),
                );
                ui.label(format!("Motivo: {}", req.reason));
                ui.separator();
                ui.label(RichText::new(&req.summary).monospace());
                egui::CollapsingHeader::new("Argumentos").show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(200.0)
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
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("✔ Permitir uma vez").clicked() {
                        decision = Some(ApprovalDecision::AllowOnce);
                    }
                    if req.allow_session && ui.button("Permitir nesta sessão").clicked() {
                        decision = Some(ApprovalDecision::AllowSession);
                    }
                    if ui
                        .button(RichText::new("✖ Negar").color(Color32::LIGHT_RED))
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
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(m) = self.rx.try_recv() {
            self.handle(m);
        }

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Claudio");
                ui.separator();
                ui.label("Modelo:");
                let current = self.model.clone();
                egui::ComboBox::from_id_salt("model")
                    .selected_text(&current)
                    .show_ui(ui, |ui| {
                        for m in self.rt.cfg.models.clone() {
                            let avail = self
                                .availability
                                .iter()
                                .find(|(n, _)| n == &m.name)
                                .map(|(_, a)| a.clone());
                            let (mark, tip) = match &avail {
                                Some(Availability::Ready) => ("●", "pronto".to_string()),
                                Some(Availability::Startable) => {
                                    ("◐", "inicia sob demanda".to_string())
                                }
                                Some(Availability::Unavailable(w)) => ("○", w.clone()),
                                None => ("?", "verificando".into()),
                            };
                            let label = format!(
                                "{mark} {} ({}{})",
                                m.name,
                                m.model,
                                if m.is_local() { ", local" } else { ", remoto" }
                            );
                            ui.selectable_value(&mut self.model, m.name.clone(), label)
                                .on_hover_text(tip);
                        }
                    });
                if ui
                    .button("⟳")
                    .on_hover_text("verificar disponibilidade")
                    .clicked()
                {
                    self.refresh_availability(ctx);
                }
                ui.separator();
                if ui.button("⚙ Configurações").clicked() {
                    self.settings.reset(&self.rt.cfg);
                    self.settings.open = true;
                }
                if ui.button("➕ Nova conversa").clicked() && self.running.is_none() {
                    self.session_id = None;
                    self.items.clear();
                    self.activity.clear();
                    self.plan = Plan::default();
                }
                ui.separator();
                ui.label(
                    RichText::new(format!("modo: {:?}", self.rt.cfg.permissions.mode)).small(),
                );
                ui.label(
                    RichText::new(format!("workspace: {}", self.rt.guard.cwd().display())).small(),
                );
            });
        });

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if self.running.is_some() {
                    ui.spinner();
                }
                ui.label(&self.status);
                if let Some(id) = &self.session_id {
                    ui.label(
                        RichText::new(format!("sessão {}", &id[..8.min(id.len())]))
                            .small()
                            .weak(),
                    );
                }
            });
        });

        egui::SidePanel::left("sessions")
            .default_width(220.0)
            .show(ctx, |ui| {
                ui.heading("Histórico");
                ui.text_edit_singleline(&mut self.search)
                    .on_hover_text("filtrar");
                ui.separator();
                let mut open = None;
                let mut resume = None;
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let q = self.search.to_lowercase();
                    for s in self
                        .sessions
                        .iter()
                        .filter(|s| q.is_empty() || s.title.to_lowercase().contains(&q))
                    {
                        let selected = self.session_id.as_deref() == Some(&s.id);
                        let icon = match s.status {
                            SessionStatus::Completed => "✔",
                            SessionStatus::CompletedUnverified => "✔?",
                            SessionStatus::Running => "▶",
                            SessionStatus::Failed => "✖",
                            _ => "⏸",
                        };
                        let r = ui.selectable_label(
                            selected,
                            format!("{icon} {}", s.title.chars().take(40).collect::<String>()),
                        );
                        if r.clicked() {
                            open = Some(s.id.clone());
                        }
                        r.context_menu(|ui| {
                            if s.status.resumable() && ui.button("Retomar").clicked() {
                                resume = Some(s.id.clone());
                                ui.close_menu();
                            }
                        });
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
            });

        egui::SidePanel::right("details")
            .default_width(320.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.tab, RightTab::Plan, "Plano");
                    ui.selectable_value(&mut self.tab, RightTab::Activity, "Atividade");
                    ui.selectable_value(&mut self.tab, RightTab::Memory, "Memória");
                });
                ui.separator();
                match self.tab {
                    RightTab::Plan => {
                        let (done, total) = self.plan.progress();
                        if total == 0 {
                            ui.label(RichText::new("Sem plano ainda.").weak());
                        } else {
                            ui.add(
                                egui::ProgressBar::new(done as f32 / total as f32)
                                    .text(format!("{done}/{total}")),
                            );
                            for s in &self.plan.steps {
                                let (icon, color) = match s.status {
                                    StepStatus::Completed => ("✔", Color32::LIGHT_GREEN),
                                    StepStatus::InProgress => ("▶", Color32::LIGHT_BLUE),
                                    StepStatus::Failed => ("✖", Color32::LIGHT_RED),
                                    StepStatus::Skipped => ("–", Color32::GRAY),
                                    StepStatus::Pending => ("○", Color32::GRAY),
                                };
                                ui.label(RichText::new(format!("{icon} {}", s.title)).color(color));
                                if let Some(n) = &s.note {
                                    ui.label(RichText::new(n).small().weak());
                                }
                            }
                        }
                    }
                    RightTab::Activity => {
                        egui::ScrollArea::vertical()
                            .stick_to_bottom(true)
                            .show(ui, |ui| {
                                for l in &self.activity {
                                    ui.label(RichText::new(l).monospace().small());
                                }
                            });
                    }
                    RightTab::Memory => {
                        ui.horizontal(|ui| {
                            ui.text_edit_singleline(&mut self.new_memory);
                            if ui.button("Salvar").clicked() && !self.new_memory.trim().is_empty() {
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
                                ui.horizontal(|ui| {
                                    if ui.small_button("🗑").on_hover_text("apagar").clicked() {
                                        delete = Some(m.id);
                                    }
                                    ui.label(format!("#{} [{}] {}", m.id, m.kind, m.content));
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

        egui::TopBottomPanel::bottom("input")
            .min_height(90.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let edit = egui::TextEdit::multiline(&mut self.input)
                        .hint_text("Peça algo ao Claudio… (Ctrl+Enter envia)")
                        .desired_rows(3)
                        .desired_width(ui.available_width() - 110.0);
                    let r = ui.add(edit);
                    let send_key = r.has_focus()
                        && ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.ctrl);
                    ui.vertical(|ui| {
                        if self.running.is_some() {
                            if ui
                                .add_sized([100.0, 30.0], egui::Button::new("⏹ Cancelar"))
                                .clicked()
                            {
                                if let Some(c) = &self.running {
                                    c.cancel();
                                }
                            }
                        } else if ui
                            .add_sized([100.0, 30.0], egui::Button::new("Enviar ▶"))
                            .clicked()
                            || send_key
                        {
                            self.send(ctx);
                        }
                    });
                });
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                if self.items.is_empty() {
                    ui.add_space(40.0);
                    ui.vertical_centered(|ui| {
                        ui.heading("Olá! Como posso ajudar?");
                        ui.label(RichText::new("Arquivos, terminal, web, documentos, mídia e automações — com sua aprovação.").weak());
                    });
                }
                for item in &self.items {
                    match item {
                        ChatItem::User(t) => {
                            ui.add_space(6.0);
                            egui::Frame::new().fill(Color32::from_rgb(38, 52, 74)).corner_radius(6.0).inner_margin(8.0).show(ui, |ui| {
                                ui.label(RichText::new("Você").strong());
                                ui.label(t);
                            });
                        }
                        ChatItem::Assistant(t) => {
                            ui.add_space(6.0);
                            egui::Frame::new().fill(Color32::from_rgb(36, 36, 40)).corner_radius(6.0).inner_margin(8.0).show(ui, |ui| {
                                ui.label(RichText::new("Claudio").strong().color(Color32::LIGHT_GREEN));
                                ui.label(t);
                            });
                        }
                        ChatItem::Tool { name, summary, ok, preview } => {
                            let (icon, color) = match ok {
                                None => ("…", Color32::GRAY),
                                Some(true) => ("✔", Color32::LIGHT_GREEN),
                                Some(false) => ("✖", Color32::LIGHT_RED),
                            };
                            egui::CollapsingHeader::new(RichText::new(format!("{icon} {name}: {}", summary.chars().take(100).collect::<String>())).color(color).small())
                                .id_salt(format!("{name}{summary}{}", preview.len()))
                                .show(ui, |ui| {
                                    ui.label(RichText::new(preview).monospace().small());
                                });
                        }
                        ChatItem::Note(t, c) => {
                            ui.label(RichText::new(t).color(*c).small());
                        }
                    }
                }
            });
        });

        self.approval_window(ctx);
        if let settings::Action::Save = self.settings.show(ctx, &self.tokio, self.running.is_some())
        {
            self.apply_settings(ctx);
        }
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
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([800.0, 500.0])
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
