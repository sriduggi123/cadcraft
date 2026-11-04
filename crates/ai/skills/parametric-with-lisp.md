---
name: parametric-with-lisp
when: The work repeats (many holes, a grid, numbered labels, editing many objects the same way) or depends on formulas.
---
# Parametric work with LISP

Write one `run_lisp` routine instead of many `execute` calls. CADCraft's AutoLISP has the usual
core: `setq`, `repeat`, `while`, `foreach`, `mapcar`, `polar`, `distance`, `angle`, `inters`,
`rtos`, `itoa`, `strcat`, `getvar`/`setvar`, `ssget` (with `"X"` filters like
`'((0 . "CIRCLE") (8 . "Holes"))`), `ssname`, `sslength`, `entget`, `entmod`, `entmake`,
`entlast`, `tblsearch`, and `command` / `vl-cmdf`. There is no ActiveX (`vlax-…`) and no
`cvunit`.

- Angles in LISP are radians: `(* pi (/ deg 180.0))`.
- Draw with `(command "_.CIRCLE" (list x y) r)`; prefer `entmake` for many objects (faster).
- Finish with `(princ)` or return a short value, e.g. the count made; the result comes back.
- Keep loops bounded (`repeat n`), at most a few thousand objects.
- After it runs, check with `query_entities` (counts) and `render`.

Example, a bolt circle of 6 holes:
```
(setq c '(50.0 50.0) r 40.0 n 6 i 0)
(repeat n
  (entmake (list '(0 . "CIRCLE") (cons 10 (polar c (* i (/ (* 2 pi) n)) r)) '(40 . 4.0)))
  (setq i (1+ i)))
n
```
