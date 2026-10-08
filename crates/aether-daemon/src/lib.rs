//! Aether daemon — wires proxy engine + storage + control plane.
//! Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).

pub mod config;
pub mod control;
pub mod daemon;
pub mod replay;
pub mod rules;

pub use config::{AetherConfig, default_data_dir};
pub use control::{ControlRequest, DecisionDto, HeldPayloadDto, HeldRow, StatsInfo};
pub use daemon::{DaemonInfo, DaemonOptions, VERSION, run};
pub use replay::{run_replay, validate_replay_request};
pub use rules::{RuleEntry, RulesStore};
