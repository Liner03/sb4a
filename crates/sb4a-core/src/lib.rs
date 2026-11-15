// SPDX-License-Identifier: GPL-3.0-or-later
//! sb4a module core: supervisor, configuration transactions and geo rulesets.
//!
//! The Go `rootbox` binary stays the sing-box host; this crate owns everything
//! around it (lifecycle, validation, atomic state, ruleset provisioning).

pub mod atomic;
pub mod error;
pub mod portable;
pub mod process;
pub mod ruleset;
pub mod runtime;
pub mod snapshot;
pub mod state;
pub mod supervisor;

pub use error::Code;
