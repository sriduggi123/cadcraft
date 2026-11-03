//! Cloud files (the fork's multiplayer work). File ▸ Collaborate… puts the open drawing on the
//! server and shows its link; File ▸ Open Link… opens one in a new tab. A link is the key to its
//! file: the server lists nothing, so "My cloud files" is this app's own list of the files it
//! made or opened, kept with the preferences. The status bar names the shared tab's file and who
//! is in it; other people's cursors, selections and rubber bands are drawn over the canvas. The
//! work is the engine's `cloud.*` commands; what waits on the network runs on a background thread
//! (or, in the browser, across frames), so the window never stops responding.

use std::sync::mpsc::{Receiver, TryRecvError, channel};

use cadcraft_engine::cmd::collab::{self as cloud, CloudJoin};
use cadcraft_geom::Vec2;
use egui::{Color32, Rect, Sense, Shape, Stroke, pos2, vec2};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::CadApp;
use crate::theme::Tokens;

/// Most files kept in "My cloud files".
const MAX_RECENT: usize = 200;

/// A file in "My cloud files".
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Recent {
    pub link: String,
    pub title: String,
    /// When this app last opened it, in seconds since 1970.
    pub opened: u64,
}

/// The dialog's state; the server address, name and file list are kept with the preferences.
#[derive(Default)]
pub struct State {
    pub open: bool,
    pub server: String,
    pub user: String,
    pub recent: Vec<Recent>,
    share_title: String,
    link_input: String,
    /// Who is in each of my files, by link, from the last status question.
    people: std::collections::BTreeMap<String, Vec<String>>,
    error: Option<String>,
    notice: Option<String>,
    busy: Option<Busy>,
    /// The cursor over the drawing, in drawing coordinates (set by the canvas), sent as presence.
    pub cursor: Option<Vec2>,
}

/// Network work in progress.
enum Busy {
    #[cfg(not(target_arch = "wasm32"))]
    Status(Receiver<Result<Vec<Value>, String>>),
    Connect {
        how: CloudJoin,
        url: String,
        user: String,
        title: String,
        link: Receiver<Result<Box<dyn cadcraft_engine::collab::Link>, String>>,
    },
    /// Connected; waiting for the server's welcome (it arrives through `collab_tick`).
    Join {
        how: CloudJoin,
        since: f64,
    },
}

impl Busy {
    /// Sharing or opening a file (not just asking who is in my files).
    fn joining(&self) -> Option<CloudJoin> {
        match self {
            Busy::Connect { how, .. } | Busy::Join { how, .. } => Some(*how),
            #[cfg(not(target_arch = "wasm32"))]
            Busy::Status(_) => None,
        }
    }
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
    Share(String),
    Open(String),
    Forget(String),
    Stop,
}

/// Keep the dialog's settings and file list in the preferences.
pub fn save_prefs(st: &State, prefs: &mut Value) {
    if !st.server.is_empty() {
        prefs["cloudServer"] = json!(st.server);
    }
    if !st.user.is_empty() {
        prefs["cloudUser"] = json!(st.user);
    }
    if !st.recent.is_empty() {
        prefs["cloudRecent"] = json!(st.recent);
    }
}

pub fn load_prefs(st: &mut State, v: &Value) {
    if let Some(server) = v.get("cloudServer").and_then(Value::as_str) {
        st.server = server.to_string();
    }
    if let Some(user) = v.get("cloudUser").and_then(Value::as_str) {
        st.user = user.to_string();
    }
    if let Some(r) = v.get("cloudRecent").and_then(|r| serde_json::from_value::<Vec<Recent>>(r.clone()).ok()) {
        st.recent = r.into_iter().take(MAX_RECENT).collect();
    }
}

/// `ui.cloud` (Open Link…) and `ui.cloud.share` (Collaborate…): open the dialog.
pub fn route(app: &mut CadApp, id: &str, _params: &Value) -> Option<Result<Value, String>> {
    if !matches!(id, "ui.cloud" | "ui.cloud.share") {
        return None;
    }
    app.cloud.open = true;
    app.cloud.error = None;
    app.cloud.notice = None;
    if app.cloud.share_title.is_empty() || id == "ui.cloud.share" {
        app.cloud.share_title = cloud::default_title(&app.session);
    }
    refresh(app);
    Some(Ok(Value::Null))
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

/// Ask who is in my files (natively, in the background; the browser shows the list without it).
fn refresh(app: &mut CadApp) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        if app.cloud.busy.is_none() && !app.cloud.recent.is_empty() {
            let links: Vec<String> = app.cloud.recent.iter().map(|r| r.link.clone()).collect();
            app.cloud.busy = Some(Busy::Status(background(move || cloud::file_status(&links).map_err(|e| e.to_string()))));
        }
    }
    #[cfg(target_arch = "wasm32")]
    let _ = app;
}

