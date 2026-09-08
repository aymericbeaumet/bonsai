/// Print the embedded skill body for appending to AGENTS.md.
pub fn run() -> anyhow::Result<()> {
    let (_, body) = super::skill::SKILL_MD
        .split_once("\n---\n")
        .expect("bundled skill must have YAML frontmatter");
    crate::output::write(format_args!("{}", body.trim_start()))
}
