//! Applying a batch of changes to a drawing.

use std::sync::Arc;

use cadcraft_doc::{Block, Common, Drawing, Entity, EntityKind, EntityStore, HVal, Handle};

use crate::container::{locate, store_mut};
use crate::diff::fields;
use crate::{Change, Container, Error, Json, Result, docfields, vars};

/// Apply `changes` to `d` in order. On an error `d` may hold part of the batch: callers apply to
/// a clone and keep it only when the whole batch applies. Per-person variables in a batch are
/// ignored (a client never sends them).
pub fn apply(d: &mut Drawing, changes: &[Change]) -> Result<()> {
    for c in changes {
        apply_one(d, c)?;
    }
    Ok(())
}

fn apply_one(d: &mut Drawing, c: &Change) -> Result<()> {
    match c {
        Change::Delete { id } => delete(d, *id),
        Change::Block { name, value } => block(d, name, value.as_ref()),
        Change::Create { id, container, entity } => create(d, *id, container, entity),
        Change::Common { id, prop, value } => common(d, *id, prop, value.as_ref()),
        Change::Geometry { id, value } => {
            let kind: EntityKind = value.read("geometry")?;
            modify(d, *id, |e| e.kind = kind)
        }
        Change::Order { container, ids } => order(d, container, ids),
        Change::Var { name, value } => var(d, name, value.as_ref()),
        Change::Doc { field, value } => docfields::apply(d, field, value),
    }
}

fn delete(d: &mut Drawing, id: Handle) -> Result<()> {
    let c = locate(d, id).ok_or(Error::Unknown(id))?;
    store_mut(d, &c).and_then(|s| s.remove(id)).map(|_| ()).ok_or(Error::Unknown(id))
}

fn block(d: &mut Drawing, name: &str, value: Option<&Json>) -> Result<()> {
    let Some(v) = value else {
        d.blocks.remove(name);
        return Ok(());
    };
    let mut b: Block = v.read("block")?;
    if b.name != name {
        return Err(Error::Invalid(format!("block `{}` sent as `{name}`", b.name)));
    }
    if let Some(old) = d.blocks.get(name) {
        b.entities = old.entities.clone();
    }
    d.blocks.insert(name.to_string(), Arc::new(b));
    Ok(())
}

fn create(d: &mut Drawing, id: Handle, container: &Container, entity: &Json) -> Result<()> {
    let e: Entity = entity.read("entity")?;
    if e.handle != id {
        return Err(Error::Invalid(format!("entity {:?} sent as {id:?}", e.handle)));
    }
    if locate(d, id).is_some() {
        return Err(Error::Invalid(format!("{id:?} already exists")));
    }
    let s = store_mut(d, container).ok_or_else(|| Error::Invalid(format!("no collection {container:?}")))?;
    s.push(e);
    Ok(())
}

fn modify(d: &mut Drawing, id: Handle, f: impl FnOnce(&mut Entity)) -> Result<()> {
    let c = locate(d, id).ok_or(Error::Unknown(id))?;
    let done = store_mut(d, &c).is_some_and(|s| s.modify(id, f));
    if done { Ok(()) } else { Err(Error::Unknown(id)) }
}

fn common(d: &mut Drawing, id: Handle, prop: &str, value: Option<&Json>) -> Result<()> {
    let c = locate(d, id).ok_or(Error::Unknown(id))?;
    let e = store_mut(d, &c).and_then(|s| s.get(id).cloned()).ok_or(Error::Unknown(id))?;
    let mut m = fields(&e.common)?;
    match value {
        Some(v) => m.insert(prop.to_string(), v.0.clone()),
        None => m.remove(prop),
    };
    let common: Common = Json(serde_json::Value::Object(m)).read(prop)?;
    modify(d, id, |e| e.common = common)
}

/// Put a collection in the given draw order; it must list exactly the collection's entities.
fn order(d: &mut Drawing, container: &Container, ids: &[Handle]) -> Result<()> {
    let s = store_mut(d, container).ok_or_else(|| Error::Invalid(format!("no collection {container:?}")))?;
    if ids.len() != s.len() {
        return Err(Error::Invalid(format!("draw order lists {} of {} objects", ids.len(), s.len())));
    }
    let mut out = EntityStore::new();
    for h in ids {
        let e = s.get(*h).ok_or(Error::Unknown(*h))?;
        if out.contains(*h) {
            return Err(Error::Invalid(format!("draw order lists {h:?} twice")));
        }
        out.push((**e).clone());
    }
    *s = out;
    Ok(())
}

fn var(d: &mut Drawing, name: &str, value: Option<&Json>) -> Result<()> {
    if vars::is_local(name) {
        return Ok(());
    }
    match value {
        Some(v) => d.header.set(name, v.read::<HVal>(name)?),
        None => {
            d.header.vars.remove(name);
        }
    }
    Ok(())
}