/// Open a cloud file's link (from the command line, say), with the dialog showing progress.
pub fn open_link(app: &mut CadApp, link: &str) {
    app.cloud.open = true;
    start(app, CloudJoin::Open, link);
}

/// Share the active drawing as `target` (its title), or open link `target`, in the background.
fn start(app: &mut CadApp, how: CloudJoin, target: &str) {
    let p = params(app);
    let (url, title) = match how {
        CloudJoin::Share => (format!("{}/new", cloud::cloud_server(&p)), target.trim().to_string()),
        CloudJoin::Open => match cloud::parse_link(target) {
            Ok((url, _)) => (url, String::new()),
            Err(e) => {
                app.cloud.error = Some(e.to_string());
                return;
            }
        },
    };
    // Already in that file: switching to its tab needs no network.
    if how == CloudJoin::Open && app.session.collab_url() == Some(url.as_str()) {
        let _ = app.run("cloud.open", json!({ "link": target }));
        app.cloud.open = false;
        return;
    }
    let user = cloud::cloud_user(&p);
    let to = url.clone();
    app.cloud.error = None;
    app.cloud.notice = None;
    app.cloud.busy = Some(Busy::Connect { how, url, user, title, link: background(move || cloud::cloud_connect(&to).map_err(|e| e.to_string())) });
}

/// Put the shared tab's file at the top of "My cloud files".
fn remember(app: &mut CadApp) {
    let (Some(link), Some(title)) = (cloud::current_link(&app.session), app.session.collab_title().map(str::to_string)) else { return };
    let st = &mut app.cloud;
    if st.recent.first().is_some_and(|r| r.link == link && r.title == title) {
        return;
    }
    st.recent.retain(|r| r.link != link);
    st.recent.insert(0, Recent { link, title, opened: now_secs() });
    st.recent.truncate(MAX_RECENT);
}

fn now_secs() -> u64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Move the network work along and send presence; called every frame, with the dialog open or not.
pub fn tick(app: &mut CadApp, ctx: &egui::Context) {
    app.session.collab_presence(app.cloud.cursor, ctx.input(|i| i.time));
    remember(app);
    let Some(busy) = app.cloud.busy.take() else { return };
    ctx.request_repaint_after(std::time::Duration::from_millis(50));
    let now = ctx.input(|i| i.time);
    app.cloud.busy = match busy {
        #[cfg(not(target_arch = "wasm32"))]
        Busy::Status(rx) => match rx.try_recv() {
            Ok(Ok(files)) => {
                app.cloud.people = files
                    .iter()
                    .filter_map(|f| {
                        let link = f.get("link")?.as_str()?.to_string();
                        let people = f.get("people")?.as_array()?.iter().filter_map(Value::as_str).map(str::to_string).collect();
                        Some((link, people))
                    })
                    .collect();
                None
            }
            Ok(Err(_)) | Err(TryRecvError::Disconnected) => None,
            Err(TryRecvError::Empty) => Some(Busy::Status(rx)),
        },
        Busy::Connect { how, url, user, title, link } => match link.try_recv() {
            Ok(Ok(link)) => match cloud::cloud_begin(&mut app.session, how, link, &url, &user, &title) {
                Ok(()) => Some(Busy::Join { how, since: now }),
                Err(e) => {
                    app.cloud.error = Some(e.to_string());
                    None
                }
            },
            Ok(Err(e)) => {
                app.cloud.error = Some(e);
                None
            }
            Err(TryRecvError::Empty) => Some(Busy::Connect { how, url, user, title, link }),
            Err(TryRecvError::Disconnected) => None,
        },
        Busy::Join { how, since } => {
            if app.session.collab_joined() && cloud::current_link(&app.session).is_some() {
                cloud::cloud_joined(&mut app.session, how);
                app.canvas.list = None;
                remember(app);
                // After sharing, the dialog stays open on the link to copy.
                app.cloud.open = how == CloudJoin::Share;
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
                Some(Busy::Join { how, since })
            }
        }
    };
}

/// "3 min ago" for a time in seconds since 1970.
fn ago(t: u64) -> String {
    match now_secs().saturating_sub(t) {
        0..60 => crate::tl!("just now").to_string(),
        s @ 60..3600 => format!("{} min ago", s / 60),
        s @ 3600..86_400 => format!("{} h ago", s / 3600),
        s => format!("{} days ago", s / 86_400),
    }
}

