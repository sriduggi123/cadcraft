//! Server snapshots (decision D13): a whole drawing as a postcard envelope with the drawing as JSON
//! text inside it. JSON keeps field names, so a snapshot keeps loading after entity types gain
//! fields with defaults; the envelope carries a format number for real breaks.
//!
//! [`body`] names every field of [`Drawing`] without `..`, so a field added upstream stops the
//! build here until it is either synced or marked session-only (and then [`crate::diff`] and
//! `docfields` need it too).

use std::sync::Arc;

use cadcraft_doc::foreign::Foreign;
use cadcraft_doc::{
    AnnoScale, Block, Constraint, DimStyle, Drawing, Entity, EntityStore, Group, Header, Layer, LayerState, Layout, Linetype, MLeaderStyle,
    MLineStyle, NamedView, Parametric, PlotStyleTable, TableStyle, TextStyle, Ucs, VportConfig,
};
use serde::{Deserialize, Serialize};

use crate::{Error, Json, Result};

/// The envelope format this build writes and reads.
pub const FORMAT: u32 = 1;

/// Largest snapshot accepted.
pub const MAX_BYTES: usize = 1 << 30;

#[derive(Serialize, Deserialize)]
struct Envelope {
    format: u32,
    body: Json,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Body {
    header: Header,
    foreign: Foreign,
    layers: Vec<Layer>,
    linetypes: Vec<Linetype>,
    text_styles: Vec<TextStyle>,
    dim_styles: Vec<DimStyle>,
    mleader_styles: Vec<MLeaderStyle>,
    table_styles: Vec<TableStyle>,
    mline_styles: Vec<MLineStyle>,
    blocks: Vec<Contents<Block>>,
    model: Vec<Entity>,
    layouts: Vec<Contents<Layout>>,
    active_layout: Option<String>,
    views: Vec<NamedView>,
    vports: Vec<VportConfig>,
    ucss: Vec<Ucs>,
    layer_states: Vec<LayerState>,
    groups: Vec<Group>,
    plot_style_tables: Vec<PlotStyleTable>,
    constraints: Vec<Constraint>,
    parametric: Parametric,
    scales: Vec<AnnoScale>,
    handseed: u64,
}

/// A block or layout with its entities, which its own serde form leaves out.
#[derive(Serialize, Deserialize)]
struct Contents<T> {
    def: T,
    entities: Vec<Entity>,
}

fn entities(s: &EntityStore) -> Vec<Entity> {
    s.iter().map(|e| (**e).clone()).collect()
}

fn store(v: Vec<Entity>) -> EntityStore {
    let mut s = EntityStore::new();
    v.into_iter().for_each(|e| s.push(e));
    s
}

fn body(d: &Drawing) -> Body {
    let Drawing {
        header,
        foreign,
        layers,
        linetypes,
        text_styles,
        dim_styles,
        mleader_styles,
        table_styles,
        mline_styles,
        blocks,
        model,
        layouts,
        active_layout,
        views,
        vports,
        ucss,
        layer_states,
        groups,
        plot_style_tables,
        constraints,
        parametric,
        scales,
        handseed,
        // Session-only: never synced or stored.
        hidden_objects: _,
        read_notes: _,
        refedit: _,
        skipped_records: _,
        portable_fonts: _,
    } = d;
    Body {
        header: header.clone(),
        foreign: (**foreign).clone(),
        layers: layers.clone(),
        linetypes: linetypes.clone(),
        text_styles: text_styles.clone(),
        dim_styles: dim_styles.clone(),
        mleader_styles: mleader_styles.clone(),
        table_styles: table_styles.clone(),
        mline_styles: mline_styles.clone(),
        blocks: blocks.values().map(|b| Contents { def: (**b).clone(), entities: entities(&b.entities) }).collect(),
        model: entities(model),
        layouts: layouts.iter().map(|l| Contents { def: l.clone(), entities: entities(&l.entities) }).collect(),
        active_layout: active_layout.clone(),
        views: views.clone(),
        vports: vports.clone(),
        ucss: ucss.clone(),
        layer_states: layer_states.clone(),
        groups: groups.clone(),
        plot_style_tables: plot_style_tables.clone(),
        constraints: constraints.clone(),
        parametric: parametric.clone(),
        scales: scales.clone(),
        handseed: *handseed,
    }
}

fn drawing(b: Body) -> Drawing {
    Drawing {
        header: b.header,
        foreign: Arc::new(b.foreign),
        layers: b.layers,
        linetypes: b.linetypes,
        text_styles: b.text_styles,
        dim_styles: b.dim_styles,
        mleader_styles: b.mleader_styles,
        table_styles: b.table_styles,
        mline_styles: b.mline_styles,
        blocks: b
            .blocks
            .into_iter()
            .map(|c| {
                let mut def = c.def;
                def.entities = store(c.entities);
                (def.name.clone(), Arc::new(def))
            })
            .collect(),
        model: store(b.model),
        layouts: b
            .layouts
            .into_iter()
            .map(|c| {
                let mut def = c.def;
                def.entities = store(c.entities);
                def
            })
            .collect(),
        active_layout: b.active_layout,
        views: b.views,
        vports: b.vports,
        ucss: b.ucss,
        layer_states: b.layer_states,
        groups: b.groups,
        plot_style_tables: b.plot_style_tables,
        constraints: b.constraints,
        parametric: b.parametric,
        scales: b.scales,
        handseed: b.handseed,
        hidden_objects: Default::default(),
        read_notes: Vec::new(),
        refedit: None,
        skipped_records: Default::default(),
        portable_fonts: false,
    }
}

/// Encode `d` (without its session-only data).
pub fn encode(d: &Drawing) -> Result<Vec<u8>> {
    let env = Envelope { format: FORMAT, body: Json::of(&body(d))? };
    postcard::to_allocvec(&env).map_err(|e| Error::Encode(e.to_string()))
}

/// Decode a snapshot [`encode`] wrote.
pub fn decode(bytes: &[u8]) -> Result<Drawing> {
    if bytes.len() > MAX_BYTES {
        return Err(Error::TooLarge(MAX_BYTES));
    }
    let env: Envelope = postcard::from_bytes(bytes).map_err(|e| Error::Decode(e.to_string()))?;
    if env.format != FORMAT {
        return Err(Error::Decode(format!("snapshot format {} (this build reads {FORMAT})", env.format)));
    }
    env.body.read("snapshot").map(drawing)
}
