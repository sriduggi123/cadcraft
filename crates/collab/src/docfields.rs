//! Drawing-level fields synced whole: compared after a command, written as one `Doc` change.
//! Entity collections, layouts, blocks and drawing variables have their own changes; the
//! snapshot's field list ([`crate::snapshot`]) is what makes sure no field is forgotten.

use cadcraft_doc::{Drawing, Layout};

use crate::{Change, Error, Json, Result};

macro_rules! doc_fields {
    ($($f:ident),* $(,)?) => {
        /// `Doc` changes for the whole fields that differ.
        pub(crate) fn diff(a: &Drawing, b: &Drawing, out: &mut Vec<Change>) -> Result<()> {
            $(
                if a.$f != b.$f {
                    out.push(Change::Doc { field: stringify!($f).into(), value: Json::of(&b.$f)? });
                }
            )*
            Ok(())
        }

        /// Write one whole field.
        pub(crate) fn apply(d: &mut Drawing, field: &str, v: &Json) -> Result<()> {
            match field {
                $(stringify!($f) => d.$f = v.read(field)?,)*
                LAYOUTS => apply_layouts(d, v)?,
                _ => return Err(Error::Invalid(format!("unknown drawing field `{field}`"))),
            }
            Ok(())
        }
    };
}

doc_fields!(
    foreign,
    layers,
    linetypes,
    text_styles,
    dim_styles,
    mleader_styles,
    table_styles,
    mline_styles,
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
);

/// The `Doc` field holding the layouts' own data (page setups, tab order); their paper-space
/// entities sync as entities.
pub(crate) const LAYOUTS: &str = "layouts";

/// The layouts' own data, without their entities (serde skips them).
pub(crate) fn layouts_value(d: &Drawing) -> Result<Json> {
    Json::of(&d.layouts)
}

/// Replace the layouts' own data; a layout keeps the entities it had under the same name.
fn apply_layouts(d: &mut Drawing, v: &Json) -> Result<()> {
    let mut layouts: Vec<Layout> = v.read(LAYOUTS)?;
    for l in &mut layouts {
        if let Some(old) = d.layouts.iter().find(|o| o.name == l.name) {
            l.entities = old.entities.clone();
        }
    }
    d.layouts = layouts;
    Ok(())
}