/// Draw the dialog while open and act on what was chosen.
pub fn show(app: &mut CadApp, ctx: &egui::Context) {
    if !app.cloud.open {
        return;
    }
    let shared = app.session.collab_live();
    let link = cloud::current_link(&app.session);
    let title = app.session.collab_title().unwrap_or_default().to_string();
    let has_doc = app.session.state().is_ok();
    let others = app.session.collab_others();
    let following = app.session.collab_following();
    let mut follow: Option<Option<u32>> = None;
    let mut open = true;
    let mut action = None;
    let st = &mut app.cloud;
    let joining = st.busy.as_ref().and_then(Busy::joining);
    let idle = joining.is_none();
    egui::Window::new(crate::tl!("Cloud Files")).open(&mut open).collapsible(false).default_width(520.0).show(ctx, |ui| {
        // The shared drawing's link, or sharing the open one.
        match &link {
            Some(link) => {
                ui.label(egui::RichText::new(format!("{} \u{201c}{title}\u{201d}", crate::tl!("Shared:"))).strong());
                ui.horizontal(|ui| {
                    let mut text = link.clone();
                    ui.add(egui::TextEdit::singleline(&mut text).desired_width(360.0).font(egui::TextStyle::Monospace));
                    if ui.button(crate::tl!("Copy Link")).clicked() {
                        ui.ctx().copy_text(link.clone());
                        st.notice = Some(crate::tl!("Link copied: anyone with it can open and edit this drawing.").to_string());
                    }
                    if ui
                        .button(crate::tl!("Stop Sharing"))
                        .on_hover_text(crate::tl!("This tab becomes a local copy; the file stays in the cloud"))
                        .clicked()
                    {
                        action = Some(Action::Stop);
                    }
                });
            }
            None => {
                ui.label(egui::RichText::new(crate::tl!("Collaborate on the open drawing")).strong());
                ui.horizontal(|ui| {
                    ui.label(crate::tl!("Title"));
                    ui.add(egui::TextEdit::singleline(&mut st.share_title).desired_width(260.0));
                    let can = has_doc && !shared && idle && !st.share_title.trim().is_empty();
                    if ui.add_enabled(can, egui::Button::new(crate::tl!("Collaborate"))).clicked() {
                        action = Some(Action::Share(st.share_title.trim().to_string()));
                    }
                });
            }
        }
        if let Some(how) = joining {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(if how == CloudJoin::Share { crate::tl!("Putting the drawing in the cloud…") } else { crate::tl!("Opening…") });
            });
        }
        if let Some(e) = &st.error {
            ui.colored_label(Tokens::get().warn, e);
        }
        if let Some(n) = &st.notice {
            ui.label(n);
        }
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
        ui.label(egui::RichText::new(crate::tl!("Open a link")).strong());
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut st.link_input).hint_text("https://…/s/…").desired_width(400.0));
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let can = idle && !st.link_input.trim().is_empty();
            if ui.add_enabled(can, egui::Button::new(crate::tl!("Open"))).clicked() || (enter && can) {
                action = Some(Action::Open(st.link_input.trim().to_string()));
            }
        });
        ui.separator();
        ui.label(egui::RichText::new(crate::tl!("My cloud files")).strong());
        egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
            if st.recent.is_empty() {
                ui.label(crate::tl!("Files you share or open appear here."));
            }
            egui::Grid::new("cloud_files").num_columns(4).striped(true).spacing([12.0, 6.0]).show(ui, |ui| {
                for r in &st.recent {
                    let name = if r.title.is_empty() { crate::tl!("(untitled)").to_string() } else { r.title.clone() };
                    ui.label(egui::RichText::new(name).strong()).on_hover_text(&r.link);
                    let people = st.people.get(&r.link).filter(|p| !p.is_empty()).map(|p| format!("{} editing", p.join(", "))).unwrap_or_default();
                    ui.label(people);
                    ui.label(ago(r.opened));
                    ui.horizontal(|ui| {
                        if ui.add_enabled(idle, egui::Button::new(crate::tl!("Open"))).clicked() {
                            action = Some(Action::Open(r.link.clone()));
                        }
                        if ui.button(crate::tl!("Copy Link")).clicked() {
                            ui.ctx().copy_text(r.link.clone());
                        }
                        if ui
                            .small_button(crate::tl!("Remove"))
                            .on_hover_text(crate::tl!("Remove from this list (the file stays in the cloud)"))
                            .clicked()
                        {
                            action = Some(Action::Forget(r.link.clone()));
                        }
                    });
                    ui.end_row();
                }
            });
        });
        ui.separator();
        egui::CollapsingHeader::new(crate::tl!("Settings")).show(ui, |ui| {
            egui::Grid::new("cloud_conn").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label(crate::tl!("Server"));
                ui.add(egui::TextEdit::singleline(&mut st.server).hint_text(cloud::DEFAULT_SERVER).desired_width(300.0));
                ui.end_row();
                ui.label(crate::tl!("Your name"));
                ui.add(egui::TextEdit::singleline(&mut st.user).desired_width(300.0));
                ui.end_row();
            });
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
        Some(Action::Share(title)) => start(app, CloudJoin::Share, &title),
        Some(Action::Open(link)) => start(app, CloudJoin::Open, &link),
        Some(Action::Forget(link)) => app.cloud.recent.retain(|r| r.link != link),
        Some(Action::Stop) => {
            let _ = app.run("collab.leave", json!({}));
        }
    }
}

