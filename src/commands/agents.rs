/// Print the embedded skill body for appending to AGENTS.md.
pub fn run() {
    let (_, body) = super::skill::SKILL_MD
        .split_once("\n---\n")
        .expect("bundled skill must have YAML frontmatter");
    print!("{}", body.trim_start());
}
