---
name: dimension-a-part
when: The user asks to dimension a part, a view or some objects, or to add sizes.
---
# Dimension a part

1. Find the part: `query_entities` with a window around it (or the selection), and `render` that
   window to see its shape, in the same reply. Skip this for a part drawn in this request: you
   know its coordinates.
2. Dimension style: the current one, unless the user names another (`drawing_summary` lists
   them; `dimstyle.current {name}` switches).
   Settings, as the first steps of the batch (`{"command": "setvar", "params": {"name", "value"}}`):
   - text size: new dimension text is `dimTextHeight` high (in the drawing summary). Aim for
     about 1/40 of the part's largest size (2.5 for a 100-long part). When it is off by more than
     twice, set `DIMSCALE` to that height / DIMTXT, rounded (DIMTXT = dimTextHeight / DIMSCALE).
   - decimals: `DIMDEC` 0 when the sizes are whole numbers, else 1 or 2 (the default 4 makes
     long labels that collide).
   - `DIMTIH` 0 and `DIMTOH` 0: the text follows the dimension line, so vertical dimensions
     stay narrow.
3. Choose a layer for dimensions: an existing one named like `DIM`, `Dimensions` or `Annotation`;
   else make one with `layer.new {name: "Dimensions", color: "green"}`. Make it current with
   `layer.current`, and put the old current layer back at the end.
4. Overall sizes first: one horizontal and one vertical `dimlinear` across the extremes, with
   `{object: handle, at}` when one object spans the size (it stays associative), otherwise
   `{p1, p2, at}` on the end points.
5. Then features:
   - holes and arcs: `dimdiameter {handle, at}` for circles, `dimradius {handle, at}` for arcs
     and fillets; several equal holes get one dimension with text like `4X <>` (`text` param).
   - positions of holes: `dimlinear` from a datum edge to the hole centres; a row of them with
     `dimbaseline {points}` or `dimcontinue {points}` after the first.
   - angles: `dimangular {lines: [h1, h2], at}`.
6. Placement: the first row of dimension lines 3 text heights outside the part, each further
   row 3 text heights beyond the one before (with text 2.5: rows at 7.5, 15, 22.5 from the
   edge). Chains of short sizes (edge to hole, hole to hole) go in the inner row, overall sizes in
   the outer row. Don't cross other dimensions or the part.
7. Send steps 3 to 6 as one `run_commands` call (the layer, then every dimension), not one
   call per dimension.
8. Check with one `render` on the same window; move any that overlap (`dimtedit {handles, at}`).