/// The colour of client `client` in this session.
fn colour(client: u32) -> Color32 {
    let p = Tokens::get().presence;
    p.get(client as usize % p.len()).copied().unwrap_or(Color32::GRAY)
}

fn layout_of(space: &cadcraft_doc::Space) -> String {
    match space {
        cadcraft_doc::Space::Model => String::new(),
        cadcraft_doc::Space::Paper(n) => n.clone(),
    }
}

/// The others' rubber bands on the shared tab, as objects in their colour.
pub fn previews(app: &CadApp) -> Vec<Vec<cadcraft_doc::Entity>> {
    let Ok(st) = app.session.state() else { return Vec::new() };
    if app.session.collab_uid() != Some(st.uid) {
        return Vec::new();
    }
    let layout = layout_of(&st.space);
    app.session
        .collab_others()
        .into_iter()
        .filter(|(_, p)| p.layout == layout && !p.preview.is_empty())
        .map(|(peer, p)| {
            let c = colour(peer.client);
            p.preview
                .iter()
                .filter_map(|j| serde_json::from_value::<cadcraft_doc::Entity>(j.0.clone()).ok())
                .map(|mut e| {
                    e.common.color = cadcraft_color::Color::True(cadcraft_color::Rgb(c.r(), c.g(), c.b()));
                    e
                })
                .collect()
        })
        .collect()
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
    let layout = layout_of(&st.space);
    let painter = painter.with_clip_rect(rect);
    let Some(shown) = st.doc.space(&st.space) else { return };
    for (peer, p) in app.session.collab_others().iter().filter(|(_, p)| p.layout == layout) {
        let c = colour(peer.client);
        for h in p.selection.iter().take(MAX_OUTLINES) {
            // Only objects of the space shown: a layout tab shows model objects through viewports.
            let Some(e) = shown.get(*h) else { continue };
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

/// The status bar's cloud label for the active tab ("Cloud: Floor plan · 2 here"),
/// right-aligned at `right`; clicking it opens the dialog with the link. Returns the left edge
/// for the next item.
pub fn status(app: &mut CadApp, ui: &mut egui::Ui, right: f32, cy: f32) -> f32 {
    if app.session.collab_url().is_none() {
        return right;
    }
    let active = app.session.state().map(|st| st.uid).ok();
    if active.is_none() || active != app.session.collab_uid() {
        return right;
    }
    let name = app.session.collab_title().filter(|t| !t.is_empty()).map_or_else(|| crate::tl!("shared drawing").to_string(), str::to_string);
    let people = app.session.collab_status().map_or(0, |(_, p, _)| p.len());
    let lead = app.session.collab_following().and_then(|c| app.session.collab_others().into_iter().find(|(p, _)| p.client == c)).map(|(p, _)| p.name);
    let text = match lead {
        _ if app.session.collab_reconnecting() => format!("Cloud: {name} · reconnecting…"),
        Some(lead) => format!("Cloud: {name} · {people} here · following {lead}"),
        None => format!("Cloud: {name} · {people} here"),
    };
    let t = Tokens::get();
    let p = ui.painter().clone();
    let g = p.layout_no_wrap(text, crate::theme::small(), t.text);
    let w = g.size().x.min(260.0) + 12.0;
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
    use super::{Recent, State, load_prefs, save_prefs};

    #[test]
    fn my_cloud_files_survive_a_restart() {
        let mut st = State::default();
        st.recent.push(Recent { link: "https://cad.example.com/s/Ab".into(), title: "Floor plan".into(), opened: 5 });
        let mut prefs = serde_json::json!({});
        save_prefs(&st, &mut prefs);
        let mut back = State::default();
        load_prefs(&mut back, &prefs);
        assert_eq!(back.recent, st.recent);
    }
}
