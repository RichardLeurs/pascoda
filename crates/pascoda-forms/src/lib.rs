//! Form documents for the Pascoda designer: the Lazarus `.lfm` format, read and edited without
//! losing anything we do not understand.

pub mod doc;
pub mod lfm;
pub mod model;
pub mod sync;

pub use doc::{EditError, FormDoc};
pub use lfm::ParseError;
pub use model::{Component, Kind, PropValue, Property};
