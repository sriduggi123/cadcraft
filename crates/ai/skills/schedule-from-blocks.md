---
name: schedule-from-blocks
when: The user asks to count blocks, make a parts list, bill of materials, door/window schedule, or table from attributes.
---
# Schedule from blocks

1. `drawing_summary` lists the blocks. `query_entities {type: "Insert"}` gives the references
   with their block names and attributes.
2. `dataextraction {blocks: "DOOR*", tags: "*"}` counts the references and gathers attribute
   values (identical rows combined with a count).
3. Show the user the counts. To put them in the drawing, make a table:
   `table {at, rows, cols, title, header: [...], cells: [[...], ...]}`, placed beside the
   drawing (from the extents) or where the user picks (`pick_points`).
4. Sort rows by the first column; totals in the last row if the user wants them.
