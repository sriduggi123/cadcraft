//! Drawing variables that belong to each person in a shared session (decision D9): the settings
//! new objects take (current layer, colour, styles), command defaults, drafting aids, the view and
//! selection, and app settings. They are never synced: one person changing their current layer
//! must not change anyone else's. The plan's appendix lists the groups.

/// Per-person variables, upper case, without `$`. Sorted (binary search).
const LOCAL: &[&str] = &[
    "ANNOALLVISIBLE",
    "ANNOAUTOSCALE",
    "APERTURE",
    "ATTREQ",
    "AUDITCTL",
    "AUTOSNAP",
    "BLOCKEDITOR",
    "CANNOSCALE",
    "CANNOSCALEVALUE",
    "CECOLOR",
    "CELTSCALE",
    "CELTYPE",
    "CELWEIGHT",
    "CHAMFERA",
    "CHAMFERB",
    "CHAMFERC",
    "CHAMFERD",
    "CHAMMODE",
    "CIRCLERAD",
    "CLAYER",
    "CMDACTIVE",
    "CMDECHO",
    "CMDNAMES",
    "CMLEADERSTYLE",
    "CMLJUST",
    "CMLSCALE",
    "CONSTRAINTINFER",
    "CPLOTSTYLE",
    "CROSSINGAREACOLOR",
    "CTAB",
    "CTABLESTYLE",
    "CURSORSIZE",
    "DIMSTYLE",
    "DONUTID",
    "DONUTOD",
    "DYNDIGRIP",
    "DYNDIVIS",
    "DYNMODE",
    "DYNPICOORDS",
    "DYNPIFORMAT",
    "DYNPROMPT",
    "EDGEMODE",
    "ELEVATION",
    "FILEDIA",
    "FILLETRAD",
    "GRIDMAJOR",
    "GRIDMODE",
    "GRIDUNIT",
    "GRIPOBJLIMIT",
    "GRIPS",
    "GRIPSIZE",
    "HIGHLIGHT",
    "HPANG",
    "HPDLGMODE",
    "HPNAME",
    "HPSCALE",
    "INSNAME",
    "ISAVEBAK",
    "LASTPOINT",
    "LWDISPLAY",
    "MAXARRAY",
    "MEASUREINIT",
    "OBJECTISOLATIONMODE",
    "OFFSETDIST",
    "OFFSETERASE",
    "OFFSETLAYER",
    "ORTHOMODE",
    "OSMODE",
    "OSNAPHATCH",
    "PICKADD",
    "PICKBOX",
    "PICKDRAG",
    "PICKFIRST",
    "PICKSTYLE",
    "PLATFORM",
    "PLOTTRANSPARENCYOVERRIDE",
    "POLARADDANG",
    "POLARANG",
    "POLARDIST",
    "POLARMODE",
    "POLYSIDES",
    "PREVIEWEFFECT",
    "PREVIEWFILTER",
    "PROJMODE",
    "QNEWTEMPLATE",
    "QPMODE",
    "REVCLOUDARCLEN",
    "SAVEFILEPATH",
    "SAVETIME",
    "SELECTIONAREA",
    "SELECTIONAREAOPACITY",
    "SELECTIONCYCLING",
    "SELECTIONEFFECT",
    "SELECTIONPREVIEW",
    "SELECTSIMILARMODE",
    "SNAPANG",
    "SNAPBASE",
    "SNAPISOPAIR",
    "SNAPMODE",
    "SNAPSTYL",
    "SNAPTYPE",
    "SNAPUNIT",
    "TEXTSIZE",
    "TEXTSTYLE",
    "THICKNESS",
    "TILEMODE",
    "TRANSPARENCYDISPLAY",
    "TRIMEXTENDMODE",
    "TRIMMODE",
    "UNDOCTL",
    "UNDOMARKS",
    "VIEWCTR",
    "VIEWSIZE",
    "WINDOWAREACOLOR",
    "ZOOMFACTOR",
];

/// `DIM*` variables that stay shared: they set dimensions already in the drawing, not only the
/// next one. Every other `DIM*` variable is an override of the current dimension style, which
/// belongs to the person drawing.
const SHARED_DIM: &[&str] = &["DIMASSOC", "DIMSCALE"];

/// Whether drawing variable `name` (any case, with or without `$`) belongs to each person.
pub fn is_local(name: &str) -> bool {
    let n = name.trim_start_matches('$').to_ascii_uppercase();
    if n.starts_with("DIM") && n != "DIMSTYLE" {
        return !SHARED_DIM.contains(&n.as_str());
    }
    LOCAL.binary_search(&n.as_str()).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_is_sorted_and_unique() {
        assert!(LOCAL.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn current_settings_are_local_and_units_shared() {
        for v in ["CLAYER", "$clayer", "CECOLOR", "OSMODE", "DIMSTYLE", "DIMTXT", "VIEWCTR"] {
            assert!(is_local(v), "{v}");
        }
        for v in ["INSUNITS", "$LTSCALE", "DIMSCALE", "DIMASSOC", "EXTMIN", "MEASUREMENT"] {
            assert!(!is_local(v), "{v}");
        }
    }
}
