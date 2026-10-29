use serde::Deserialize;

use crate::error::invalid;
use crate::{Error, Project, Result, fsx};

const DEFAULT_MODELS: &str = include_str!("../../../assets/models.toml");

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ModelSpec {
    pub provider: String,
    pub id: String,
    /// How a reference image is named in the prompt, `{n}` counting from 1.
    pub ref_image: String,
    pub max_images: usize,
    pub min_duration: f64,
    pub max_duration: f64,
    #[serde(default)]
    pub whole_seconds: bool,
    pub resolutions: Vec<String>,
    pub aspects: Vec<String>,
    #[serde(default)]
    pub start_frame: bool,
    #[serde(default)]
    pub start_frame_with_refs: bool,
    /// Name of a prompts/models/<notes>.md template with per-model tweaks.
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Deserialize)]
struct ModelsFile {
    #[serde(default)]
    model: Vec<ModelSpec>,
}

fn parse(text: &str) -> std::result::Result<Vec<ModelSpec>, toml::de::Error> {
    Ok(toml::from_str::<ModelsFile>(text)?.model)
}

impl Project {
    /// Built-in models, with the project's models.toml layered on top.
    pub fn models(&self) -> Result<Vec<ModelSpec>> {
        let mut models = parse(DEFAULT_MODELS).expect("assets/models.toml is valid");
        let path = self.root.join("models.toml");
        if path.exists() {
            let ours = parse(&fsx::read(&path)?).map_err(|source| Error::Toml { path, source })?;
            for m in ours {
                match models
                    .iter_mut()
                    .find(|x| x.provider == m.provider && x.id == m.id)
                {
                    Some(x) => *x = m,
                    None => models.push(m),
                }
            }
        }
        Ok(models)
    }

    pub fn model(&self, provider: &str, id: &str) -> Result<ModelSpec> {
        match self
            .models()?
            .into_iter()
            .find(|m| m.provider == provider && m.id == id)
        {
            Some(m) => Ok(m),
            None => invalid(format!(
                "unknown model {provider} {id}, add it to models.toml"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_models_parse() {
        let models = parse(DEFAULT_MODELS).unwrap();
        assert!(models.iter().any(|m| m.provider == "fal"));
        assert!(models.iter().any(|m| m.provider == "venice"));
        for m in &models {
            assert!(
                m.ref_image.contains("{n}"),
                "{} ref_image needs {{n}}",
                m.id
            );
            assert!(m.min_duration <= m.max_duration);
        }
    }
}
