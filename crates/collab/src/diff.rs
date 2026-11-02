//! Diffing: the changes that turn a drawing into an edited version of it (decisions D3, D4).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use cadcraft_doc::{Drawing, Entity, EntityStore, Handle};

use crate::container::{store, stores};
use crate::{Change, Container, Json, Result, docfields, vars};

/// Where an entity is and what it is, on one side of an edit.
type Side = BTreeMap<Handle, (Container, Arc<Entity>)>;

/// The changes that turn `before` into `after` (an edited clone of it), in the order
/// [`apply`](crate::apply) needs. Collections both drawings still share are skipped, so the cost
/// follows the size of the edit. Per-person variables ([`vars::is_local`]), the handle seed and
/// session-only data (hidden objects, REFEDIT, read notes) are never in it.
pub fn diff(before: &Drawing, after: &Drawing) -> Result<Vec<Change>> {
    let (old, new, reordered) = touched(before, after);
    let mut out = Vec::new();
    // Deletions first: an entity that changes collection leaves before it arrives.
    for (h, (c, _)) in &old {
        if new.get(h).is_none_or(|(nc, _)| nc != c) {
            out.push(Change::Delete { id: *h });
        }
    }
    blocks(before, after, &mut out, true)?;
    let layouts = docfields::layouts_value(after)?;
    if docfields::layouts_value(before)? != layouts {
        out.push(Change::Doc { field: docfields::LAYOUTS.into(), value: layouts });
    }
    entities(&old, &new, &mut out)?;
    for c in reordered {
        if let Some(s) = store(after, &c) {
            out.push(Change::Order { container: c, ids: s.handles() });
        }
    }
    blocks(before, after, &mut out, false)?;
    header(before, after, &mut out)?;
    docfields::diff(before, after, &mut out)?;
    Ok(out)
}

/// The entities that changed on each side, and the collections whose draw order changed.
fn touched(before: &Drawing, after: &Drawing) -> (Side, Side, Vec<Container>) {
    let empty = EntityStore::new();
    let (mut old, mut new, mut reordered) = (Side::new(), Side::new(), Vec::new());
    let a = stores(after);
    let gone = stores(before).into_iter().filter(|(c, _)| !a.iter().any(|(ac, _)| ac == c));
    for (c, s) in gone {
        old.extend(s.iter().map(|e| (e.handle, (c.clone(), e.clone()))));
    }
    for (c, s) in a {
        let b = store(before, &c).unwrap_or(&empty);
        if s.same_as(b) {
            continue;
        }
        for h in s.changed_since(b) {
            if let Some(e) = b.get(h) {
                old.insert(h, (c.clone(), e.clone()));
            }
            if let Some(e) = s.get(h) {
                new.insert(h, (c.clone(), e.clone()));
            }
        }
        if s.handles() != applied_order(b, s) {
            reordered.push(c);
        }
    }
    (old, new, reordered)
}

/// The draw order `apply` gives a collection without an `Order` change: what stayed, in its old
/// order, then what arrived, in handle order (the order of the `Create` changes).
fn applied_order(before: &EntityStore, after: &EntityStore) -> Vec<Handle> {
    let mut out: Vec<Handle> = before.handles().into_iter().filter(|h| after.contains(*h)).collect();
    let mut arrived: Vec<Handle> = after.handles().into_iter().filter(|h| !before.contains(*h)).collect();
    arrived.sort();
    out.extend(arrived);
    out
}

/// Creations and property writes.
fn entities(old: &Side, new: &Side, out: &mut Vec<Change>) -> Result<()> {
    for (h, (c, e)) in new {
        match old.get(h) {
            Some((oc, o)) if oc == c => properties(o, e, out)?,
            _ => out.push(Change::Create { id: *h, container: c.clone(), entity: Json::of(&**e)? }),
        }
    }
    Ok(())
}

/// One `Common` change per field that differs, and the geometry whole when it differs.
fn properties(a: &Entity, b: &Entity, out: &mut Vec<Change>) -> Result<()> {
    if a.common != b.common {
        let (ca, cb) = (fields(&a.common)?, fields(&b.common)?);
        let keys: BTreeSet<&String> = ca.keys().chain(cb.keys()).collect();
        for k in keys {
            let v = cb.get(k);
            if ca.get(k) != v {
                out.push(Change::Common { id: b.handle, prop: k.clone(), value: v.cloned().map(Json) });
            }
        }
    }
    if a.kind != b.kind {
        out.push(Change::Geometry { id: b.handle, value: Json::of(&b.kind)? });
    }
    Ok(())
}

/// A value's JSON fields.
pub(crate) fn fields<T: serde::Serialize>(v: &T) -> Result<serde_json::Map<String, serde_json::Value>> {
    match Json::of(v)?.0 {
        serde_json::Value::Object(m) => Ok(m),
        _ => Ok(serde_json::Map::new()),
    }
}

/// Block definitions' own data: new and changed ones (`puts`), or removed ones.
fn blocks(before: &Drawing, after: &Drawing, out: &mut Vec<Change>, puts: bool) -> Result<()> {
    if puts {
        for (name, b) in &after.blocks {
            let old = before.blocks.get(name);
            if old.is_some_and(|o| Arc::ptr_eq(o, b)) {
                continue;
            }
            let value = Json::of(&**b)?;
            if old.map(|o| Json::of(&**o)).transpose()?.as_ref() != Some(&value) {
                out.push(Change::Block { name: name.clone(), value: Some(value) });
            }
        }
    } else {
        for name in before.blocks.keys().filter(|n| !after.blocks.contains_key(*n)) {
            out.push(Change::Block { name: name.clone(), value: None });
        }
    }
    Ok(())
}

/// Shared drawing variables that changed.
fn header(before: &Drawing, after: &Drawing, out: &mut Vec<Change>) -> Result<()> {
    let (a, b) = (&before.header.vars, &after.header.vars);
    if a == b {
        return Ok(());
    }
    let names: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    for n in names.into_iter().filter(|n| !vars::is_local(n)) {
        let v = b.get(n);
        if a.get(n) != v {
            out.push(Change::Var { name: n.clone(), value: v.map(Json::of).transpose()? });
        }
    }
    Ok(())
}
