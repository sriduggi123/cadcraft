---
name: dimension-a-part
when: The user asks to dimension a part, a view or some objects, or to add sizes.
---
# Dimension a part

1. Find the part: `query_entities` with a window around it (or the selection). `render` that
   window to see its shape and which side is the front.
2. Pick the dimension style: `drawing_summary` lists them. Use the current one unless the user
   names another; `dimstyle.current {name}` switches.
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
6. Placement: put the dimension line outside the part, about 2 to 3 text heights away, and
   stacked dimensions one text height apart (`dimspace` evens them out). Don't cross other
   dimensions or the part.
7. Check with `render` on the same window; move any that overlap (`dimtedit {handles, at}`).
