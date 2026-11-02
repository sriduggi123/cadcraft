//! Where entities live: model space, a layout's paper space or a block definition.

use std::sync::Arc;

use cadcraft_doc::{Drawing, EntityStore, Handle};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Container {
    Model,
    Paper { layout: String },
    Block { name: String },
}

/// Every entity collection of `d`, model space first.
pub(crate) fn stores(d: &Drawing) -> Vec<(Container, &EntityStore)> {
    let mut out = vec![(Container::Model, &d.model)];
    out.extend(d.layouts.iter().map(|l| (Container::Paper { layout: l.name.clone() }, &l.entities)));
    out.extend(d.blocks.iter().map(|(n, b)| (Container::Block { name: n.clone() }, &b.entities)));
    out
}

pub(crate) fn store<'a>(d: &'a Drawing, c: &Container) -> Option<&'a EntityStore> {
    match c {
        Container::Model => Some(&d.model),
        Container::Paper { layout } => d.layouts.iter().find(|l| &l.name == layout).map(|l| &l.entities),
        Container::Block { name } => d.blocks.get(name).map(|b| &b.entities),
    }
}

pub(crate) fn store_mut<'a>(d: &'a mut Drawing, c: &Container) -> Option<&'a mut EntityStore> {
    match c {
        Container::Model => Some(&mut d.model),
        Container::Paper { layout } => d.layouts.iter_mut().find(|l| &l.name == layout).map(|l| &mut l.entities),
        Container::Block { name } => d.blocks.get_mut(name).map(|b| &mut Arc::make_mut(b).entities),
    }
}

/// The collection holding `h`.
pub(crate) fn locate(d: &Drawing, h: Handle) -> Option<Container> {
    stores(d).into_iter().find(|(_, s)| s.contains(h)).map(|(c, _)| c)
}
