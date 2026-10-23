//! Voice conversation in the desktop app: microphone button, conversation
//! mode (listen → answer aloud → listen again) and one-click voice setup.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use claudio_core::config::Config;
use claudio_tools::downloads::{self, ProgressHandle};
use claudio_tools::speech::{self, ListenState};
use eframe::egui::{self, RichText};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VoiceState {
    Idle,
    Waiting,
    Hearing,
    Transcribing,
    Speaking,
}

pub enum VoiceEvent {
    /// What the user said (send it as a message).
    Heard(String),
    /// Nobody spoke: conversation mode pauses.
    Silence,
    Error(String),
    /// Voice was installed/configured: save this configuration.
    Configured(Box<Config>),
}

enum Msg {
    State(VoiceState),
    Heard(String),
    Silence,
    Error(String),
    SpeakDone,
    SetupDone(Result<Box<Config>, String>),
}

pub struct VoiceUi {
    pub state: VoiceState,
    /// Conversation mode: answers are spoken and the microphone reopens.
    pub conversation: bool,
    cancel: Option<CancellationToken>,
    tx: mpsc::UnboundedSender<Msg>,
    rx: mpsc::UnboundedReceiver<Msg>,
    setup_open: bool,
    setup_running: bool,
    setup_error: Option<String>,
    progress: ProgressHandle,
    phase: Arc<Mutex<String>>,
    setup_cancel: CancellationToken,
    /// Start listening as soon as voice is ready (after setup).
    listen_after_setup: bool,
}

impl Default for VoiceUi {
    fn default() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            state: VoiceState::Idle,
            conversation: false,
            cancel: None,
            tx,
            rx,
            setup_open: false,
            setup_running: false,
            setup_error: None,
            progress: downloads::new_progress(),
            phase: Arc::default(),
            setup_cancel: CancellationToken::new(),
            listen_after_setup: false,
        }
    }
}

impl VoiceUi {
    pub fn busy(&self) -> bool {
        self.state != VoiceState::Idle
    }

    /// Stop listening or speaking.
    pub fn stop(&mut self) {
        if let Some(c) = self.cancel.take() {
            c.cancel();
        }
        self.state = VoiceState::Idle;
    }

    /// Microphone button / conversation start. Opens the setup screen if needed.
    pub fn listen(
        &mut self,
        cfg: &Arc<Config>,
        tokio: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        if !speech::status(cfg).can_listen() {
            self.setup_open = true;
            self.setup_error = None;
            self.listen_after_setup = true;
            return;
        }
        self.stop();
        let cancel = CancellationToken::new();
        self.cancel = Some(cancel.clone());
        self.state = VoiceState::Waiting;
        let tx = self.tx.clone();
        let cfg = cfg.clone();
        let ctx = ctx.clone();
        tokio.spawn(async move {
            let tx2 = tx.clone();
            let ctx2 = ctx.clone();
            let r = speech::listen_utterance(&cancel, Duration::from_secs(12), move |s| {
                let _ = tx2.send(Msg::State(match s {
                    ListenState::Waiting => VoiceState::Waiting,
                    ListenState::Hearing => VoiceState::Hearing,
                }));
                ctx2.request_repaint();
            })
            .await;
            let msg = match r {
                Ok(Some(samples)) if !cancel.is_cancelled() => {
                    let _ = tx.send(Msg::State(VoiceState::Transcribing));
                    ctx.request_repaint();
                    match speech::transcribe(&cfg, samples).await {
                        Ok(t) if t.trim().is_empty() => Msg::Silence,
                        Ok(t) => Msg::Heard(t),
                        Err(e) => Msg::Error(format!("{e:#}")),
                    }
                }
                Ok(_) if cancel.is_cancelled() => Msg::State(VoiceState::Idle),
                Ok(_) => Msg::Silence,
                Err(e) => Msg::Error(format!("{e:#}")),
            };
            let _ = tx.send(msg);
            ctx.request_repaint();
        });
    }

    /// Read an answer aloud (conversation mode reopens the microphone after).
    pub fn speak(
        &mut self,
        cfg: &Arc<Config>,
        text: String,
        tokio: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        self.stop();
        let cancel = CancellationToken::new();
        self.cancel = Some(cancel.clone());
        self.state = VoiceState::Speaking;
        let tx = self.tx.clone();
        let cfg = cfg.clone();
        let ctx = ctx.clone();
        tokio.spawn(async move {
            let r = speech::speak(&cfg, &text, &cancel).await;
            let _ = tx.send(match r {
                Ok(()) => Msg::SpeakDone,
                Err(e) => Msg::Error(format!("não consegui falar a resposta: {e:#}")),
            });
            ctx.request_repaint();
        });
    }

