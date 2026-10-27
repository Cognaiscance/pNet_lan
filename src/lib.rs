//! Private IP labels over pNet.
//!
//! `pnet-lan-client` and `pnet-lan-server` are separate processes. They share
//! this library, register under those names, and use protocol
//! `application/pnet-lan`.

mod directory;
mod fetch;
mod node_api;
mod startup;
mod store;

pub use directory::ProcessKind;
pub use startup::main_for;
