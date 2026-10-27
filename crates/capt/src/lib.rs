//! Canon CAPT support for the LBP2900 (Canon engine "P1080", shared with the LBP3000).
//!
//! The protocol facts implemented here come from studying Canon's mac-capt v10.0.9
//! driver; see `docs/re-notes.md` for the details and where each fact comes from.

pub mod engine;
pub mod halftone;
pub mod packet;
pub mod page;
pub mod raster;
pub mod slim;
pub mod spool;

pub use engine::{Engine, EngineOptions, Transport};
pub use page::{PageEncoder, Paper};
