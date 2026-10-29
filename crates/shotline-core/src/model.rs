use serde::{Deserialize, Serialize};
use shotline_protocol::{ModelInfo, Op};

use crate::{Error, Project, Result, fsx};

const BUILTIN_MODELS: &str = include_str!("../../../assets/models.toml");

/// A model and the plugin that serves it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelSpec {
    pub plugin: String,
    #[serde(flatten)]
    pub info: ModelInfo,
}

#[derive(Deserialize)]
struct ModelsFile {
    #[serde(default)]
    model: Vec<ModelSpec>,
}

fn parse(text: &str) -> std::result::Result<Vec<ModelSpec>, toml::de::Error> {
    Ok(toml::from_str::<ModelsFile>(text)?.model)
}

/// Every model shotline knows about. Later additions replace earlier ones with the same plugin,
/// id and operation: built-in list, then what plugins describe, then the project's models.toml.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    models: Vec<ModelSpec>,
}

impl Catalog {
    pub fn builtin() -> Catalog {
        let mut c = Catalog::default();
        c.extend(parse(BUILTIN_MODELS).expect("assets/models.toml is valid"));
        c
    }

    pub fn extend(&mut self, models: impl IntoIterator<Item = ModelSpec>) {
        for m in models {
            match self
                .models
                .iter_mut()
                .find(|x| x.plugin == m.plugin && x.info.id == m.info.id && x.info.op == m.info.op)
            {
                Some(x) => *x = m,
                None => self.models.push(m),
            }
        }
    }

    pub fn add_plugin(&mut self, plugin: &str, models: impl IntoIterator<Item = ModelInfo>) {
        self.extend(models.into_iter().map(|info| ModelSpec {
            plugin: plugin.to_string(),
            info,
        }));
    }

    pub fn all(&self) -> &[ModelSpec] {
        &self.models
    }

    pub fn find(&self, plugin: &str, id: &str, op: &Op) -> Option<&ModelSpec> {
        self.models
            .iter()
            .find(|m| m.plugin == plugin && m.info.id == id && &m.info.op == op)
    }

    pub fn get(&self, plugin: &str, id: &str, op: &Op) -> Result<&ModelSpec> {
        self.find(plugin, id, op).ok_or_else(|| {
            Error::Invalid(format!(
                "no {op} model {id} from plugin {plugin}; is the plugin installed?"
            ))
        })
    }
}

impl Project {
    /// `base` with the project's models.toml layered on top.
    pub fn catalog(&self, mut base: Catalog) -> Result<Catalog> {
        let path = self.root.join("models.toml");
        if path.exists() {
            let ours = parse(&fsx::read(&path)?).map_err(|source| Error::Toml { path, source })?;
            base.extend(ours);
        }
        Ok(base)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_models_parse() {
        let c = Catalog::builtin();
        let fal = c
            .find(
                "fal",
                "minimax/h3-max/reference-to-video",
                &Op::VideoGenerate,
            )
            .unwrap();
        assert_eq!(fal.info.max_images, Some(9));
        assert_eq!(fal.info.min_duration, Some(0.92));
        for m in c.all() {
            if let Some(t) = &m.info.ref_image {
                assert!(t.contains("{n}"), "{} ref_image needs {{n}}", m.info.id);
            }
        }
    }

    #[test]
    fn later_entries_replace_earlier() {
        let mut c = Catalog::builtin();
        let n = c.all().len();
        c.add_plugin(
            "fal",
            [ModelInfo {
                id: "minimax/h3-max/reference-to-video".into(),
                op: Op::VideoGenerate,
                max_images: Some(3),
                ..ModelInfo::default()
            }],
        );
        assert_eq!(c.all().len(), n);
        let m = c
            .get(
                "fal",
                "minimax/h3-max/reference-to-video",
                &Op::VideoGenerate,
            )
            .unwrap();
        assert_eq!(m.info.max_images, Some(3));
        assert!(c.get("fal", "nope", &Op::VideoGenerate).is_err());
    }
}
