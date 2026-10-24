//! Persisted placement and user layouts for the live IDE tool-window framework.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    #[default]
    DockPinned,
    DockUnpinned,
    Undock,
    Float,
    Window,
}
impl Mode {
    pub const ALL: [Self; 5] = [
        Self::DockPinned,
        Self::DockUnpinned,
        Self::Undock,
        Self::Float,
        Self::Window,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::DockPinned => "Dock Pinned",
            Self::DockUnpinned => "Dock Unpinned",
            Self::Undock => "Undock",
            Self::Float => "Float",
            Self::Window => "Window",
        }
    }
    pub fn docked(self) -> bool {
        matches!(self, Self::DockPinned | Self::DockUnpinned)
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Slot {
    #[default]
    LeftTop,
    LeftBottom,
    RightTop,
    RightBottom,
    BottomLeft,
    BottomRight,
    TopLeft,
    TopRight,
}
impl Slot {
    pub const ALL: [Self; 8] = [
        Self::LeftTop,
        Self::LeftBottom,
        Self::RightTop,
        Self::RightBottom,
        Self::BottomLeft,
        Self::BottomRight,
        Self::TopLeft,
        Self::TopRight,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::LeftTop => "Left Top",
            Self::LeftBottom => "Left Bottom",
            Self::RightTop => "Right Top",
            Self::RightBottom => "Right Bottom",
            Self::BottomLeft => "Bottom Left",
            Self::BottomRight => "Bottom Right",
            Self::TopLeft => "Top Left",
            Self::TopRight => "Top Right",
        }
    }
    pub fn edge(self) -> usize {
        match self {
            Self::LeftTop | Self::LeftBottom => 0,
            Self::RightTop | Self::RightBottom => 1,
            Self::TopLeft | Self::TopRight => 2,
            _ => 3,
        }
    }
    pub fn second(self) -> bool {
        matches!(
            self,
            Self::LeftBottom | Self::RightBottom | Self::TopRight | Self::BottomRight
        )
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Placement {
    pub mode: Mode,
    pub slot: Slot,
    pub sidebar: bool,
    pub title: String,
    pub color: Option<[u8; 3]>,
    pub pinned: bool,
}
impl Default for Placement {
    fn default() -> Self {
        Self {
            mode: Mode::DockPinned,
            slot: Slot::LeftTop,
            sidebar: true,
            title: String::new(),
            color: None,
            pinned: false,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Framework {
    pub window_bounds: BTreeMap<String, [f32; 4]>,
    pub window_maximized: std::collections::BTreeSet<String>,
    pub wide: bool,
    pub maximized: Option<String>,
    pub tools: BTreeMap<String, Placement>,
    /// Additional simultaneously visible tools, beyond legacy active left/bottom.
    pub open: Vec<String>,
    pub selected: BTreeMap<Slot, String>,
    pub group_tabs: bool,
    pub side_by_side: bool,
    pub edge_sizes: [f32; 4],
    pub split_ratios: [f32; 4],
}
impl Default for Framework {
    fn default() -> Self {
        Self {
            window_bounds: BTreeMap::new(),
            window_maximized: Default::default(),
            wide: false,
            maximized: None,
            tools: BTreeMap::new(),
            open: Vec::new(),
            selected: BTreeMap::new(),
            group_tabs: true,
            side_by_side: true,
            edge_sizes: [260.0, 320.0, 250.0, 300.0],
            split_ratios: [0.5; 4],
        }
    }
}
impl Framework {
    pub fn placement(&self, id: &str) -> Placement {
        self.tools.get(id).cloned().unwrap_or_else(|| Placement {
            slot: if id.starts_with("right:") {
                Slot::RightTop
            } else if id.starts_with("bottom:") {
                Slot::BottomLeft
            } else {
                Slot::LeftTop
            },
            ..Default::default()
        })
    }
    pub fn placement_mut(&mut self, id: &str) -> &mut Placement {
        let default = self.placement(id);
        self.tools.entry(id.into()).or_insert(default)
    }
    pub fn sanitize(&mut self) {
        self.window_bounds
            .retain(|_, bounds| bounds.iter().all(|number| number.is_finite()));
        for bounds in self.window_bounds.values_mut() {
            bounds[0] = bounds[0].clamp(-100_000.0, 100_000.0);
            bounds[1] = bounds[1].clamp(-100_000.0, 100_000.0);
            bounds[2] = bounds[2].clamp(240.0, 2400.0);
            bounds[3] = bounds[3].clamp(160.0, 1800.0);
        }
        self.open.sort();
        self.open.dedup();
        self.open.truncate(64);
        for size in &mut self.edge_sizes {
            *size = if size.is_finite() {
                size.clamp(100.0, 1600.0)
            } else {
                300.0
            };
        }
        for ratio in &mut self.split_ratios {
            *ratio = if ratio.is_finite() {
                ratio.clamp(0.15, 0.85)
            } else {
                0.5
            };
        }
        for placement in self.tools.values_mut() {
            placement.title = placement.title.chars().take(120).collect();
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layouts {
    pub named: BTreeMap<String, crate::ToolWindowLayout>,
    pub default_layout: Option<crate::ToolWindowLayout>,
    pub restore_on_startup: bool,
}
impl Default for Layouts {
    fn default() -> Self {
        Self {
            named: BTreeMap::new(),
            default_layout: None,
            restore_on_startup: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_layouts_and_invalid_geometry_are_safe() {
        let mut state: Framework = serde_json::from_str("{}").unwrap();
        assert_eq!(state.placement("bottom:Run").slot, Slot::BottomLeft);
        assert!(state.group_tabs);
        state.edge_sizes[0] = f32::NAN;
        state
            .window_bounds
            .insert("bottom:Run".into(), [0.0, 0.0, f32::NAN, 300.0]);
        state.split_ratios[1] = f32::INFINITY;
        state.open = vec!["Bottom:Run".into(), "Bottom:Run".into()];
        state.sanitize();
        assert!(state.window_bounds.is_empty());
        assert_eq!(state.open.len(), 1);
        assert!(state.edge_sizes[0].is_finite());
        assert_eq!(state.split_ratios[1], 0.5);
    }
    #[test]
    fn named_layouts_retain_modes_and_sidebar_preferences() {
        let mut layout = crate::ToolWindowLayout::default();
        let placement = layout.framework.placement_mut("Bottom:Run");
        placement.mode = Mode::Window;
        placement.slot = Slot::RightBottom;
        placement.sidebar = false;
        let mut layouts = Layouts::default();
        layouts.named.insert("Debugging".into(), layout);
        assert_eq!(
            serde_json::from_str::<Layouts>(&serde_json::to_string(&layouts).unwrap()).unwrap(),
            layouts
        );
    }
}
