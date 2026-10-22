//! TUI 封面：渲染职责归 app，获取/缓存职责归 runtime。

pub mod accent;
mod layout;
mod probe;
mod render;

pub use layout::CoverGeometry;
pub use probe::run_if_probe;
pub use ratatui_image::picker::ProtocolType;
pub use render::{CoverCapabilities, CoverRenderer, protocol_from_config, protocol_label};
pub use voicefox_runtime::{CoverService, CoverState, sweep_temp_files};
