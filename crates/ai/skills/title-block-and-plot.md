---
name: title-block-and-plot
when: The user asks to set up a sheet or layout, fill a title block, set a viewport scale, or print/plot to PDF.
---
# Title block and plot

1. Layouts: `layout.list` shows them with their viewports. Make one with `layout.new {name}`
   if needed, switch with `layout.set {name}`.
2. Page: `pagesetup {layout, paper: "A3", landscape: true}` (sizes such as A4, A3, Letter,
   "ANSI B").
3. Viewport: `mview {option: "fit"}` makes one filling the printable area. Scale it with
   `zoom {mode: "scale", factor, xp: true}` inside it (factor = paper units per model unit,
   e.g. 1/50 for 1:50 in millimetres).
4. Title block: if the drawing has a title block block (`drawing_summary` blocks, names like
   `TITLE*`, `TB*`), insert it at the sheet's lower-left with `insert {name, at, attribs: {TAG:
   value}}`; fill an existing one with `attedit {handle, values}` (`find_text` finds its
   attributes). Ask the user for the title, number and author if not given.
5. Plot: `plot {layout, device: "CADCraft PDF", path}` writes a PDF (the user is asked first).
   Ask where to save it if the user didn't say.
