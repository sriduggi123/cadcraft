//! Cloud files (the fork's multiplayer work): File ▸ Open from Cloud… and Save to Cloud… open
//! one dialog listing the server's files and who has each open; the status bar names the cloud
//! file of the active tab and how many people are in it. The work is the engine's `cloud.list`,
//! `cloud.open` and `cloud.save` commands; this module only shows them. What waits on the network
//! (the list, connecting) runs on a background thread, so the window never stops responding.

use std::sync::mpsc::{Receiver, TryRecvError, channel};

use cadcraft_engine::cmd::collab::{self as cloud, CloudJoin};
use cadcraft_geom::Vec2;
use egui::{Color32, Rect, Sense, Shape, Stroke, pos2, vec2};
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
    busy: Option<Busy>,
    /// The cursor over the drawing, in drawing coordinates (set by the canvas), sent as presence.
    pub cursor: Option<Vec2>,
}

/// Network work in progress.
enum Busy {
    List(Receiver<Result<Vec<Value>, String>>),
    Connect {
        how: CloudJoin,
        name: String,
        url: String,
        user: String,
        link: Receiver<Result<Box<dyn cadcraft_engine::collab::Link>, String>>,
    },
    /// Connected; waiting for the server's welcome (it arrives through `collab_tick`).
    Join {
        how: CloudJoin,
        name: String,
        since: f64,
    },
}

/// How long the welcome may take once connected.
const JOIN_WAIT: f64 = 20.0;

/// Run `f` on a background thread; its result arrives on the receiver.
#[cfg(not(target_arch = "wasm32"))]
fn background<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Receiver<T> {
    let (tx, rx) = channel();
    let _ = std::thread::Builder::new().name("cloud".into()).spawn(move || {
        let _ = tx.send(f());
    });
    rx
}

/// The browser has no threads; connecting there doesn't wait anyway.
#[cfg(target_arch = "wasm32")]
fn background<T>(f: impl FnOnce() -> T) -> Receiver<T> {
    let (tx, rx) = channel();
    let _ = tx.send(f());
    rx
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
    let server = cloud::cloud_server(&params(app));
    app.cloud.error = None;
    #[cfg(not(target_arch = "wasm32"))]
    {
        app.cloud.busy = Some(Busy::List(background(move || cloud::list_files(&server).map_err(|e| e.to_string()))));
    }
    #[cfg(target_arch = "wasm32")]
    {
        app.cloud.error = Some(format!("open {} in the browser for the file list", server.replacen("ws", "http", 1)));
    }
}

/// Open or save `name` in the background.
fn start(app: &mut CadApp, how: CloudJoin, name: String) {
    let p = params(app);
    let url = cloud::cloud_file_url(&cloud::cloud_server(&p), &name);
    if let Err(e) = cloud::check_cloud_name(&name) {
        app.cloud.error = Some(e.to_string());
        return;
    }
    // Already in that file: switching to its tab needs no network.
    if how == CloudJoin::Open && app.session.collab_url() == Some(url.as_str()) {
        let mut p = p;
        p["name"] = json!(name);
        let _ = app.run("cloud.open", p);
        app.cloud.open = false;
        return;
    }
    let user = cloud::cloud_user(&p);
    let target = url.clone();
    app.cloud.error = None;
    app.cloud.busy = Some(Busy::Connect { how, name, url, user, link: background(move || cloud::cloud_connect(&target).map_err(|e| e.to_string())) });
}

