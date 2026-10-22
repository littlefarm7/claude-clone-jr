//! First-run assistant: lets anyone get a working AI in two clicks.

use std::sync::{Arc, Mutex};

use claudio_core::config::Config;
use claudio_llm::catalog::preset;
use claudio_llm::ModelConfig;
use claudio_tools::downloads::{self, ProgressHandle};
use claudio_tools::setup::{self, Hardware, LocalPlan, CLOUD_CHOICES};
use eframe::egui::{self, RichText, Vec2};
use tokio_util::sync::CancellationToken;

use crate::theme;

#[derive(PartialEq, Clone, Copy)]
enum Step {
    Welcome,
    Local,
    Installing,
    Cloud,
}

type Shared<T> = Arc<Mutex<Option<T>>>;

pub enum Outcome {
    None,
    /// New configuration to save, plus a cloud key to store.
    Finished {
        cfg: Box<Config>,
        key: Option<(String, String)>,
        cloud: Option<Box<ModelConfig>>,
    },
    Skipped,
}

pub struct Onboarding {
    pub open: bool,
    step: Step,
    hw: Option<Hardware>,
    plan: Option<LocalPlan>,
    progress: ProgressHandle,
    phase: Arc<Mutex<String>>,
    cancel: CancellationToken,
    local_result: Shared<Result<Config, String>>,
    cloud: usize,
    key: String,
    connecting: bool,
    cloud_result: Shared<Result<ModelConfig, String>>,
    error: Option<String>,
}

impl Onboarding {
    pub fn new(open: bool) -> Self {
        Self {
            open,
            step: Step::Welcome,
            hw: None,
            plan: None,
            progress: downloads::new_progress(),
            phase: Arc::default(),
            cancel: CancellationToken::new(),
            local_result: Arc::default(),
            cloud: 0,
            key: String::new(),
            connecting: false,
            cloud_result: Arc::default(),
            error: None,
        }
    }

    pub fn start(&mut self) {
        if self.step != Step::Installing {
            *self = Self::new(true);
        }
        self.open = true;
    }

    pub fn busy(&self) -> bool {
        self.step == Step::Installing || self.connecting
    }

    fn go_local(&mut self) {
        let hw = setup::detect_hardware();
        self.plan = Some(setup::recommend_local(&hw));
        self.hw = Some(hw);
        self.error = None;
        self.step = Step::Local;
    }

    fn install(&mut self, cfg: &Config, tokio: &tokio::runtime::Runtime) {
        let Some(plan) = self.plan.clone() else {
            return;
        };
        self.progress = downloads::new_progress();
        self.cancel = CancellationToken::new();
        *self.local_result.lock().unwrap() = None;
        *self.phase.lock().unwrap() = "Começando…".into();
        self.error = None;
        self.step = Step::Installing;
        let (progress, cancel, phase, out) = (
            self.progress.clone(),
            self.cancel.clone(),
            self.phase.clone(),
            self.local_result.clone(),
        );
        let mut cfg = cfg.clone();
        tokio.spawn(async move {
            let r = setup::setup_local(&mut cfg, &plan, &progress, &cancel, |s| {
                *phase.lock().unwrap() = s.to_string();
            })
            .await;
            *out.lock().unwrap() = Some(r.map(|_| cfg).map_err(|e| format!("{e:#}")));
        });
    }

    fn connect(&mut self, tokio: &tokio::runtime::Runtime) {
        let choice = CLOUD_CHOICES[self.cloud];
        let key = self.key.trim().to_string();
        if key.is_empty() {
            self.error = Some("Cole a chave de acesso no campo.".into());
            return;
        }
        self.connecting = true;
        self.error = None;
        let out = self.cloud_result.clone();
        *out.lock().unwrap() = None;
        tokio.spawn(async move {
            let r = setup::connect_cloud(choice.preset, &key).await;
            *out.lock().unwrap() = Some(r.map_err(|e| format!("{e:#}")));
        });
    }

