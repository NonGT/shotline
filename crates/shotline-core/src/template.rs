use crate::{Project, Result, fsx};

const DEFAULTS: &[(&str, &str)] = &[
    ("style", include_str!("../../../assets/prompts/style.md")),
    (
        "identity",
        include_str!("../../../assets/prompts/identity.md"),
    ),
    (
        "continue",
        include_str!("../../../assets/prompts/continue.md"),
    ),
];

impl Project {
    /// prompts/<name>.md from the project if it's there, else the built-in one.
    pub fn template(&self, name: &str) -> Result<Option<String>> {
        let path = self.root.join("prompts").join(format!("{name}.md"));
        if path.exists() {
            return Ok(Some(fsx::read(&path)?));
        }
        Ok(DEFAULTS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, t)| t.to_string()))
    }
}

/// Fills `{key}` placeholders.
pub fn render(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = template.trim().to_string();
    for (k, v) in vars {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::render;

    #[test]
    fn renders() {
        assert_eq!(
            render("Style: {style}\n", &[("style", "noir")]),
            "Style: noir"
        );
        assert_eq!(render("Image {n}", &[("n", "3")]), "Image 3");
    }
}
