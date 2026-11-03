//! Changes: the unit of synchronisation. One command's changes form one batch.

use cadcraft_doc::Handle;
use serde::{Deserialize, Serialize};

use crate::{Container, Json};

/// One change to a drawing. A batch from [`diff`](crate::diff) lists them in the order
/// [`apply`](crate::apply) needs: deletions, block and layout definitions, creations, property
/// writes, draw order, block removals, then drawing variables and other drawing-level data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Change {
    /// An entity is gone (or leaves its collection, followed by a `Create` elsewhere).
    Delete { id: Handle },
    /// A block definition's own data (everything but its entities); `None` removes the block.
    Block { name: String, value: Option<Json> },
    /// A new entity, whole, appended to `container` (an `Order` follows when it goes elsewhere).
    Create { id: Handle, container: Container, entity: Json },
    /// One field of an entity's `Common` data (layer, color…); `None` = the field's default.
    Common { id: Handle, prop: String, value: Option<Json> },
    /// An entity's geometry (its `kind`), whole: its parts depend on each other.
    Geometry { id: Handle, value: Json },
    /// The draw order of a collection: every handle in it, bottom first.
    Order { container: Container, ids: Vec<Handle> },
    /// A shared drawing variable; `None` removes it.
    Var { name: String, value: Option<Json> },
    /// A drawing-level field written whole (layouts, and tables that can't go entry by entry).
    Doc { field: String, value: Json },
    /// One named entry of a drawing-level table (a layer, a text style…): `value` replaces the
    /// entry called `name` in place, under its own name (a rename), or is added; `None` removes it.
    Entry { field: String, name: String, value: Option<Json> },
}
