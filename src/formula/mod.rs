//! Embedded formulas: shared registry, asynchronous session runs, and Rhai APIs.
pub(crate) mod bridge;
pub mod cli;
mod manager;
mod registry;
mod runtime;
pub mod ui;

pub use manager::Manager;
pub use registry::{Definition, Parameter, get, list, put, validate};
pub(crate) use runtime::helper::inventory as helper_inventory;
pub use runtime::reference;
