//! "Configurações" window: API models, local model downloads and general options.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use claudio_core::config::Config;
use claudio_core::permissions::Mode;
use claudio_core::secrets::{mask, secrets_path_for, Secrets};
use claudio_llm::catalog::{api_model_config, ApiPreset, API_PRESETS, LOCAL_MODELS};
use claudio_llm::{ChatRequest, Message};
use claudio_tools::downloads::{self, LlamaVariant, ProgressHandle};
use eframe::egui::{self, Color32, RichText};
use tokio_util::sync::CancellationToken;

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Basic,
    Api,
    Local,
    General,
}

struct Job {
    progress: ProgressHandle,
    cancel: CancellationToken,
}

pub enum Action {
    None,
    Save,
    /// Open the setup assistant.
    Setup,
}

pub struct Settings {
    pub open: bool,
    tab: Tab,
    pub draft: Config,
    pub secrets: Secrets,
    pub secrets_path: PathBuf,
    preset: usize,
    api_name: String,
    api_model: String,
    api_base: String,
    api_key: String,
    api_default: bool,
    test_result: Arc<Mutex<Option<(bool, String)>>>,
    testing: bool,
    jobs: HashMap<String, Job>,
    variant: LlamaVariant,
    url_input: String,
    file_input: String,
    workspace: String,
    llama_path: String,
    message: Option<(String, Color32)>,
    pub dirty: bool,
    mem_total_mb: u64,
    advanced: bool,
}

impl Settings {
    pub fn new(cfg: &Config) -> Self {
        let mut s = Self {
            open: false,
            tab: Tab::Basic,
            draft: cfg.clone(),
            secrets: Secrets::default(),
            secrets_path: secrets_path_for(cfg),
            preset: 0,
            api_name: String::new(),
            api_model: String::new(),
            api_base: String::new(),
            api_key: String::new(),
            api_default: true,
            test_result: Arc::default(),
            testing: false,
            jobs: HashMap::new(),
            variant: LlamaVariant::Cpu,
            url_input: String::new(),
            file_input: String::new(),
            workspace: String::new(),
            llama_path: String::new(),
            message: None,
            dirty: false,
            mem_total_mb: claudio_llm::router::detect_resources().mem_total_mb,
            advanced: false,
        };
        s.reset(cfg);
        s
    }

    /// Reload the working copy from the live configuration.
    pub fn reset(&mut self, cfg: &Config) {
        self.draft = cfg.clone();
        self.secrets_path = secrets_path_for(cfg);
        self.secrets = Secrets::load_from(&self.secrets_path).unwrap_or_default();
        self.workspace = cfg
            .permissions
            .workspace_roots
            .first()
            .cloned()
            .unwrap_or_default();
        self.llama_path = cfg.local.llama_server.clone().unwrap_or_default();
        self.select_preset(self.preset);
        self.dirty = false;
        self.message = None;
    }

    fn select_preset(&mut self, i: usize) {
        self.preset = i;
        let p = &API_PRESETS[i];
        self.api_name = if p.id == "anthropic" {
            "claude".into()
        } else {
            p.id.into()
        };
        self.api_model = p.models.first().map(|s| s.to_string()).unwrap_or_default();
        self.api_base = p.base_url.unwrap_or("").to_string();
        self.api_key.clear();
        *self.test_result.lock().unwrap() = None;
    }

