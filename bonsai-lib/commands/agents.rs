/// Print the skill body for appending to AGENTS.md.
pub fn run(skill_md: &str) -> anyhow::Result<()> {
    let (_, body) = skill_md
        .split_once("\n---\n")
        .expect("bundled skill must have YAML frontmatter");
    crate::output::write(format_args!("{}", body.trim_start()))
}
