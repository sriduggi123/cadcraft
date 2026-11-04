---
name: layer-cleanup
when: The user asks to clean up, rename, merge or standardise layers, or tidy a drawing's layers.
---
# Clean up layers

1. `drawing_summary` for the layers; `query_entities {layer, limit: 0}` gives each layer's
   object count (`count`) without the objects.
2. Propose the plan to the user with `ask_user` before changing anything: which layers to
   rename, which to merge into which, which empty ones to remove. List it compactly.
3. Renames: `layer.set {name, newName}`. Merges: `laymrg {from: [names], to}` (objects move,
   the old layers go). Never merge or delete layer `0` or `Defpoints`.
4. Objects drawn with a fixed colour that should follow the layer: `properties.set {handles,
   color: "ByLayer"}` (ask first if many).
5. Empty layers: `purge {type: "layers"}` (the user is asked to confirm).
6. Report: layers before and after, what moved where.
