use super::{ModelChoice, Provider};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    io,
    process::Command,
    sync::mpsc::{self, Receiver},
    thread,
};

#[derive(Clone, Debug, Serialize)]
pub struct Model {
    pub provider: Provider,
    pub model: String,
    pub label: String,
    pub efforts: Vec<String>,
    pub default_effort: String,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct Catalog {
    pub models: Vec<Model>,
    pub loading: bool,
    pub errors: Vec<String>,
}
impl Catalog {
    pub fn validate(&self, choice: &ModelChoice) -> Result<(), String> {
        let model=self.models.iter().find(|m|m.provider==choice.provider && m.model==choice.model).ok_or_else(||format!("{} is not in the current {} catalog. Refresh models and choose an available model.",choice.model,choice.provider.name()))?;
        if !model.efforts.contains(&choice.effort) {
            return Err(format!(
                "{} does not support {} effort.",
                model.label, choice.effort
            ));
        }
        Ok(())
    }
}
pub struct Discovered {
    pub models: Vec<Model>,
    pub limits: Vec<crate::quota::Observation>,
}
pub type Discovery = Receiver<(Provider, io::Result<Discovered>)>;
pub fn discover(muse: bool) -> Discovery {
    let (tx, rx) = mpsc::channel();
    for provider in [Provider::Codex, Provider::Muse] {
        if provider == Provider::Muse && !muse {
            continue;
        }
        let tx = tx.clone();
        thread::spawn(move || {
            let result = match provider {
                Provider::Codex => codex(),
                Provider::Muse => crate::muse::model_catalog().and_then(|v| {
                    Ok(Discovered {
                        models: parse(Provider::Muse, &v)?,
                        limits: crate::quota::muse_limits(&v["usage"]),
                    })
                }),
            };
            let _ = tx.send((provider, result));
        });
    }
    rx
}
fn codex() -> io::Result<Discovered> {
    let (mut child, mut rpc) =
        crate::rpc::Rpc::start(Command::new("codex").args(["app-server", "--listen", "stdio://"]))?;
    let result = (|| {
        rpc.call("initialize",json!({"clientInfo":{"name":"sprowt_model_catalog","version":"0.1"},"capabilities":{"experimentalApi":true}}))?;
        rpc.write(json!({"method":"initialized"}))?;
        let models = crate::codex::model_catalog(&mut rpc)?;
        Ok(Discovered {
            models: parse(Provider::Codex, &json!({"models":models}))?,
            limits: rpc
                .call("account/rateLimits/read", json!({}))
                .ok()
                .map_or_else(Vec::new, |v| {
                    crate::quota::codex_limits(&v, super::now_ms())
                }),
        })
    })();
    crate::rpc::terminate(&mut child);
    result
}
pub(crate) fn parse(provider: Provider, value: &Value) -> io::Result<Vec<Model>> {
    let entries = value["models"]
        .as_array()
        .ok_or_else(|| io::Error::other("Provider returned no model catalog."))?;
    let mut models = Vec::new();
    for item in entries {
        let (id, label, efforts) = match provider {
            Provider::Codex => (
                item["model"].as_str(),
                item["displayName"].as_str(),
                item["supportedReasoningEfforts"].as_array().map(|a| {
                    a.iter()
                        .filter_map(|e| e["reasoningEffort"].as_str().map(str::to_owned))
                        .collect::<Vec<_>>()
                }),
            ),
            Provider::Muse => (
                item["modelId"].as_str(),
                item["displayLabel"].as_str(),
                item["variants"].as_array().map(|a| {
                    a.iter()
                        .filter_map(|e| {
                            e.as_str()
                                .or_else(|| e["reasoningEffort"].as_str())
                                .map(str::to_owned)
                        })
                        .collect::<Vec<_>>()
                }),
            ),
        };
        let Some(id) = id.filter(|id| !id.is_empty() && id.len() < 200) else {
            continue;
        };
        let Some(efforts) = efforts.filter(|e| !e.is_empty()) else {
            continue;
        };
        let default = item["defaultReasoningEffort"]
            .as_str()
            .filter(|s| efforts.iter().any(|e| e == s))
            .unwrap_or(&efforts[0])
            .to_owned();
        models.push(Model {
            provider: provider.clone(),
            model: id.into(),
            label: label.unwrap_or(id).into(),
            efforts,
            default_effort: default,
        });
    }
    if models.is_empty() {
        return Err(io::Error::other("Provider listed no supported models."));
    }
    Ok(models)
}
