//! Per-person undo (P4): take back one person's step in a drawing others have edited since. The
//! step is the drawing before and after it; what it changed is reversed, except what someone has
//! changed since, which stays as they left it. The result is an ordinary edit, sent as a batch.

use std::collections::BTreeSet;

use cadcraft_doc::{Drawing, Handle};

use crate::{Change, Container, Result, diff};

/// What a change writes: two changes to the same key overwrite each other.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Key {
    Entity(Handle),
    Prop(Handle, String),
    Geometry(Handle),
    Order(Container),
    Block(String),
    Var(String),
    Doc(String),
    Entry(String, String),
}

pub(crate) fn key(c: &Change) -> Key {
    match c {
        Change::Delete { id } | Change::Create { id, .. } => Key::Entity(*id),
        Change::Common { id, prop, .. } => Key::Prop(*id, prop.clone()),
        Change::Geometry { id, .. } => Key::Geometry(*id),
        Change::Order { container, .. } => Key::Order(container.clone()),
        Change::Block { name, .. } => Key::Block(name.clone()),
        Change::Var { name, .. } => Key::Var(name.clone()),
        Change::Doc { field, .. } => Key::Doc(field.clone()),
        Change::Entry { field, name, .. } => Key::Entry(field.clone(), name.clone()),
    }
}

/// Take back the step that turned `before` into `after`, in `now` (the drawing shown, which may
/// have others' edits since). Returns the drawing and how many of the step's changes were left
/// because someone changed the same thing since.
pub fn revert(now: &Drawing, before: &Drawing, after: &Drawing) -> Result<(Drawing, usize)> {
    let back = diff(after, before)?;
    let since: BTreeSet<Key> = diff(after, now)?.iter().map(key).collect();
    let (keep, left): (Vec<Change>, Vec<Change>) = back.into_iter().partition(|c| !since.contains(&key(c)));
    let mut d = now.clone();
    crate::apply::apply_lenient(&mut d, &keep);
    Ok((d, left.len()))
}
