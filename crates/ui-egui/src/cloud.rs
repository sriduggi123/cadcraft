//! Cloud files (the fork's multiplayer work): File ▸ Open from Cloud… and Save to Cloud… open
//! one dialog listing the server's files and who has each open; the status bar names the cloud
//! file of the active tab and how many people are in it. The work is the engine's `cloud.list`,
//! `cloud.open` and `cloud.save` commands; this module only shows them.

use egui::{Rect, Sense, pos2, vec2};
use serde_json::{Value, json};

use crate::CadApp;
use crate::theme::Tokens;

/// The dialog's state; the server address and name are kept with the preferences.
#[derive(Default)]
pub struct State {
    pub open: bool,
    pub server: String,
    pub user: String,
    save_name: String,
    files: Vec<Value>,
    error: Option<String>,
}

enum Action {
    Refresh,
    Open(String),
    Save(String),
}

/// `ui.cloud` and `ui.cloud.save`: open the dialog with a fresh list.
pub fn route(app: &mut CadApp, id: &str, _params: &Value) -> Option<Result<Value, String>> {
    if !matches!(id, "ui.cloud" | "ui.cloud.save") {
        return None;
    }
    app.cloud.open = true;
    if id == "ui.cloud.save" && app.cloud.save_name.is_empty() {
        app.cloud.save_name = app.session.state().map(|st| suggested_name(&st.title)).unwrap_or_default();
    }
    refresh(app);
    Some(Ok(Value::Null))
}

/// A cloud file name from a tab title: `Floor plan.dwg` → `Floor-plan`.
fn suggested_name(title: &str) -> String {
    let stem = title.rsplit_once('.').map_or(title, |(s, _)| s);
    stem.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '-' }).take(64).collect()
}

fn params(app: &CadApp) -> Value {
    let mut p = json!({});
    if !app.cloud.server.trim().is_empty() {
        p["server"] = json!(app.cloud.server.trim());
    }
    if !app.cloud.user.trim().is_empty() {
        p["user"] = json!(app.cloud.user.trim());
    }
    p
}

fn refresh(app: &mut CadApp) {
    let p = params(app);
    match app.run("cloud.list", p) {
        Ok(r) => {
            app.cloud.files = r.get("files").and_then(Value::as_array).cloned().unwrap_or_default();
            app.cloud.error = None;
        }
        Err(e) => {
            app.cloud.files.clear();
            app.cloud.error = Some(e);
        }
    }
}

/// "3 min ago" for a time in seconds since 1970 (empty in the browser, which has no clock here).
fn ago(modified: u64) -> String {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = modified;
        String::new()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        match now.saturating_sub(modified) {
            0..60 => crate::tl!("just now").to_string(),
            s @ 60..3600 => format!("{} min ago", s / 60),
            s @ 3600..86_400 => format!("{} h ago", s / 3600),
            s => format!("{} days ago", s / 86_400),
        }
    }
}

