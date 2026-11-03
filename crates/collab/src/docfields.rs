//! Drawing-level fields: compared after a command. A table of named entries (layers, styles,
//! views…) syncs entry by entry ([`Change::Entry`]), so two people changing different layers
//! keep both changes; anything else, or a table change entries can't express (a reordering),
//! is written whole as one `Doc` change.
//! Entity collections, layouts, blocks and drawing variables have their own changes; the
//! snapshot's field list ([`crate::snapshot`]) is what makes sure no field is forgotten.

use std::collections::BTreeSet;

use cadcraft_doc::{Drawing, Layout};
use serde_json::Value;

use crate::{Change, Error, Json, Result};

macro_rules! doc_fields {
    ($($f:ident),* $(,)?) => {
        /// `Doc` changes for the whole fields that differ.
        pub(crate) fn diff(a: &Drawing, b: &Drawing, out: &mut Vec<Change>) -> Result<()> {
            $(
                if a.$f != b.$f {
                    let (va, vb) = (Json::of(&a.$f)?, Json::of(&b.$f)?);
                    match entries(stringify!($f), &va.0, &vb.0) {
                        Some(changes) => out.extend(changes),
                        None => out.push(Change::Doc { field: stringify!($f).into(), value: vb }),
                    }
                }
            )*
            Ok(())
        }

        /// The JSON value of one field.
        fn value(d: &Drawing, field: &str) -> Result<Json> {
            match field {
                $(stringify!($f) => Json::of(&d.$f),)*
                _ => Err(Error::Invalid(format!("unknown drawing field `{field}`"))),
            }
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

/// The name of a table entry: its `name` field.
fn name_of(v: &Value) -> Option<&str> {
    v.get("name").and_then(Value::as_str)
}

/// Entries of a table: `None` unless it is an array of objects with unique names.
fn named(v: &Value) -> Option<(&[Value], BTreeSet<&str>)> {
    let items = v.as_array()?;
    let names: BTreeSet<&str> = items.iter().map(name_of).collect::<Option<_>>()?;
    (names.len() == items.len()).then_some((items.as_slice(), names))
}

/// The entry changes that turn table `a` into `b`, or `None` when they can't (not a table of
/// named entries, or the order changed beyond what in-place renames and additions at the end
/// give).
fn entries(field: &str, a: &Value, b: &Value) -> Option<Vec<Change>> {
    let ((ia, na), (ib, nb)) = (named(a)?, named(b)?);
    let entry = |name: &str, value: Option<&Value>| Change::Entry { field: field.into(), name: name.into(), value: value.cloned().map(Json) };
    let mut out = Vec::new();
    let mut renamed = BTreeSet::new();
    for (i, x) in ia.iter().enumerate() {
        let from = name_of(x)?;
        if nb.contains(from) {
            continue;
        }
        // The entry at the same place has a name `a` doesn't: a rename.
        match ib.get(i).filter(|y| name_of(y).is_some_and(|n| !na.contains(n))) {
            Some(y) => {
                renamed.insert(name_of(y)?);
                out.push(entry(from, Some(y)));
            }
            None => out.push(entry(from, None)),
        }
    }
    for y in ib {
        let n = name_of(y)?;
        match ia.iter().find(|x| name_of(x) == Some(n)) {
            Some(x) if x == y => {}
            Some(_) => out.push(entry(n, Some(y))),
            None if renamed.contains(n) => {}
            None => out.push(entry(n, Some(y))),
        }
    }
    let mut check = a.clone();
    for c in &out {
        if let Change::Entry { name, value, .. } = c {
            set_entry(&mut check, name, value.as_ref().map(|j| &j.0)).ok()?;
        }
    }
    (check == *b).then_some(out)
}

/// Replace, rename, add or remove entry `name` of table `t`. A rename onto a name another entry
/// has (someone made it meanwhile) replaces that entry, so names stay unique.
fn set_entry(t: &mut Value, name: &str, value: Option<&Value>) -> Result<()> {
    let items = t.as_array_mut().ok_or_else(|| Error::Invalid("not a table".into()))?;
    let at = items.iter().position(|x| name_of(x) == Some(name));
    let Some(v) = value else {
        if let Some(i) = at {
            items.remove(i);
        }
        return Ok(());
    };
    let new = name_of(v).ok_or_else(|| Error::Invalid("a table entry has no name".into()))?;
    if new != name
        && let Some(j) = items.iter().position(|x| name_of(x) == Some(new))
    {
        items.remove(j);
    }
    match items.iter().position(|x| name_of(x) == Some(name)) {
        Some(i) => {
            if let Some(slot) = items.get_mut(i) {
                *slot = v.clone();
            }
        }
        None => items.push(v.clone()),
    }
    Ok(())
}

/// Apply one entry change.
pub(crate) fn apply_entry(d: &mut Drawing, field: &str, name: &str, v: Option<&Json>) -> Result<()> {
    let mut t = value(d, field)?;
    set_entry(&mut t.0, name, v.map(|j| &j.0))?;
    apply(d, field, &t)
}
