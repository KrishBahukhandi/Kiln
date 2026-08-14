//! The runtimes Kiln ships with.
//!
//! Adding one means adding a module here and a line in
//! [`crate::Registry::builtin`]. Nothing else in the workspace needs to change.

pub mod go;
pub mod node;
pub mod python;

pub use go::GoProvider;
pub use node::NodeProvider;
pub use python::PythonProvider;
