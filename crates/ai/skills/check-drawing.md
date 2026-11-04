---
name: check-drawing
when: The user asks to check, audit, fix or find problems in a drawing.
---
# Check a drawing

1. `audit {}` reports errors without fixing; show them, then `audit {fix: true}` if the user
   agrees.
2. Duplicates and overlaps: `overkill {}` deletes duplicate objects (ask first when it would
   remove many; report how many).
3. Look for: zero-length lines (`purge {type: "zerolength"}`), empty text
   (`purge {type: "emptytext"}`), objects on layer `Defpoints` that should plot, text heights
   that differ from the rest (`find_text`), objects with fixed colours instead of ByLayer.
4. Report what was found and fixed, in a short list, with counts.
