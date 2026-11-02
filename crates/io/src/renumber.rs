//! Writing a drawing edited in a shared session (fork decision D5). In a session each client makes
//! handles `client << 32 | counter`, far above what AutoCAD makes. [`write_shared`] gives a DXF
//! or DWG small sequential handles instead: every handle the file defines (groups 5 and 105) and
//! every reference to one (320–369, 390–399, 480–481, 1005, per the DXF reference) is renumbered
//! consistently, and `$HANDSEED` and our own seed record follow. Ordinary saves ([`crate::write`])
//! don't renumber: a file's own handles, large ones included (#1773), stay stable (#495).
//!
//! Handles inside binary chunks (proxy graphics, ACIS data) are not rewritten.

use std::collections::HashMap;
use std::path::Path;

use cadcraft_doc::{Drawing, Format};
use cadcraft_dxf::{Tag, Value, Version};

use crate::{IoError, Result, SaveOptions, dxf_write, save_version};

/// Write a drawing from a shared session: like [`crate::write_with`], with its handles renumbered
/// in DXF and DWG output. Other formats carry no handles and are written as usual.
pub fn write_shared(d: &Drawing, name: impl AsRef<Path>, opts: &SaveOptions) -> Result<Vec<u8>> {
    let name = name.as_ref();
    let version = opts.version.unwrap_or_else(|| save_version(d));
    match Format::from_path(name) {
        Some(Format::Dxf) => {
            let mut tags = dxf_write::tags(d, version);
            renumber(&mut tags);
            let code_page = dxf_write::code_page(d, version);
            Ok(match (opts.binary, version.utf8()) {
                (true, true) => cadcraft_dxf::write_binary(&tags, true),
                (true, false) => cadcraft_dxf::write_binary_in(&tags, &code_page),
                (false, true) => cadcraft_dxf::write_ascii_as(&tags, true).into_bytes(),
                (false, false) => cadcraft_dxf::write_ascii_in(&tags, &code_page),
            })
        }
        Some(Format::Dwg) if !opts.binary => {
            let version = opts.version.unwrap_or_else(|| version.max(Version::R2004));
            let mut tags = cadcraft_dxf::parse_ascii(&dxf_write::dwg_bridge(d, version)).map_err(|e| IoError::Format(e.to_string()))?;
            renumber(&mut tags);
            cadcraft_dwg::dxf_to_dwg_as(cadcraft_dxf::write_ascii(&tags).as_bytes(), version.code()).map_err(IoError::Format)
        }
        _ => crate::write_with(d, name, opts),
    }
}

fn is_handle_code(code: i32) -> bool {
    matches!(code, 5 | 105 | 320..=369 | 390..=399 | 480..=481 | 1005)
}

fn handle(t: &Tag) -> Option<u64> {
    u64::from_str_radix(t.str().trim(), 16).ok()
}

/// Whether `tags[i]` is the value of a header variable (`9 $NAME` before it).
fn is_header_value(tags: &[Tag], i: usize, name: &str) -> bool {
    i.checked_sub(1).and_then(|p| tags.get(p)).is_some_and(|t| t.code == 9 && t.str() == name)
}

/// Renumber every handle of `tags` from 1, definitions in file order first.
fn renumber(tags: &mut [Tag]) {
    let refs = |(i, t): (usize, &Tag)| is_handle_code(t.code) && !is_header_value(tags, i, "$HANDSEED");
    let mut map: HashMap<u64, u64> = HashMap::new();
    let mut next: u64 = 1;
    let mut assign = |h: u64, map: &mut HashMap<u64, u64>| {
        if h != 0 && !map.contains_key(&h) {
            map.insert(h, next);
            next = next.saturating_add(1);
        }
    };
    // Definitions first, in file order, then references to handles the file doesn't define.
    for (i, t) in tags.iter().enumerate() {
        if matches!(t.code, 5 | 105) && !is_header_value(tags, i, "$HANDSEED") {
            handle(t).into_iter().for_each(|h| assign(h, &mut map));
        }
    }
    for (i, t) in tags.iter().enumerate() {
        if refs((i, t)) {
            handle(t).into_iter().for_each(|h| assign(h, &mut map));
        }
    }
    let seed = map.len() as u64 + 1;
    rewrite(tags, &map, seed);
}

fn rewrite(tags: &mut [Tag], map: &HashMap<u64, u64>, seed: u64) {
    let seed_record = own_seed_record(tags);
    for i in 0..tags.len() {
        let header_seed = is_header_value(tags, i, "$HANDSEED");
        let Some(t) = tags.get_mut(i) else { continue };
        if header_seed {
            t.value = Value::Str(format!("{seed:X}"));
        } else if is_handle_code(t.code)
            && let Some(n) = handle(t).and_then(|h| map.get(&h))
        {
            t.value = Value::Str(format!("{n:X}"));
        }
    }
    if let Some(t) = seed_record.and_then(|i| tags.get_mut(i)) {
        t.value = Value::Str(format!("{seed:X}"));
    }
}

/// The index of the text of our `CADCRAFT_HANDSEED` record: the XRECORD the root dictionary
/// names with that key, its first group 1.
fn own_seed_record(tags: &[Tag]) -> Option<usize> {
    let key = crate::dxf_ext::HANDSEED_KEY;
    let at = tags.windows(2).position(|w| matches!(w, [a, b] if a.code == 3 && a.str() == key && b.code == 350))?;
    let h = tags.get(at + 1)?.str();
    let start = tags.windows(2).position(|w| matches!(w, [a, b] if a.code == 0 && a.str() == "XRECORD" && b.code == 5 && b.str() == h))?;
    let end = tags.iter().skip(start + 1).position(|t| t.code == 0).map_or(tags.len(), |n| start + 1 + n);
    (start..end).find(|i| tags.get(*i).is_some_and(|t| t.code == 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadcraft_doc::{Entity, EntityKind, Line};
    use cadcraft_geom::Vec3;

    fn session_drawing() -> Drawing {
        let mut d = Drawing { handseed: (7 << 32) | 1, ..Drawing::default() };
        for _ in 0..3 {
            let h = d.new_handle();
            d.model.push(Entity::new(h, EntityKind::Line(Line { a: Vec3::ZERO, b: Vec3::new(1.0, 0.0, 0.0) })));
        }
        d
    }

    fn handles(tags: &[Tag]) -> Vec<u64> {
        tags.iter().filter(|t| is_handle_code(t.code)).filter_map(handle).collect()
    }

    /// Handles at or above this lie in a client space other than 0.
    const SESSION_FLOOR: u64 = 1 << 32;

    #[test]
    fn shared_writes_get_small_handles() {
        let d = session_drawing();
        for name in ["x.dxf", "x.dwg"] {
            let back = crate::read(&write_shared(&d, name, &SaveOptions::default()).unwrap(), name).unwrap();
            assert_eq!(back.model.len(), 3, "{name}");
            assert!(back.handseed < SESSION_FLOOR, "{name}: {:X}", back.handseed);
            assert!(back.model.iter().all(|e| e.handle.0 < back.handseed), "{name}");
        }
        let mut tags = crate::dxf_write::tags(&d, Version::R2018);
        renumber(&mut tags);
        assert!(handles(&tags).iter().all(|h| *h < SESSION_FLOOR), "{:?}", handles(&tags));
    }

    #[test]
    fn ordinary_saves_keep_large_handles() {
        let d = session_drawing();
        let back = crate::read(&crate::write_dxf(&d).into_bytes(), "x.dxf").unwrap();
        assert!(back.model.iter().all(|e| e.handle.0 >= SESSION_FLOOR));
    }
}
