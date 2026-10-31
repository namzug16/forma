use serde::Serialize;
use thiserror::Error;

/// A machine-readable problem with a stable `code`.
#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
}

impl Diagnostic {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            path: None,
            layer: None,
        }
    }

    pub fn at(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn on_layer(mut self, layer: impl Into<String>) -> Self {
        self.layer = Some(layer.into());
        self
    }
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "[{}]", self.code)?;
        if let Some(path) = &self.path {
            write!(formatter, " {path}:")?;
        }
        write!(formatter, " {}", self.message)?;
        if let Some(layer) = &self.layer {
            write!(formatter, " (layer \"{layer}\")")?;
        }
        Ok(())
    }
}

/// One or more diagnostics that stopped a command.
#[derive(Debug, Error)]
#[error("{}", .0.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"))]
pub struct Failure(pub Vec<Diagnostic>);

impl From<Diagnostic> for Failure {
    fn from(diagnostic: Diagnostic) -> Self {
        Self(vec![diagnostic])
    }
}
