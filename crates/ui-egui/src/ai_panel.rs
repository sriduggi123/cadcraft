//! Tools ▸ AI Assistant…: a chat window. What the user types goes to [`cadcraft_ai::Assistant`],
//! which reads and changes the open drawing through tools; this panel only shows the chat.

use cadcraft_ai::{Answer, Assistant, Entry, Need};
use serde_json::{Value, json};

use crate::CadApp;
use crate::theme::Tokens;

#[derive(Default)]
pub struct State {
    pub open: bool,
    input: String,
    /// Made on first use, with the cloud server of the Cloud Files settings.
    assistant: Option<Assistant>,
    /// Put the cursor in the input box on the next frame.
    focus: bool,
}

/// The cloud server the assistant talks to: the one set in Cloud Files, or the default.
fn server(app: &CadApp) -> String {
    let set = app.cloud.server.trim();
    let p = if set.is_empty() { json!({}) } else { json!({ "server": set }) };
    cadcraft_engine::cmd::collab::cloud_server(&p)
}

/// `ui.ai`: open the assistant (or bring it to the front).
pub fn route(app: &mut CadApp, id: &str, _params: &Value) -> Option<Result<Value, String>> {
    if id != "ui.ai" {
        return None;
    }
    app.ai.open = true;
    app.ai.focus = true;
    Some(Ok(Value::Null))
}

/// Pick up the model's answers and run its tool calls (also while the window is closed).
pub fn tick(app: &mut CadApp, ctx: &egui::Context) {
    let Some(a) = app.ai.assistant.as_mut() else { return };
    if !a.busy() {
        return;
    }
    if a.tick(&mut app.session) {
        ctx.request_repaint();
    }
    // A question or a pick needs the window, even if it was closed meanwhile.
    if a.need().is_some() {
        app.ai.open = true;
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(50));
}

enum Do {
    Send(String),
    Answer(Answer),
    Stop,
    Clear,
}

pub fn show(app: &mut CadApp, ctx: &egui::Context) {
    if !app.ai.open {
        return;
    }
    let server = server(app);
    let st = &mut app.ai;
    let assistant = st.assistant.get_or_insert_with(|| Assistant::new(&server));
    assistant.set_server(&server);
    let busy = assistant.busy();
    let need = assistant.need().cloned();
    let t = Tokens::get();
    let mut open = true;
    let mut action = None;
    egui::Window::new(crate::tl!("AI Assistant"))
        .open(&mut open)
        .default_width(380.0)
        .default_height(480.0)
        .default_pos(ctx.content_rect().right_top() + egui::vec2(-400.0, 80.0))
        .show(ctx, |ui| {
            let input_height = 90.0;
            egui::ScrollArea::vertical().stick_to_bottom(true).max_height((ui.available_height() - input_height).max(120.0)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                if assistant.log.is_empty() {
                    ui.label(egui::RichText::new(crate::tl!("Ask about the drawing, or ask for a change: \u{201c}dimension the plate\u{201d}, \u{201c}move the text on layer Notes to Annotation\u{201d}.")).weak());
                }
                for e in &assistant.log {
                    match e {
                        Entry::User(text) => {
                            ui.add_space(6.0);
                            ui.label(egui::RichText::new(text).strong());
                        }
                        Entry::Assistant(text) => {
                            ui.label(text);
                        }
                        Entry::Tool { line, ok } => {
                            let mark = if *ok { "✓" } else { "✗" };
                            let color = if *ok { t.text_dim } else { t.warn };
                            ui.label(egui::RichText::new(format!("{mark} {line}")).small().color(color));
                        }
                        Entry::Error(e) => {
                            ui.colored_label(t.warn, e);
                        }
                    }
                }
                if busy && need.is_none() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        let secs = assistant.working().map_or(0, |(t, _)| t.as_secs());
                        ui.label(format!("{} {secs} s", crate::tl!("Working…")));
                    });
                }
            });
            ui.separator();
            if let Some(need) = &need {
                show_need(ui, need, &t, &mut action);
            }
            let r = ui.add(
                egui::TextEdit::multiline(&mut st.input)
                    .hint_text(crate::tl!("Message the assistant (Enter sends, Shift+Enter for a new line)"))
                    .desired_rows(2)
                    .desired_width(f32::INFINITY),
            );
            if std::mem::take(&mut st.focus) {
                r.request_focus();
            }
            // Enter sends; Shift+Enter is a new line.
            let enter = r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
            if enter {
                let text = st.input.trim_end_matches('\n').to_string();
                st.input = text;
            }
            ui.horizontal(|ui| {
                // While the assistant asks a question, what is typed is the answer.
                let asked = matches!(need, Some(Need::Ask { .. }));
                let can = (!busy || asked) && !st.input.trim().is_empty();
                if ui.add_enabled(can, egui::Button::new(crate::tl!("Send"))).clicked() || (enter && can) {
                    let text = std::mem::take(&mut st.input);
                    action = Some(if asked { Do::Answer(Answer::Text(text)) } else { Do::Send(text) });
                    st.focus = true;
                }
                if busy && ui.button(crate::tl!("Stop")).clicked() {
                    action = Some(Do::Stop);
                }
                if !busy
                    && !assistant.log.is_empty()
                    && ui.button(crate::tl!("New Chat")).on_hover_text(crate::tl!("Start over; the drawing keeps its changes")).clicked()
                {
                    action = Some(Do::Clear);
                }
            });
        });
    if !open {
        st.open = false;
    }
    let Some(action) = action else { return };
    let Some(a) = app.ai.assistant.as_mut() else { return };
    match action {
        Do::Send(text) => a.ask(&mut app.session, &text),
        Do::Answer(answer) => a.answer(&mut app.session, answer),
        Do::Stop => a.stop(&mut app.session),
        Do::Clear => a.clear(&mut app.session),
    }
    ctx.request_repaint();
}

