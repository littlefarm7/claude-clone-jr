//! Administrator authorization screen: shows the exact command, asks the user
//! to take responsibility and, for `sudo`, to type their password.

use claudio_core::events::{ElevationKind, ElevationRequest, ElevationResponse};
use eframe::egui::{self, RichText, Vec2};
use tokio::sync::oneshot;
use zeroize::Zeroizing;

use crate::theme;

pub struct AdminPrompt {
    req: ElevationRequest,
    tx: Option<oneshot::Sender<ElevationResponse>>,
    password: Zeroizing<String>,
    accepted: bool,
    focus: bool,
}

impl AdminPrompt {
    pub fn new(req: ElevationRequest, tx: oneshot::Sender<ElevationResponse>) -> Self {
        Self {
            req,
            tx: Some(tx),
            password: Zeroizing::new(String::new()),
            accepted: false,
            focus: true,
        }
    }

    fn answer(&mut self, r: ElevationResponse) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(r);
        }
    }

    /// Draw the screen. Returns true when it was answered (close it).
    pub fn show(&mut self, ctx: &egui::Context) -> bool {
        let p = theme::current(ctx);
        let needs_password = self.req.kind == ElevationKind::Password;
        let mut result: Option<bool> = None;
        egui::Modal::new(egui::Id::new("admin_prompt"))
            .frame(theme::modal_frame(ctx))
            .backdrop_color(egui::Color32::from_black_alpha(160))
            .show(ctx, |ui| {
                ui.set_width(480.0_f32.min(ctx.screen_rect().width() - 80.0));
                ui.horizontal(|ui| {
                    ui.label(RichText::new("🔐").size(28.0));
                    ui.add(
                        egui::Label::new(
                            RichText::new("O Claudio pede acesso de administrador")
                                .size(19.0)
                                .strong(),
                        )
                        .wrap(),
                    );
                });
                if !self.req.reason.trim().is_empty() {
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!("Motivo: {}", self.req.reason)).color(p.muted),
                        )
                        .wrap(),
                    );
                }
                theme::card(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        RichText::new("Comando que vai rodar com acesso total ao sistema:")
                            .small()
                            .color(p.muted),
                    );
                    egui::ScrollArea::vertical()
                        .max_height(140.0)
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(RichText::new(&self.req.command).monospace())
                                    .wrap()
                                    .selectable(true),
                            );
                        });
                });
                ui.add(
                    egui::Label::new(
                        RichText::new(
                            "⚠ Comandos de administrador podem alterar ou danificar o sistema. \
                             Só autorize se você entende e concorda com o que está escrito acima.",
                        )
                        .color(p.warning),
                    )
                    .wrap(),
                );
                theme::toggle(
                    ui,
                    &mut self.accepted,
                    "Eu li o comando e assumo a responsabilidade",
                );
                let mut submit = false;
                if needs_password {
                    ui.label(RichText::new("Senha do seu usuário (administrador)").strong());
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut *self.password)
                            .password(true)
                            .hint_text("digite a senha")
                            .desired_width(f32::INFINITY)
                            .margin(Vec2::new(12.0, 10.0)),
                    );
                    if self.focus {
                        r.request_focus();
                        self.focus = false;
                    }
                    submit = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.label(
                        RichText::new(
                            "A senha vai direto para o sistema: não é enviada à IA, não é salva e é apagada da memória logo depois.",
                        )
                        .small()
                        .color(p.muted),
                    );
                } else {
                    ui.label(
                        RichText::new(if cfg!(windows) {
                            "Depois de autorizar aqui, o Windows mostra a tela dele para confirmar (UAC)."
                        } else {
                            "Depois de autorizar aqui, o sistema mostra a tela dele pedindo a senha."
                        })
                        .small()
                        .color(p.muted),
                    );
                }
                ui.add_space(4.0);
                let ready = self.accepted && (!needs_password || !self.password.is_empty());
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(ready, |ui| {
                        if theme::primary_button(ui, "  Autorizar  ").clicked() {
                            submit = true;
                        }
                    });
                    if theme::pill_button(ui, "Cancelar").clicked()
                        || ui.input(|i| i.key_pressed(egui::Key::Escape))
                    {
                        result = Some(false);
                    }
                });
                if submit && ready {
                    result = Some(true);
                }
            });
        match result {
            Some(true) => {
                let r = if needs_password {
                    ElevationResponse::Password(std::mem::take(&mut self.password))
                } else {
                    ElevationResponse::Confirmed
                };
                self.answer(r);
                true
            }
            Some(false) => {
                self.answer(ElevationResponse::Denied);
                true
            }
            None => false,
        }
    }
}

impl Drop for AdminPrompt {
    fn drop(&mut self) {
        self.answer(ElevationResponse::Denied);
    }
}