    /// Draw the assistant. `cfg` is the live configuration.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        cfg: &Config,
        tokio: &tokio::runtime::Runtime,
    ) -> Outcome {
        if !self.open {
            return Outcome::None;
        }
        let mut outcome = Outcome::None;

        // Background results.
        if let Some(r) = self.local_result.lock().unwrap().take() {
            match r {
                Ok(new_cfg) => {
                    outcome = Outcome::Finished {
                        cfg: Box::new(new_cfg),
                        key: None,
                        cloud: None,
                    };
                }
                Err(e) => {
                    self.error = Some(setup::friendly_error(&e));
                    self.step = Step::Local;
                }
            }
        }
        if let Some(r) = self.cloud_result.lock().unwrap().take() {
            self.connecting = false;
            match r {
                Ok(entry) => {
                    let mut new_cfg = cfg.clone();
                    let name = entry.name.clone();
                    let mut stored = entry.clone();
                    stored.api_key = None;
                    match new_cfg.models.iter_mut().find(|m| m.name == name) {
                        Some(m) => *m = stored,
                        None => new_cfg.models.push(stored),
                    }
                    new_cfg.default_model = Some(name.clone());
                    new_cfg.privacy.local_only = false;
                    new_cfg.privacy.prefer_local = false;
                    let key = entry.api_key.clone().map(|k| (name, k));
                    outcome = Outcome::Finished {
                        cfg: Box::new(new_cfg),
                        key,
                        cloud: Some(Box::new(entry)),
                    };
                    self.key.clear();
                }
                Err(e) => self.error = Some(e),
            }
        }
        if self.busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }

        let p = theme::current(ctx);
        let screen = ctx.screen_rect();
        let width = (screen.width() - 48.0).clamp(320.0, 640.0);
        egui::Modal::new(egui::Id::new(("onboarding", self.step as u8, self.error.is_some())))
            .frame(theme::modal_frame(ctx))
            .backdrop_color(egui::Color32::from_black_alpha(150))
            .show(ctx, |ui| {
                {
                    ui.set_width(width - 40.0);
                    match self.step {
                        Step::Welcome => {
                            ui.vertical_centered(|ui| {
                                ui.add_space(6.0);
                                ui.label(RichText::new("✨").size(40.0));
                                ui.label(RichText::new("Bem-vindo ao Claudio").size(26.0).strong());
                                ui.label(
                                    RichText::new("Seu assistente que faz tarefas no computador por você.\nPara começar, escolha onde a inteligência artificial vai funcionar:")
                                        .color(p.muted),
                                );
                            });
                            ui.add_space(14.0);
                            if theme::choice_card(ui, width - 40.0, "💻", "No meu computador", "Grátis e privado: nada sai do seu computador. Nós cuidamos de tudo; só precisa de internet para baixar uma vez (2 a 5 GB).", false).clicked() {
                                self.go_local();
                            }
                            ui.add_space(4.0);
                            if theme::choice_card(ui, width - 40.0, "☁", "Na nuvem", "Mais rápido e mais inteligente, mesmo em computadores simples. Usa uma conta da Anthropic, OpenAI ou Google.", false).clicked() {
                                self.step = Step::Cloud;
                                self.error = None;
                            }
                            ui.add_space(10.0);
                            ui.vertical_centered(|ui| {
                                if ui.link(RichText::new("Configurar depois").color(p.muted)).clicked() {
                                    outcome = Outcome::Skipped;
                                }
                            });
                        }
                        Step::Local => {
                            back(ui, &mut self.step);
                            ui.label(RichText::new("💻  IA no seu computador").size(22.0).strong());
                            if let (Some(hw), Some(plan)) = (&self.hw, &self.plan) {
                                ui.add_space(4.0);
                                theme::card(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.label(RichText::new("O que encontramos:").strong());
                                    ui.label(format!("• Memória: {:.0} GB", hw.ram_mb as f64 / 1024.0));
                                    ui.label(match &hw.gpu {
                                        Some(g) if plan.gpu_layers > 0 => format!("• Placa de vídeo: {} — vamos usá-la para ir mais rápido", g.name),
                                        Some(g) => format!("• Placa de vídeo: {} — vamos usar o processador", g.name),
                                        None => "• Vamos usar o processador".to_string(),
                                    });
                                    if let Some(free) = hw.free_disk_mb {
                                        ui.label(format!("• Espaço livre: {:.0} GB", free as f64 / 1024.0));
                                    }
                                });
                                ui.add_space(6.0);
                                ui.label(&plan.explanation);
                                ui.label(RichText::new("Você não precisa fazer mais nada: o download é feito uma vez e continua de onde parou se a internet cair.").color(p.muted));
                            }
                            show_error(ui, &self.error, p.danger);
                            ui.add_space(8.0);
                            ui.horizontal(|ui| {
                                let label = if self.error.is_some() {
                                    "  Tentar de novo  "
                                } else {
                                    "  Instalar e começar  "
                                };
                                if theme::primary_button(ui, label).clicked() {
                                    self.install(cfg, tokio);
                                }
                            });
                        }
                        Step::Installing => {
                            ui.label(RichText::new("Preparando tudo para você…").size(22.0).strong());
                            ui.add_space(4.0);
                            let phase = self.phase.lock().unwrap().clone();
                            ui.label(RichText::new(phase).color(p.muted));
                            let pr = self.progress.lock().unwrap().clone();
                            let text = match pr.total {
                                Some(t) if t > 0 => format!(
                                    "{:.0}%  ·  {:.1} de {:.1} GB",
                                    pr.fraction().unwrap_or(0.0) * 100.0,
                                    pr.downloaded as f64 / 1_073_741_824.0,
                                    t as f64 / 1_073_741_824.0
                                ),
                                _ => "aguarde…".into(),
                            };
                            ui.add(
                                egui::ProgressBar::new(pr.fraction().unwrap_or(0.0))
                                    .desired_height(14.0)
                                    .corner_radius(7)
                                    .fill(p.accent)
                                    .animate(pr.total.is_none()),
                            );
                            ui.label(RichText::new(text).small().color(p.muted));
                            ui.add_space(6.0);
                            ui.label(RichText::new("Pode continuar usando o computador. Se fechar o programa, o download continua depois de onde parou.").small().color(p.muted));
                            ui.add_space(6.0);
                            if theme::pill_button(ui, "Cancelar").clicked() {
                                self.cancel.cancel();
                            }
                        }
                        Step::Cloud => {
                            back(ui, &mut self.step);
                            ui.label(RichText::new("☁  IA na nuvem").size(22.0).strong());
                            ui.label(RichText::new("Escolha o serviço:").color(p.muted));
                            ui.add_space(4.0);
                            let half = (width - 40.0 - 10.0) / 2.0;
                            for row in CLOUD_CHOICES.chunks(2) {
                                ui.horizontal(|ui| {
                                    for c in row {
                                        let i = CLOUD_CHOICES.iter().position(|x| x.preset == c.preset).unwrap();
                                        if theme::choice_card(ui, half, "", c.title, c.subtitle, i == self.cloud).clicked() {
                                            self.cloud = i;
                                            self.error = None;
                                        }
                                    }
                                });
                            }
                            let c = CLOUD_CHOICES[self.cloud];
                            ui.add_space(6.0);
                            theme::card(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.label(RichText::new(format!("Como conseguir a chave do {}:", c.title)).strong());
                                for (i, s) in c.steps.iter().enumerate() {
                                    ui.label(format!("{}. {s}", i + 1));
                                }
                                if let Some(pr) = preset(c.preset) {
                                    if theme::pill_button(ui, "🌐  Abrir o site").clicked() {
                                        ui.ctx().open_url(egui::OpenUrl::new_tab(pr.key_url));
                                    }
                                }
                            });
                            ui.add_space(6.0);
                            ui.label(RichText::new("Chave de acesso").strong());
                            ui.add(
                                egui::TextEdit::singleline(&mut self.key)
                                    .password(true)
                                    .hint_text("cole aqui (Ctrl+V)")
                                    .desired_width(f32::INFINITY)
                                    .margin(Vec2::new(12.0, 10.0)),
                            );
                            ui.label(
                                RichText::new(format!(
                                    "Ao conectar, você autoriza o envio das suas conversas para o {}. A chave fica guardada só neste computador.",
                                    c.title
                                ))
                                .small()
                                .color(p.muted),
                            );
                            show_error(ui, &self.error, p.danger);
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                ui.add_enabled_ui(!self.connecting, |ui| {
                                    if theme::primary_button(ui, "  Conectar  ").clicked() {
                                        self.connect(tokio);
                                    }
                                });
                                if self.connecting {
                                    ui.spinner();
                                    ui.label("Testando a conexão…");
                                }
                            });
                        }
                    }
                }
            });
        if !matches!(outcome, Outcome::None) {
            self.open = false;
            self.step = Step::Welcome;
        }
        outcome
    }
}

fn back(ui: &mut egui::Ui, step: &mut Step) {
    if ui.link("⬅ Voltar").clicked() {
        *step = Step::Welcome;
    }
}

fn show_error(ui: &mut egui::Ui, err: &Option<String>, color: egui::Color32) {
    if let Some(e) = err {
        ui.add_space(4.0);
        ui.add(egui::Label::new(RichText::new(format!("⚠ {e}")).color(color)).wrap());
    }
}
