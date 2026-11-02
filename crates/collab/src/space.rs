//! Client handle spaces (decision D5): in a session each client makes handles
//! `client << 32 | counter`, so two people never make the same one. Space 0 holds the handles of
//! files from before (and of anyone outside a session); the server hands out clients from 1.

use cadcraft_doc::{Drawing, Handle};

use crate::container::stores;

/// The client space a handle lies in.
pub fn space_of(h: Handle) -> u32 {
    u32::try_from(h.0 >> 32).unwrap_or(u32::MAX)
}

/// Make `d` allocate new handles in `client`'s space, after every handle of that space it already
/// holds (a client that rejoins continues where it was).
pub fn enter(d: &mut Drawing, client: u32) {
    let base = u64::from(client) << 32;
    let used = stores(d)
        .into_iter()
        .flat_map(|(_, s)| s.iter().map(|e| e.handle))
        .chain(d.foreign.objects.iter().map(|o| o.handle))
        .filter(|h| space_of(*h) == client)
        .map(|h| h.0)
        .max();
    d.handseed = used.map_or(base.saturating_add(1), |m| m.saturating_add(1));
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadcraft_doc::{Entity, EntityKind, Point};

    fn point(h: u64) -> Entity {
        Entity::new(Handle(h), EntityKind::Point(Point { p: Default::default(), angle: 0.0 }))
    }

    #[test]
    fn clients_allocate_in_their_own_space() {
        let mut d = Drawing::default();
        d.model.push(point(0x20));
        enter(&mut d, 3);
        let h = d.new_handle();
        assert_eq!((space_of(h), h.0 & 0xffff_ffff), (3, 1));
        d.model.push(point(h.0));
        enter(&mut d, 3);
        assert_eq!(d.new_handle().0, h.0 + 1);
        enter(&mut d, 4);
        assert_eq!(space_of(d.new_handle()), 4);
    }
}