    fn preset(&self) -> &'static ApiPreset {
        &API_PRESETS[self.preset]
    }

    pub fn saved_message(&mut self) {
        self.info("✔ Configurações salvas.");
    }

    fn info(&mut self, text: impl Into<String>) {
        self.message = Some((text.into(), Color32::LIGHT_GREEN));
    }

    fn error(&mut self, text: impl Into<String>) {
        let t: String = text.into();
        let t = if t.chars().count() > 400 {
            format!("{}…", t.chars().take(400).collect::<String>())
        } else {
            t
        };
        self.message = Some((t, Color32::LIGHT_RED));
    }

    /// Draw the window. Returns `Action::Save` when the user saves.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        tokio: &tokio::runtime::Runtime,
        busy: bool,
    ) -> Action {
        if !self.open {
            return Action::None;
        }
        self.poll_jobs();
        if self.testing && self.test_result.lock().unwrap().is_some() {
            self.testing = false;
        }
        if !self.jobs.is_empty() || self.testing {
            ctx.request_repaint_after(std::time::Duration::from_millis(300));
        }
        let mut action = Action::None;
        let mut open = self.open;
        let p = crate::theme::current(ctx);
        let screen = ctx.screen_rect();
        egui::Window::new(RichText::new("Configurações").size(20.0).strong())
            .open(&mut open)
            .default_size([620.0, 560.0])
            .max_width((screen.width() - 40.0).min(900.0))
            .max_height(screen.height() - 40.0)
            .resizable(true)
            .collapsible(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                if self.advanced {
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.tab, Tab::Basic, "Básico");
                        ui.selectable_value(&mut self.tab, Tab::Api, "IAs na nuvem (API)");
                        ui.selectable_value(&mut self.tab, Tab::Local, "IAs no computador");
                        ui.selectable_value(&mut self.tab, Tab::General, "Avançado");
                    });
                    ui.add_space(4.0);
                } else {
                    self.tab = Tab::Basic;
                }
                egui::ScrollArea::vertical()
                    .max_height(ui.available_height() - 60.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| match self.tab {
                        Tab::Basic => {
                            if self.basic_tab(ui) {
                                action = Action::Setup;
                            }
                        }
                        Tab::Api => self.api_tab(ui, tokio),
                        Tab::Local => self.local_tab(ui, tokio),
                        Tab::General => self.general_tab(ui),
                    });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(!busy && self.dirty, |ui| {
                        if crate::theme::primary_button(ui, "  Salvar  ").clicked() {
                            action = Action::Save;
                        }
                    });
                    if busy {
                        ui.label(
                            RichText::new("Espere a tarefa atual terminar para salvar.")
                                .small()
                                .color(p.muted),
                        );
                    } else if !self.dirty {
                        ui.label(RichText::new("Nenhuma alteração.").small().color(p.muted));
                    }
                });
                if let Some((m, c)) = &self.message {
                    let c = if *c == Color32::LIGHT_GREEN {
                        p.success
                    } else {
                        p.danger
                    };
                    ui.add(egui::Label::new(RichText::new(m).color(c)).wrap());
                }
            });
        self.open = open;
        action
    }

    // -------------------------------------------------------------- Basic ---

    /// Simple options for everyone. Returns true to open the setup assistant.
    fn basic_tab(&mut self, ui: &mut egui::Ui) -> bool {
        use crate::theme::{card, pill_button};
        let p = crate::theme::current(ui.ctx());
        let mut setup = false;
        let mut changed = false;

        ui.label(RichText::new("Sua IA").strong());
        card(ui, |ui| {
            ui.set_width(ui.available_width());
            let name = self.draft.default_model.clone().unwrap_or_default();
            let ready = claudio_tools::setup::ai_ready(&self.draft);
            ui.horizontal(|ui| {
                crate::status_dot(ui, if ready { p.success } else { p.warning });
                ui.label(
                    RichText::new(claudio_tools::setup::ai_label(&self.draft, &name)).strong(),
                );
            });
            ui.label(
                RichText::new(if ready {
                    "Pronta para usar."
                } else {
                    "Ainda não configurada."
                })
                .color(p.muted),
            );
            if crate::theme::primary_button(ui, "  Trocar ou configurar IA  ").clicked() {
                setup = true;
            }
        });

        ui.add_space(8.0);
        ui.label(RichText::new("Pasta onde o Claudio salva os arquivos").strong());
        ui.horizontal(|ui| {
            changed |= ui
                .add(
                    egui::TextEdit::singleline(&mut self.workspace)
                        .desired_width(ui.available_width() - 120.0),
                )
                .changed();
            if pill_button(ui, "📂 Abrir").clicked() {
                let path = claudio_core::config::expand(self.workspace.trim());
                let _ = std::fs::create_dir_all(&path);
                ui.ctx().open_url(egui::OpenUrl::new_tab(format!(
                    "file://{}",
                    path.to_string_lossy().replace('\\', "/")
                )));
            }
        });

        ui.add_space(8.0);
        ui.label(RichText::new("Quando pedir sua permissão").strong());
        let d = &mut self.draft;
        changed |= ui
            .radio_value(
                &mut d.permissions.mode,
                Mode::Ask,
                "Antes de criar, mudar ou apagar algo (recomendado)",
            )
            .changed();
        changed |= ui
            .radio_value(
                &mut d.permissions.mode,
                Mode::AutoSafe,
                "Só para ações arriscadas (pode criar e editar na pasta acima sem perguntar)",
            )
            .changed();
        changed |= ui
            .radio_value(
                &mut d.permissions.mode,
                Mode::Strict,
                "Para tudo, até para ler",
            )
            .changed();

        ui.add_space(8.0);
        ui.label(RichText::new("Privacidade").strong());
        changed |= crate::theme::toggle(
            ui,
            &mut d.privacy.local_only,
            "Nunca usar IA na nuvem (nada sai do computador)",
        )
        .changed();

        ui.add_space(8.0);
        ui.label(RichText::new("Aparência").strong());
        ui.horizontal(|ui| {
            for (v, l) in [
                ("system", "Automática"),
                ("light", "☀ Clara"),
                ("dark", "🌙 Escura"),
            ] {
                let selected = d.ui.theme == v || (v == "system" && d.ui.theme.is_empty());
                if ui.selectable_label(selected, l).clicked() && !selected {
                    d.ui.theme = v.to_string();
                    changed = true;
                }
            }
        });

        ui.add_space(12.0);
        crate::theme::toggle(ui, &mut self.advanced, "Mostrar opções avançadas")
            .on_hover_text("Para quem quer escolher modelos, chaves e limites manualmente.");

        if changed {
            let ws = self.workspace.trim().to_string();
            if !ws.is_empty() {
                if self.draft.permissions.workspace_roots.is_empty() {
                    self.draft.permissions.workspace_roots.push(ws);
                } else {
                    self.draft.permissions.workspace_roots[0] = ws;
                }
            }
            self.dirty = true;
        }
        setup
    }

    // ---------------------------------------------------------------- API ---

    fn api_tab(&mut self, ui: &mut egui::Ui, tokio: &tokio::runtime::Runtime) {
        ui.heading("Modelos configurados");
        let mut remove = None;
        let mut make_default = None;
        egui::Grid::new("models")
            .striped(true)
            .num_columns(5)
            .show(ui, |ui| {
                for m in &self.draft.models {
                    let is_default = self.draft.default_model.as_deref() == Some(&m.name);
                    ui.label(RichText::new(&m.name).strong());
                    ui.label(format!("{:?}", m.provider).to_lowercase());
                    ui.label(&m.model);
                    let key = if m.is_local() {
                        "local".to_string()
                    } else if let Some(k) = self.secrets.keys.get(&m.name) {
                        format!("chave {}", mask(k))
                    } else if m.resolve_api_key().is_some() {
                        "chave (variável de ambiente)".into()
                    } else {
                        "sem chave".into()
                    };
                    ui.label(RichText::new(key).small());
                    ui.horizontal(|ui| {
                        if is_default {
                            ui.label(RichText::new("★ padrão").color(Color32::GOLD));
                        } else if ui.small_button("Usar como padrão").clicked() {
                            make_default = Some(m.name.clone());
                        }
                        if ui.small_button("🗑").on_hover_text("remover").clicked() {
                            remove = Some(m.name.clone());
                        }
                    });
                    ui.end_row();
                }
            });
        if let Some(n) = make_default {
            self.draft.default_model = Some(n);
            self.dirty = true;
        }
        if let Some(n) = remove {
            self.draft.models.retain(|m| m.name != n);
            self.secrets.keys.remove(&n);
            if self.draft.default_model.as_deref() == Some(n.as_str()) {
                self.draft.default_model = self.draft.models.first().map(|m| m.name.clone());
            }
            self.dirty = true;
        }

        ui.add_space(12.0);
        ui.heading("Adicionar ou atualizar modelo por API");
        egui::Grid::new("api_form")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Provedor");
                let current = self.preset;
                egui::ComboBox::from_id_salt("preset")
                    .selected_text(API_PRESETS[current].label)
                    .show_ui(ui, |ui| {
                        for (i, p) in API_PRESETS.iter().enumerate() {
                            if ui.selectable_label(i == current, p.label).clicked() {
                                self.select_preset(i);
                            }
                        }
                    });
                ui.end_row();

                ui.label("Nome no Claudio");
                ui.text_edit_singleline(&mut self.api_name);
                ui.end_row();

                ui.label("Modelo");
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut self.api_model);
                    let models = self.preset().models;
                    if !models.is_empty() {
                        egui::ComboBox::from_id_salt("model_suggestions")
                            .selected_text("sugestões")
                            .show_ui(ui, |ui| {
                                for m in models {
                                    if ui.selectable_label(self.api_model == *m, *m).clicked() {
                                        self.api_model = m.to_string();
                                    }
                                }
                            });
                    }
                });
                ui.end_row();

                if self.preset().base_url.is_some() {
                    ui.label("URL base");
                    ui.text_edit_singleline(&mut self.api_base);
                    ui.end_row();
                }

                if self.preset().needs_key {
                    ui.label("Chave de API");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.api_key)
                                .password(true)
                                .hint_text("cole a chave aqui"),
                        );
                        if !self.preset().key_url.is_empty() {
                            ui.hyperlink_to("obter chave", self.preset().key_url);
                        }
                    });
                    ui.end_row();
                }
                ui.label("");
                ui.checkbox(&mut self.api_default, "Usar como modelo padrão");
                ui.end_row();
            });

        ui.horizontal(|ui| {
            if ui
                .add_enabled(!self.testing, egui::Button::new("🔌 Testar conexão"))
                .clicked()
            {
                self.test_api(tokio);
            }
            if ui.button("➕ Adicionar / atualizar").clicked() {
                self.add_api();
            }
            if self.testing {
                ui.spinner();
            }
        });
        if let Some((ok, msg)) = self.test_result.lock().unwrap().clone() {
            ui.add(
                egui::Label::new(RichText::new(msg).color(if ok {
                    Color32::LIGHT_GREEN
                } else {
                    Color32::LIGHT_RED
                }))
                .wrap(),
            );
        }
        ui.label(
            RichText::new(format!(
                "As chaves ficam em {} (permissão 0600) e nunca no config.toml. Variáveis de ambiente (ANTHROPIC_API_KEY, OPENAI_API_KEY, GEMINI_API_KEY) têm prioridade.",
                self.secrets_path.display()
            ))
            .small()
            .weak(),
        );
    }

    fn entry_from_form(&self) -> Option<claudio_llm::ModelConfig> {
        let name = self.api_name.trim();
        let model = self.api_model.trim();
        if name.is_empty() || model.is_empty() {
            return None;
        }
        let base = (!self.api_base.trim().is_empty()).then(|| self.api_base.trim());
        Some(api_model_config(self.preset(), name, model, base))
    }

    fn add_api(&mut self) {
        let Some(entry) = self.entry_from_form() else {
            self.error("preencha o nome e o modelo");
            return;
        };
        if self.preset().needs_key
            && self.api_key.trim().is_empty()
            && !self.secrets.keys.contains_key(&entry.name)
        {
            self.error("informe a chave de API");
            return;
        }
        let name = entry.name.clone();
        match self.draft.models.iter_mut().find(|m| m.name == name) {
            Some(m) => *m = entry,
            None => self.draft.models.push(entry),
        }
        if !self.api_key.trim().is_empty() {
            self.secrets.set(&name, &self.api_key);
            self.api_key.clear();
        }
        if self.api_default {
            self.draft.default_model = Some(name.clone());
        }
        self.dirty = true;
        self.info(format!("modelo '{name}' pronto; clique em Salvar"));
    }

    fn test_api(&mut self, tokio: &tokio::runtime::Runtime) {
        let Some(mut entry) = self.entry_from_form() else {
            *self.test_result.lock().unwrap() = Some((false, "preencha o nome e o modelo".into()));
            return;
        };
        let key = if self.api_key.trim().is_empty() {
            self.secrets.keys.get(&entry.name).cloned()
        } else {
            Some(self.api_key.trim().to_string())
        };
        entry.api_key = key;
        self.testing = true;
        let out = self.test_result.clone();
        *out.lock().unwrap() = None;
        tokio.spawn(async move {
            let t0 = std::time::Instant::now();
            let r = async {
                let p = claudio_llm::build_provider(&entry)?;
                p.chat(ChatRequest {
                    messages: vec![Message::user("Responda apenas: OK")],
                    tools: vec![],
                    max_tokens: 16,
                    temperature: Some(0.0),
                })
                .await
            }
            .await;
            *out.lock().unwrap() = Some(match r {
                Ok(resp) => (
                    true,
                    format!(
                        "✔ respondeu em {} ms: {}",
                        t0.elapsed().as_millis(),
                        resp.message
                            .text_content()
                            .trim()
                            .chars()
                            .take(60)
                            .collect::<String>()
                    ),
                ),
                Err(e) => (
                    false,
                    format!(
                        "✖ {}",
                        format!("{e:#}").chars().take(200).collect::<String>()
                    ),
                ),
            });
        });
    }

    // -------------------------------------------------------------- Local ---

    fn start_job(
        &mut self,
        key: &str,
        tokio: &tokio::runtime::Runtime,
        fut: impl FnOnce(
            ProgressHandle,
            CancellationToken,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = anyhow::Result<PathBuf>> + Send>,
        >,
    ) {
        let progress = downloads::new_progress();
        let cancel = CancellationToken::new();
        let task = fut(progress.clone(), cancel.clone());
        let p2 = progress.clone();
        tokio.spawn(async move {
            let r = task.await;
            let mut p = p2.lock().unwrap();
            p.done = true;
            match r {
                Ok(path) => p.result = Some(path),
                Err(e) => p.error = Some(format!("{e:#}")),
            }
        });
        self.jobs.insert(key.to_string(), Job { progress, cancel });
    }

    /// Apply finished jobs to the working configuration.
    fn poll_jobs(&mut self) {
        let finished: Vec<String> = self
            .jobs
            .iter()
            .filter(|(_, j)| j.progress.lock().unwrap().done)
            .map(|(k, _)| k.clone())
            .collect();
        for key in finished {
            let job = self.jobs.remove(&key).unwrap();
            let p = job.progress.lock().unwrap().clone();
            if let Some(e) = p.error {
                self.error(e);
                continue;
            }
            let Some(path) = p.result else { continue };
            if key == "llama" {
                self.draft.local.llama_server = Some(path.to_string_lossy().into_owned());
                self.dirty = true;
                self.info("llama.cpp instalado; clique em Salvar");
            } else if let Some(m) = claudio_llm::catalog::local_model(&key) {
                downloads::use_local_model(
                    &mut self.draft,
                    m.model_config(&path.to_string_lossy()),
                );
                self.dirty = true;
                self.info(format!(
                    "{} baixado e selecionado; clique em Salvar",
                    m.name
                ));
            } else {
                downloads::use_local_model(&mut self.draft, downloads::custom_gguf_config(&path));
                self.dirty = true;
                self.info(format!(
                    "{} baixado e selecionado; clique em Salvar",
                    path.display()
                ));
            }
        }
    }

    fn progress_row(ui: &mut egui::Ui, job: &Job) {
        let p = job.progress.lock().unwrap().clone();
        ui.horizontal(|ui| {
            let mb = p.downloaded as f32 / 1_048_576.0;
            let text = match p.total {
                Some(t) => format!("{mb:.0} / {:.0} MB", t as f32 / 1_048_576.0),
                None => format!("{mb:.0} MB"),
            };
            ui.add(
                egui::ProgressBar::new(p.fraction().unwrap_or(0.0))
                    .desired_width(220.0)
                    .text(text),
            );
            if ui.small_button("Cancelar").clicked() {
                job.cancel.cancel();
            }
        });
        if !p.stage.is_empty() {
            ui.label(RichText::new(p.stage).small().weak());
        }
    }

    fn local_tab(&mut self, ui: &mut egui::Ui, tokio: &tokio::runtime::Runtime) {
        // 1. llama.cpp
        ui.heading("1. Motor local (llama.cpp)");
        let server = claudio_llm::local::find_binary(
            self.draft
                .local
                .llama_server
                .as_deref()
                .map(claudio_core::config::expand)
                .as_deref(),
        );
        match &server {
            Some(p) => {
                ui.label(
                    RichText::new(format!("✔ llama-server: {}", p.display()))
                        .color(Color32::LIGHT_GREEN),
                );
            }
            None => {
                ui.label(
                    RichText::new(
                        "llama-server não encontrado. Instale abaixo (download oficial do GitHub).",
                    )
                    .color(Color32::YELLOW),
                );
            }
        }
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("variant")
                .selected_text(self.variant.label())
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.variant,
                        LlamaVariant::Cpu,
                        LlamaVariant::Cpu.label(),
                    );
                    if !cfg!(target_os = "macos") {
                        ui.selectable_value(
                            &mut self.variant,
                            LlamaVariant::Vulkan,
                            LlamaVariant::Vulkan.label(),
                        );
                    }
                });
            let installing = self.jobs.contains_key("llama");
            let label = if server.is_some() {
                "Reinstalar / atualizar"
            } else {
                "⬇ Instalar llama.cpp"
            };
            if ui
                .add_enabled(!installing, egui::Button::new(label))
                .clicked()
            {
                let v = self.variant;
                self.start_job("llama", tokio, move |p, c| {
                    Box::pin(async move {
                        downloads::install_llama_cpp(&downloads::llama_releases_api(), v, &p, &c)
                            .await
                    })
                });
            }
        });
        if let Some(job) = self.jobs.get("llama") {
            Self::progress_row(ui, job);
        }
        ui.horizontal(|ui| {
            ui.label("Ou indique um llama-server já instalado:");
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.llama_path)
                    .desired_width(340.0)
                    .hint_text("/caminho/para/llama-server"),
            );
            if r.lost_focus() || ui.button("Usar").clicked() {
                let p = self.llama_path.trim();
                let new = (!p.is_empty()).then(|| p.to_string());
                if new != self.draft.local.llama_server {
                    if new
                        .as_deref()
                        .map(|p| claudio_core::config::expand(p).is_file())
                        .unwrap_or(true)
                    {
                        self.draft.local.llama_server = new;
                        self.dirty = true;
                    } else {
                        self.error("arquivo do llama-server não encontrado");
                    }
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label("Camadas na GPU");
            if ui
                .add(egui::Slider::new(&mut self.draft.local.gpu_layers, 0..=99))
                .changed()
            {
                self.dirty = true;
            }
            ui.label("Contexto");
            if ui
                .add(
                    egui::DragValue::new(&mut self.draft.local.ctx_size)
                        .range(2048..=131072)
                        .speed(512),
                )
                .changed()
            {
                self.dirty = true;
            }
        });
        ui.label(RichText::new("GPU: use a variante Vulkan e aumente as camadas (ex.: 99 com 8 GB de VRAM, ~20 com 4 GB).").small().weak());

        // 2. Catalog
        ui.add_space(10.0);
        ui.heading("2. Escolha um modelo");
        ui.label(
            RichText::new(format!(
                "Memória deste computador: {:.1} GB. Os arquivos vão para {}",
                self.mem_total_mb as f32 / 1024.0,
                claudio_core::config::models_dir().display()
            ))
            .small(),
        );
        let current_gguf = self.draft.model("local").and_then(|m| m.gguf_path.clone());
        let mut start: Option<&'static claudio_llm::catalog::LocalModel> = None;
        let mut use_now: Option<&'static claudio_llm::catalog::LocalModel> = None;
        egui::Grid::new("catalog")
            .striped(true)
            .num_columns(4)
            .spacing([10.0, 6.0])
            .show(ui, |ui| {
                for m in LOCAL_MODELS {
                    let path = downloads::model_path(m);
                    let downloaded = path.is_file();
                    let in_use = current_gguf.as_deref() == Some(path.to_string_lossy().as_ref());
                    ui.vertical(|ui| {
                        ui.set_min_width(300.0);
                        ui.set_max_width(320.0);
                        ui.label(RichText::new(m.name).strong());
                        ui.label(RichText::new(m.description).small().weak());
                    });
                    ui.label(format!("{:.1} GB", m.size_mb as f32 / 1024.0));
                    if m.fits(self.mem_total_mb) {
                        ui.label(
                            RichText::new(format!("RAM ≥ {} GB ✔", m.min_ram_mb / 1000))
                                .color(Color32::LIGHT_GREEN),
                        );
                    } else {
                        ui.label(
                            RichText::new(format!("RAM ≥ {} GB ✖", m.min_ram_mb / 1000))
                                .color(Color32::LIGHT_RED),
                        );
                    }
                    ui.vertical(|ui| {
                        if let Some(job) = self.jobs.get(m.id) {
                            Self::progress_row(ui, job);
                        } else if in_use {
                            ui.label(RichText::new("★ em uso").color(Color32::GOLD));
                        } else if downloaded {
                            if ui.button("Usar").clicked() {
                                use_now = Some(m);
                            }
                        } else if ui.button("⬇ Baixar").clicked() {
                            start = Some(m);
                        }
                    });
                    ui.end_row();
                }
            });
        if let Some(m) = use_now {
            downloads::use_local_model(
                &mut self.draft,
                m.model_config(&downloads::model_path(m).to_string_lossy()),
            );
            self.dirty = true;
            self.info(format!("{} selecionado; clique em Salvar", m.name));
        }
        if let Some(m) = start {
            let (url, dest) = (downloads::model_url(m), downloads::model_path(m));
            self.start_job(m.id, tokio, move |p, c| {
                Box::pin(async move { downloads::download(&url, &dest, &p, &c).await })
            });
        }

        // 3. Other sources
        ui.add_space(10.0);
        ui.heading("3. Outras opções");
        ui.horizontal(|ui| {
            ui.label("Baixar de uma URL .gguf:");
            ui.add(
                egui::TextEdit::singleline(&mut self.url_input)
                    .desired_width(360.0)
                    .hint_text("https://huggingface.co/…/arquivo.gguf"),
            );
            if ui
                .add_enabled(
                    !self.jobs.contains_key("url"),
                    egui::Button::new("⬇ Baixar"),
                )
                .clicked()
            {
                match downloads::file_name_from_url(self.url_input.trim()) {
                    Ok(name) => {
                        let url = self.url_input.trim().to_string();
                        let dest = claudio_core::config::models_dir().join(name);
                        self.start_job("url", tokio, move |p, c| {
                            Box::pin(async move { downloads::download(&url, &dest, &p, &c).await })
                        });
                    }
                    Err(e) => self.error(format!("{e:#}")),
                }
            }
        });
        if let Some(job) = self.jobs.get("url") {
            Self::progress_row(ui, job);
        }
        ui.horizontal(|ui| {
            ui.label("Usar arquivo .gguf do computador:");
            ui.add(
                egui::TextEdit::singleline(&mut self.file_input)
                    .desired_width(360.0)
                    .hint_text("/caminho/para/modelo.gguf"),
            );
            if ui.button("Usar").clicked() {
                let p = claudio_core::config::expand(self.file_input.trim());
                if p.is_file() && p.to_string_lossy().to_lowercase().ends_with(".gguf") {
                    downloads::use_local_model(&mut self.draft, downloads::custom_gguf_config(&p));
                    self.dirty = true;
                    self.info("arquivo selecionado; clique em Salvar");
                } else {
                    self.error("arquivo .gguf não encontrado");
                }
            }
        });
    }

    // ------------------------------------------------------------ General ---

    fn general_tab(&mut self, ui: &mut egui::Ui) {
        let d = &mut self.draft;
        let mut changed = false;
        egui::Grid::new("general")
            .num_columns(2)
            .spacing([12.0, 10.0])
            .show(ui, |ui| {
                ui.label("Permissões");
                egui::ComboBox::from_id_salt("mode")
                    .selected_text(match d.permissions.mode {
                        Mode::Strict => "Estrito (pede tudo)",
                        Mode::Ask => "Perguntar (padrão)",
                        Mode::AutoSafe => "Automático seguro (escrita no workspace sem perguntar)",
                    })
                    .show_ui(ui, |ui| {
                        changed |= ui
                            .selectable_value(
                                &mut d.permissions.mode,
                                Mode::Strict,
                                "Estrito (pede tudo)",
                            )
                            .changed();
                        changed |= ui
                            .selectable_value(
                                &mut d.permissions.mode,
                                Mode::Ask,
                                "Perguntar (padrão)",
                            )
                            .changed();
                        changed |= ui
                            .selectable_value(
                                &mut d.permissions.mode,
                                Mode::AutoSafe,
                                "Automático seguro (escrita no workspace sem perguntar)",
                            )
                            .changed();
                    });
                ui.end_row();
                ui.label("Pasta de trabalho");
                changed |= ui.text_edit_singleline(&mut self.workspace).changed();
                ui.end_row();
                ui.label("Privacidade");
                changed |= ui
                    .checkbox(
                        &mut d.privacy.local_only,
                        "Usar só modelos locais (nunca enviar dados para APIs)",
                    )
                    .changed();
                ui.end_row();
                ui.label("");
                changed |= ui
                    .checkbox(
                        &mut d.privacy.prefer_local,
                        "Preferir modelo local quando houver",
                    )
                    .changed();
                ui.end_row();
                ui.label("Verificação");
                changed |= ui
                    .checkbox(
                        &mut d.agent.verify,
                        "Verificar o resultado antes de concluir",
                    )
                    .changed();
                ui.end_row();
                ui.label("Idioma");
                changed |= ui.text_edit_singleline(&mut d.agent.language).changed();
                ui.end_row();
                ui.label("Máx. de iterações");
                changed |= ui
                    .add(egui::DragValue::new(&mut d.agent.max_iterations).range(1..=500))
                    .changed();
                ui.end_row();
            });
        if changed {
            let ws = self.workspace.trim().to_string();
            if !ws.is_empty() {
                if self.draft.permissions.workspace_roots.is_empty() {
                    self.draft.permissions.workspace_roots.push(ws);
                } else {
                    self.draft.permissions.workspace_roots[0] = ws;
                }
            }
            self.dirty = true;
        }
    }
}
