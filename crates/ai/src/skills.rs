//! Skills: playbooks for kinds of requests (dimension a part, clean up layers…), written for
//! CADCraft's commands. The model sees each skill's name and when to use it in the system
//! prompt and reads one with `load_skill` when a request fits, so the prompt stays short.

/// The built-in skills' files (`skills/*.md`): a header with `name:` and `when:`, then the body.
const FILES: &[&str] = &[
    include_str!("../skills/dimension-a-part.md"),
    include_str!("../skills/draw-from-description.md"),
    include_str!("../skills/layer-cleanup.md"),
    include_str!("../skills/title-block-and-plot.md"),
    include_str!("../skills/schedule-from-blocks.md"),
    include_str!("../skills/units-and-scale.md"),
    include_str!("../skills/check-drawing.md"),
    include_str!("../skills/parametric-with-lisp.md"),
];

#[derive(Clone, Debug, PartialEq)]
pub struct Skill {
    pub name: String,
    /// When to use it (one line, shown to the model in every request).
    pub when: String,
    pub body: String,
}

/// A skill file: `---`, `name: …`, `when: …`, `---`, then the body. `None` if the header is
/// missing either field.
pub fn parse(text: &str) -> Option<Skill> {
    let rest = text.trim_start().strip_prefix("---")?;
    let (head, body) = rest.split_once("\n---")?;
    let field =
        |k: &str| head.lines().find_map(|l| l.trim().strip_prefix(k)?.strip_prefix(':').map(|v| v.trim().to_string())).filter(|v| !v.is_empty());
    Some(Skill { name: field("name")?, when: field("when")?, body: body.trim_start_matches(['-', '\n', '\r']).trim().to_string() })
}

/// The built-in skills.
pub fn all() -> Vec<Skill> {
    FILES.iter().filter_map(|f| parse(f)).collect()
}

/// The skill named `name` (any case).
pub fn find(name: &str) -> Option<Skill> {
    all().into_iter().find(|s| s.name.eq_ignore_ascii_case(name.trim()))
}

/// The list for the system prompt: one line per skill.
pub fn index() -> String {
    all().iter().map(|s| format!("- {}: {}", s.name, s.when)).collect::<Vec<_>>().join("\n")
}
