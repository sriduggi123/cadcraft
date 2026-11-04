---
name: draw-from-description
when: The user describes something to draw in words: a plate with holes, a room, a bracket, a profile.
---
# Draw from a description

1. Units: the drawing's `insunits` (in the message's drawing summary). If the user's sizes use
   other units, convert; if units are unclear, `ask_user`.
2. Missing sizes that matter: ask once with `ask_user`, offering sensible defaults. Don't ask
   about sizes that don't change the result.
3. Where: an empty area next to existing geometry (from the extents), or `pick_points` when the
   user says "here".
4. Plan the geometry as coordinates before drawing: list outline points, hole centres, arcs.
   Build from simple pieces:
   - outlines: `pline {vertices, closed: true}` or `rectang {p1, dimensions, fillet}`
   - holes: `circle {center, diameter}`; patterns of holes: draw one, then `arrayrect` or
     `arraypolar`, or one `run_lisp` loop
   - rounded corners on a polyline: `fillet {handle, polyline: true, radius}`
   - walls: `pline` for the centre line, then `offset` both ways
5. Layers: outlines on the current or an `Outline`/`Visible` layer, centre lines on `Center`
   if it exists.
6. `render` the result and compare with the description; fix and say what was drawn with its
   main sizes.