/// Move the network work along; called every frame, with the dialog open or not.
pub fn tick(app: &mut CadApp, ctx: &egui::Context) {
    app.session.collab_presence(app.cloud.cursor, ctx.input(|i| i.time));
    let Some(busy) = app.cloud.busy.take() else { return };
    ctx.request_repaint_after(std::time::Duration::from_millis(50));
    let now = ctx.input(|i| i.time);
    app.cloud.busy = match busy {
        Busy::List(rx) => match rx.try_recv() {
            Ok(Ok(files)) => {
                app.cloud.files = files;
                None
            }
            Ok(Err(e)) => {
                app.cloud.files.clear();
                app.cloud.error = Some(e);
                None
            }
            Err(TryRecvError::Empty) => Some(Busy::List(rx)),
            Err(TryRecvError::Disconnected) => None,
        },
        Busy::Connect { how, name, url, user, link } => match link.try_recv() {
            Ok(Ok(link)) => match cloud::cloud_begin(&mut app.session, how, link, &url, &name, &user) {
                Ok(()) => Some(Busy::Join { how, name, since: now }),
                Err(e) => {
                    app.cloud.error = Some(e.to_string());
                    None
                }
            },
            Ok(Err(e)) => {
                app.cloud.error = Some(e);
                None
            }
            Err(TryRecvError::Empty) => Some(Busy::Connect { how, name, url, user, link }),
            Err(TryRecvError::Disconnected) => None,
        },
        Busy::Join { how, name, since } => {
            if app.session.collab_joined() {
                cloud::cloud_joined(&mut app.session, how, &name);
                app.cloud.open = false;
                app.canvas.list = None;
                None
            } else if let Some(why) = app.session.collab_ended() {
                app.cloud.error = Some(format!("could not join: {why}"));
                app.cloud.open = true;
                None
            } else if now - since > JOIN_WAIT {
                app.session.collab_leave();
                app.cloud.error = Some("the server did not answer in time".into());
                app.cloud.open = true;
                None
            } else {
                Some(Busy::Join { how, name, since })
            }
        }
    };
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
    let others = app.session.collab_others();
    let following = app.session.collab_following();
    let mut follow: Option<Option<u32>> = None;
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
        ui.horizontal(|ui| {
            if ui.add_enabled(st.busy.is_none(), egui::Button::new(crate::tl!("Refresh"))).clicked() {
                action = Some(Action::Refresh);
            }
            match &st.busy {
                Some(Busy::List(_)) => {
                    ui.spinner();
                    ui.label(crate::tl!("Loading…"));
                }
                Some(Busy::Connect { name, .. } | Busy::Join { name, .. }) => {
                    ui.spinner();
                    ui.label(format!("{} {name}…", crate::tl!("Connecting to")));
                }
                None => {}
            }
        });
        if let Some(e) = &st.error {
            ui.colored_label(Tokens::get().warn, e);
        }
        ui.separator();
        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
            if st.files.is_empty() && st.error.is_none() && st.busy.is_none() {
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
                    if ui.add_enabled(st.busy.is_none(), egui::Button::new(crate::tl!("Open"))).clicked() {
                        action = Some(Action::Open(name.to_string()));
                    }
                    ui.end_row();
                }
            });
        });
        if shared && !others.is_empty() {
            ui.separator();
            ui.label(egui::RichText::new(crate::tl!("People here")).strong());
            egui::Grid::new("cloud_people").num_columns(3).spacing([12.0, 6.0]).show(ui, |ui| {
                for (peer, p) in &others {
                    ui.label(egui::RichText::new("●").color(colour(peer.client)));
                    ui.label(match &p.command {
                        Some(cmd) => format!("{} · {cmd}", peer.name),
                        None => peer.name.clone(),
                    });
                    if following == Some(peer.client) {
                        if ui.button(crate::tl!("Stop Following")).clicked() {
                            follow = Some(None);
                        }
                    } else if ui.button(crate::tl!("Follow")).clicked() {
                        follow = Some(Some(peer.client));
                    }
                    ui.end_row();
                }
            });
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(crate::tl!("Save the current drawing as"));
            ui.add(egui::TextEdit::singleline(&mut st.save_name).desired_width(160.0));
            let can = has_doc && !shared && st.busy.is_none() && !st.save_name.trim().is_empty();
            let resp = ui.add_enabled(can, egui::Button::new(crate::tl!("Save to Cloud")));
            let resp = if shared { resp.on_disabled_hover_text(crate::tl!("This drawing is already in the cloud")) } else { resp };
            if resp.clicked() {
                action = Some(Action::Save(st.save_name.trim().to_string()));
            }
        });
    });
    app.cloud.open = open;
    if let Some(who) = follow {
        let name = who.map_or(json!(null), |c| json!(c));
        if let Err(e) = app.run("collab.follow", json!({ "name": name })) {
            app.cloud.error = Some(e);
        }
    }
    match action {
        None => {}
        Some(Action::Refresh) => refresh(app),
        Some(Action::Open(name)) => start(app, CloudJoin::Open, name),
        Some(Action::Save(name)) => start(app, CloudJoin::Save, name),
    }
}

/// The colour of client `client` in this session.
fn colour(client: u32) -> Color32 {
    let p = Tokens::get().presence;
    p.get(client as usize % p.len()).copied().unwrap_or(Color32::GRAY)
}

/// Most objects outlined per person.
const MAX_OUTLINES: usize = 500;

/// The others' selections (outlined in their colour) and cursors with name tags, over the
/// drawing of the shared tab.
pub fn paint_presence(app: &CadApp, painter: &egui::Painter, xf: &crate::canvas::Xf, rect: Rect) {
    let Ok(st) = app.session.state() else { return };
    if app.session.collab_uid() != Some(st.uid) {
        return;
    }
    let layout = match &st.space {
        cadcraft_doc::Space::Model => String::new(),
        cadcraft_doc::Space::Paper(n) => n.clone(),
    };
    let painter = painter.with_clip_rect(rect);
    for (peer, p) in app.session.collab_others().iter().filter(|(_, p)| p.layout == layout) {
        let c = colour(peer.client);
        for h in p.selection.iter().take(MAX_OUTLINES) {
            let Some(e) = st.doc.entity(*h) else { continue };
            let b = cadcraft_doc::extent_bounds(&st.doc, e);
            if b.is_empty() {
                continue;
            }
            let r = Rect::from_two_pos(xf.to_screen(b.min), xf.to_screen(b.max)).expand(3.0);
            if r.intersects(rect) {
                painter.rect_filled(r, 2.0, c.gamma_multiply(0.08));
                painter.rect_stroke(r, 2.0, Stroke::new(1.5, c), egui::StrokeKind::Middle);
            }
        }
        let Some([x, y]) = p.cursor else { continue };
        let at = xf.to_screen(Vec2::new(x, y));
        if !rect.contains(at) {
            continue;
        }
        let arrow = vec![at, at + vec2(0.0, 16.0), at + vec2(4.5, 12.0), at + vec2(11.0, 11.5)];
        painter.add(Shape::convex_polygon(arrow, c, Stroke::new(1.0, Color32::WHITE)));
        let tag = match &p.command {
            Some(cmd) => format!("{} · {cmd}", peer.name),
            None => peer.name.clone(),
        };
        let g = painter.layout_no_wrap(tag, crate::theme::small(), Color32::WHITE);
        let r = Rect::from_min_size(at + vec2(12.0, 14.0), g.size() + vec2(10.0, 4.0));
        painter.rect_filled(r, 3.0, c);
        painter.galley(r.min + vec2(5.0, 2.0), g, Color32::WHITE);
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
    let lead = app.session.collab_following().and_then(|c| app.session.collab_others().into_iter().find(|(p, _)| p.client == c)).map(|(p, _)| p.name);
    let text = match lead {
        Some(lead) => format!("Cloud: {name} · {people} here · following {lead}"),
        None => format!("Cloud: {name} · {people} here"),
    };
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