    /// Handle background results. Returns events for the app.
    pub fn poll(
        &mut self,
        cfg: &Arc<Config>,
        tokio: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) -> Vec<VoiceEvent> {
        let mut out = Vec::new();
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::State(s) => {
                    if self.cancel.is_some() || s == VoiceState::Idle {
                        self.state = s;
                    }
                }
                Msg::Heard(t) => {
                    self.state = VoiceState::Idle;
                    self.cancel = None;
                    out.push(VoiceEvent::Heard(t));
                }
                Msg::Silence => {
                    self.state = VoiceState::Idle;
                    self.cancel = None;
                    out.push(VoiceEvent::Silence);
                }
                Msg::Error(e) => {
                    self.state = VoiceState::Idle;
                    self.cancel = None;
                    self.conversation = false;
                    out.push(VoiceEvent::Error(e));
                }
                Msg::SpeakDone => {
                    self.state = VoiceState::Idle;
                    self.cancel = None;
                    if self.conversation {
                        self.listen(cfg, tokio, ctx);
                    }
                }
                Msg::SetupDone(r) => {
                    self.setup_running = false;
                    match r {
                        Ok(c) => {
                            self.setup_open = false;
                            out.push(VoiceEvent::Configured(c));
                        }
                        Err(e) => self.setup_error = Some(claudio_tools::setup::friendly_error(&e)),
                    }
                }
            }
        }
        if self.busy() || self.setup_running {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
        out
    }

    /// After the new configuration is applied: start listening if the user was waiting.
    pub fn after_configured(
        &mut self,
        cfg: &Arc<Config>,
        tokio: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        if std::mem::take(&mut self.listen_after_setup) {
            self.listen(cfg, tokio, ctx);
        }
    }

    pub fn open_setup(&mut self) {
        self.setup_open = true;
        self.setup_error = None;
    }

    fn start_setup(&mut self, cfg: &Config, tokio: &tokio::runtime::Runtime, ctx: &egui::Context) {
        self.setup_running = true;
        self.setup_error = None;
        self.progress = downloads::new_progress();
        self.setup_cancel = CancellationToken::new();
        *self.phase.lock().unwrap() = "Começando…".into();
        let (progress, cancel, phase, tx, ctx) = (
            self.progress.clone(),
            self.setup_cancel.clone(),
            self.phase.clone(),
            self.tx.clone(),
            ctx.clone(),
        );
        let mut cfg = cfg.clone();
        tokio.spawn(async move {
            let r = speech::setup_voice(&mut cfg, &progress, &cancel, |p| {
                *phase.lock().unwrap() = p.to_string();
            })
            .await;
            let _ = tx.send(Msg::SetupDone(
                r.map(|_| Box::new(cfg)).map_err(|e| format!("{e:#}")),
            ));
            ctx.request_repaint();
        });
    }

    /// The voice setup screen.
    pub fn setup_window(
        &mut self,
        ctx: &egui::Context,
        cfg: &Config,
        tokio: &tokio::runtime::Runtime,
    ) {
        if !self.setup_open {
            return;
        }
        let p = theme::current(ctx);
        let cloud = speech::cloud_stt(cfg).map(|(_, n)| n);
        let cpu_ok = speech::cpu_supports_local_stt();
        let mut close = false;
        let mut use_cloud = false;
        let mut start = false;
        egui::Modal::new(egui::Id::new(("voice_setup", self.setup_running)))
            .frame(theme::modal_frame(ctx))
            .backdrop_color(egui::Color32::from_black_alpha(150))
            .show(ctx, |ui| {
                ui.set_width(500.0_f32.min(ctx.screen_rect().width() - 80.0));
                ui.horizontal(|ui| {
                    ui.label(RichText::new("🎤").size(28.0));
                    ui.label(RichText::new("Conversar por voz").size(21.0).strong());
                });
                if self.setup_running {
                    ui.label(RichText::new(self.phase.lock().unwrap().clone()).color(p.muted));
                    let pr = self.progress.lock().unwrap().clone();
                    ui.add(
                        egui::ProgressBar::new(pr.fraction().unwrap_or(0.0))
                            .desired_height(12.0)
                            .corner_radius(6)
                            .fill(p.accent)
                            .animate(pr.total.is_none()),
                    );
                    if let Some(t) = pr.total {
                        ui.label(
                            RichText::new(format!(
                                "{:.0} de {:.0} MB",
                                pr.downloaded as f64 / 1_048_576.0,
                                t as f64 / 1_048_576.0
                            ))
                            .small()
                            .color(p.muted),
                        );
                    }
                    if theme::pill_button(ui, "Cancelar").clicked() {
                        self.setup_cancel.cancel();
                    }
                    return;
                }
                ui.add(
                    egui::Label::new(
                        "Fale com o Claudio e ouça as respostas. Para entender sua voz no próprio computador, \
                         preciso baixar o reconhecimento de voz e uma voz em português (cerca de 250 MB, só uma vez).",
                    )
                    .wrap(),
                );
                ui.label(
                    RichText::new("Nada do que você fala sai do computador.")
                        .small()
                        .color(p.muted),
                );
                if !cpu_ok {
                    ui.label(
                        RichText::new("Este processador é antigo para o reconhecimento no computador; use a opção na nuvem.")
                            .color(p.warning),
                    );
                }
                if let Some(e) = &self.setup_error {
                    ui.add(egui::Label::new(RichText::new(format!("⚠ {e}")).color(p.danger)).wrap());
                }
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    if cpu_ok && theme::primary_button(ui, "  Ativar voz no computador  ").clicked() {
                        start = true;
                    }
                    if let Some(name) = cloud {
                        if theme::pill_button(ui, format!("Usar minha conta {name}")).clicked() {
                            use_cloud = true;
                        }
                    }
                    if theme::pill_button(ui, "Agora não").clicked() {
                        close = true;
                    }
                });
                if let Some(name) = cloud {
                    ui.label(
                        RichText::new(format!(
                            "Na opção da nuvem, o áudio do que você fala é enviado para a {name} para virar texto."
                        ))
                        .small()
                        .color(p.muted),
                    );
                }
            });
        if start {
            self.start_setup(cfg, tokio, ctx);
        }
        if use_cloud {
            let mut c = cfg.clone();
            c.voice.stt = "cloud".into();
            let _ = self.tx.send(Msg::SetupDone(Ok(Box::new(c))));
        }
        if close {
            self.setup_open = false;
            self.listen_after_setup = false;
        }
    }
}
