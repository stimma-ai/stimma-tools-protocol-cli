//! Tool descriptor model and `x-control` interpretation.

use serde::Deserialize;
use serde_json::Value;

/// A tool as advertised by `tools.list` / `tools.refresh`.
#[derive(Debug, Clone, Deserialize)]
pub struct ToolDescriptor {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub task_types: Vec<String>,
    #[serde(default)]
    pub parameter_schema: Value,
    #[serde(default)]
    pub output_schema: Value,
    #[serde(default)]
    pub layout: Vec<LayoutSection>,
    #[serde(default)]
    pub subtitle: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LayoutSection {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub params: Vec<LayoutParam>,
    #[serde(default)]
    pub collapsed: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LayoutParam {
    pub name: String,
}

/// Media `x-control` values whose value is an **array** of asset references.
pub const ARRAY_MEDIA_CONTROLS: &[&str] = &["image_picker", "video_picker", "video_frame_picker"];
/// Media `x-control` values whose value is a **single** asset reference.
pub const SINGLE_MEDIA_CONTROLS: &[&str] = &["mask_editor"];

impl ToolDescriptor {
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }

    /// `metadata.badges`, if present.
    pub fn badges(&self) -> Vec<String> {
        self.metadata
            .get("badges")
            .and_then(|b| b.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The schema object for one parameter, if present.
    pub fn property(&self, name: &str) -> Option<&Value> {
        self.parameter_schema
            .get("properties")
            .and_then(|p| p.get(name))
    }

    /// Names of media-input parameters, mapped to whether they are array-valued.
    pub fn media_params(&self) -> Vec<MediaParam> {
        let mut out = Vec::new();
        if let Some(props) = self
            .parameter_schema
            .get("properties")
            .and_then(|p| p.as_object())
        {
            for (name, schema) in props {
                if let Some(control) = schema.get("x-control").and_then(|c| c.as_str()) {
                    if ARRAY_MEDIA_CONTROLS.contains(&control) {
                        out.push(MediaParam {
                            name: name.clone(),
                            array: true,
                        });
                    } else if SINGLE_MEDIA_CONTROLS.contains(&control) {
                        out.push(MediaParam {
                            name: name.clone(),
                            array: false,
                        });
                    }
                }
            }
        }
        out
    }

    /// Output roles declared in `output_schema` (best-effort; open vocabulary).
    pub fn output_roles(&self) -> Vec<String> {
        // The output schema constrains assets[].role only loosely; we surface the enum if a
        // tool pinned one, else the conventional "primary".
        self.output_schema
            .pointer("/properties/assets/items/properties/role/enum")
            .and_then(|e| e.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect::<Vec<_>>()
            })
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| vec!["primary".to_string()])
    }
}

#[derive(Debug, Clone)]
pub struct MediaParam {
    pub name: String,
    pub array: bool,
}