/// What the assistant waits for, with the buttons that answer it.
fn show_need(ui: &mut egui::Ui, need: &Need, t: &Tokens, action: &mut Option<Do>) {
    egui::Frame::new().fill(t.field).inner_margin(8.0).corner_radius(4.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        match need {
            Need::Confirm { what, lisp } => {
                ui.label(egui::RichText::new(crate::tl!("The assistant wants to:")).strong());
                egui::ScrollArea::vertical().id_salt("ai_confirm").max_height(140.0).show(ui, |ui| {
                    ui.label(egui::RichText::new(what).monospace());
                });
                ui.horizontal(|ui| {
                    if ui.button(crate::tl!("Allow")).clicked() {
                        *action = Some(Do::Answer(Answer::Allow));
                    }
                    if *lisp && ui.button(crate::tl!("Allow LISP in This Chat")).clicked() {
                        *action = Some(Do::Answer(Answer::AllowLisp));
                    }
                    if ui.button(crate::tl!("Don't Allow")).clicked() {
                        *action = Some(Do::Answer(Answer::Deny));
                    }
                });
            }
            Need::Ask { question, options } => {
                ui.label(egui::RichText::new(question).strong());
                ui.horizontal_wrapped(|ui| {
                    for o in options {
                        if ui.button(o).clicked() {
                            *action = Some(Do::Answer(Answer::Text(o.clone())));
                        }
                    }
                    if ui.small_button(crate::tl!("Skip")).clicked() {
                        *action = Some(Do::Answer(Answer::Cancel));
                    }
                });
                ui.label(egui::RichText::new(crate::tl!("Or type an answer below.")).small().color(t.text_dim));
            }
            Need::Points { prompt, count } => {
                let what = if *count == 1 {
                    crate::tl!("Click a point in the drawing.").to_string()
                } else {
                    format!("{} {count}", crate::tl!("Points to click in the drawing:"))
                };
                ui.label(egui::RichText::new(prompt).strong());
                ui.label(what);
                if ui.button(crate::tl!("Cancel")).clicked() {
                    *action = Some(Do::Answer(Answer::Cancel));
                }
            }
            Need::Objects { prompt } => {
                ui.label(egui::RichText::new(prompt).strong());
                ui.label(crate::tl!("Select the objects in the drawing, then press Done."));
                ui.horizontal(|ui| {
                    if ui.button(crate::tl!("Done")).clicked() {
                        *action = Some(Do::Answer(Answer::Done));
                    }
                    if ui.button(crate::tl!("Cancel")).clicked() {
                        *action = Some(Do::Answer(Answer::Cancel));
                    }
                });
            }
        }
    });
    ui.add_space(4.0);
}
