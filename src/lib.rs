//! Private IP labels over pNet.
//!
//! `pnet-lan-client` and `pnet-lan-server` are separate processes. They share
//! this library, register under those names, and use protocol
//! `application/pnet-lan`.

mod directory;
mod fetch;
mod forward;
mod node_api;
mod page;
mod push;
mod startup;
mod store;
mod tun;

pub use directory::ProcessKind;
pub use startup::main_for;