/// Draw the dialog while open and act on what was chosen.
pub fn show(app: &mut CadApp, ctx: &egui::Context) {
    if !app.cloud.open {
        return;
    }
    let shared = app.session.collab_live();
    let has_doc = app.session.state().is_ok();
    let mut open = true;
    let mut action = None;
    let st = &mut app.cloud;
    egui::Window::new(crate::tl!("Cloud Files")).open(&mut open).collapsible(false).default_width(460.0).show(ctx, |ui| {
        egui::Grid::new("cloud_conn").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            ui.label(crate::tl!("Server"));
            ui.add(egui::TextEdit::singleline(&mut st.server).hint_text("ws://127.0.0.1:9010").desired_width(300.0));
            ui.end_row();
            ui.label(crate::tl!("Your name"));
            ui.add(egui::TextEdit::singleline(&mut st.user).desired_width(300.0));
            ui.end_row();
        });
        if ui.button(crate::tl!("Refresh")).clicked() {
            action = Some(Action::Refresh);
        }
        if let Some(e) = &st.error {
            ui.colored_label(Tokens::get().warn, e);
        }
        ui.separator();
        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
            if st.files.is_empty() && st.error.is_none() {
                ui.label(crate::tl!("No files yet."));
            }
            egui::Grid::new("cloud_files").num_columns(4).striped(true).spacing([12.0, 6.0]).show(ui, |ui| {
                for f in &st.files {
                    let name = f.get("name").and_then(Value::as_str).unwrap_or_default();
                    let people: Vec<&str> =
                        f.get("people").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
                    ui.label(egui::RichText::new(name).strong());
                    ui.label(if people.is_empty() { String::new() } else { format!("{} editing", people.join(", ")) });
                    ui.label(ago(f.get("modified").and_then(Value::as_u64).unwrap_or(0)));
                    if ui.button(crate::tl!("Open")).clicked() {
                        action = Some(Action::Open(name.to_string()));
                    }
                    ui.end_row();
                }
            });
        });
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(crate::tl!("Save the current drawing as"));
            ui.add(egui::TextEdit::singleline(&mut st.save_name).desired_width(160.0));
            let can = has_doc && !shared && !st.save_name.trim().is_empty();
            let resp = ui.add_enabled(can, egui::Button::new(crate::tl!("Save to Cloud")));
            let resp = if shared { resp.on_disabled_hover_text(crate::tl!("This drawing is already in the cloud")) } else { resp };
            if resp.clicked() {
                action = Some(Action::Save(st.save_name.trim().to_string()));
            }
        });
    });
    app.cloud.open = open;
    let result = match action {
        None => return,
        Some(Action::Refresh) => return refresh(app),
        Some(Action::Open(name)) => {
            let mut p = params(app);
            p["name"] = json!(name);
            app.run("cloud.open", p)
        }
        Some(Action::Save(name)) => {
            let mut p = params(app);
            p["name"] = json!(name);
            app.run("cloud.save", p)
        }
    };
    match result {
        Ok(_) => {
            app.cloud.open = false;
            app.cloud.error = None;
            app.canvas.list = None;
        }
        Err(e) => app.cloud.error = Some(e),
    }
}

/// The status bar's cloud label for the active tab ("Cloud: plan · 2 here"), right-aligned at
/// `right`; clicking it opens the dialog. Returns the left edge for the next item.
pub fn status(app: &mut CadApp, ui: &mut egui::Ui, right: f32, cy: f32) -> f32 {
    let Some(url) = app.session.collab_url() else { return right };
    let active = app.session.state().map(|st| st.uid).ok();
    if active.is_none() || active != app.session.collab_uid() {
        return right;
    }
    let name = url.rsplit('/').next().unwrap_or_default().to_string();
    let people = app.session.collab_status().map_or(0, |(_, p, _)| p.len());
    let text = format!("Cloud: {name} · {people} here");
    let t = Tokens::get();
    let p = ui.painter().clone();
    let g = p.layout_no_wrap(text, crate::theme::small(), t.text);
    let w = g.size().x.min(220.0) + 12.0;
    let r = Rect::from_min_size(pos2(right - w, cy - 9.0), vec2(w, 18.0));
    let resp = ui.interact(r, ui.scope_id().with("cloud_status"), Sense::click());
    p.rect_filled(r, 3.0, if resp.hovered() { t.control_hover } else { t.toggle_on.gamma_multiply(0.25) });
    p.with_clip_rect(r.shrink2(vec2(4.0, 0.0))).galley(pos2(r.left() + 6.0, r.center().y - g.size().y / 2.0), g, t.text);
    let names = app.session.collab_status().map(|(_, p, _)| p.iter().map(|p| p.name.clone()).collect::<Vec<_>>().join(", ")).unwrap_or_default();
    crate::icons::describe_control(ui, &resp, "Cloud file", None, false);
    if resp.on_hover_text(names).clicked() {
        let _ = app.run("ui.cloud", json!({}));
    }
    r.left() - 10.0
}

#[cfg(test)]
mod tests {
    use super::suggested_name;

    #[test]
    fn a_tab_title_suggests_a_cloud_name() {
        assert_eq!(suggested_name("Floor plan.dwg"), "Floor-plan");
        assert_eq!(suggested_name("Drawing1"), "Drawing1");
        assert_eq!(suggested_name("a.b.dxf"), "a-b");
    }
}
