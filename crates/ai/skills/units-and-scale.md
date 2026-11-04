---
name: units-and-scale
when: The user asks to change units, convert inches to millimetres (or back), or rescale a drawing.
---
# Units and scale

1. Read `insunits` (0 none, 1 inches, 2 feet, 4 millimetres, 5 centimetres, 6 metres) and the
   extents: a part 300 units long in a drawing marked inches is probably millimetres
   mislabelled; ask before assuming.
2. Only the label is wrong: `units {insunits}` changes it without moving anything.
3. Converting the geometry: `scale {handles, base: [0, 0], factor}` on everything in model
   space (`query_entities` for the handles; 25.4 for inches to millimetres, 1/25.4 back), then
   `units {insunits}` to match. Text heights and dimension styles scale with it; check
   dimension text with `render`.
4. Say the factor used and the new extents.
