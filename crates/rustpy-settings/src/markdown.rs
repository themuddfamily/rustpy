use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Layout {
    Editor,
    #[default]
    Split,
    Preview,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Split {
    #[default]
    Right,
    Below,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub layout: Layout,
    pub split: Split,
    pub sync_scroll: bool,
    pub font_size: f32,
    pub toolbar: bool,
    pub auto_format_tables: bool,
    pub inspections: bool,
    pub custom_css: String,
    pub dark: bool,
    pub math: bool,
    pub mermaid: bool,
    pub plantuml: bool,
    pub pandoc: String,
    pub pdf_engine: String,
    pub katex_assets: String,
    pub mermaid_script: String,
    pub plantuml_server: String,
    pub remote_images: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            layout: Layout::Split,
            split: Split::Right,
            sync_scroll: true,
            font_size: 15.0,
            toolbar: true,
            auto_format_tables: true,
            inspections: true,
            custom_css: String::new(),
            dark: true,
            math: true,
            mermaid: false,
            plantuml: false,
            pandoc: "pandoc".into(),
            pdf_engine: String::new(),
            katex_assets: "https://cdn.jsdelivr.net/npm/katex@0.16.22/dist".into(),
            mermaid_script: "https://cdn.jsdelivr.net/npm/mermaid@11/dist/mermaid.esm.min.mjs"
                .into(),
            plantuml_server: "https://www.plantuml.com/plantuml/svg".into(),
            remote_images: false,
        }
    }
}
