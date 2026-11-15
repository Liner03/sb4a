// SPDX-License-Identifier: GPL-3.0-or-later
//! Wire types shared with the Go core and the CLI.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: i32 = 1;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessIdentity {
    #[serde(default)]
    pub pid: i32,
    #[serde(default)]
    pub start: String,
    #[serde(default)]
    pub executable: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stats {
    pub tag: String,
    pub tx: i64,
    pub rx: i64,
    #[serde(rename = "directTx")]
    pub direct_tx: i64,
    #[serde(rename = "directRx")]
    pub direct_rx: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub time: i64,
    pub code: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(rename = "schemaVersion")]
    pub schema_version: i32,
    pub phase: String,
    #[serde(default)]
    pub revision: String,
    #[serde(rename = "runningRevision", default)]
    pub running_revision: String,
    #[serde(rename = "profileId", default)]
    pub profile_id: i64,
    #[serde(rename = "profileName", default)]
    pub profile_name: String,
    #[serde(default)]
    pub supervisor: ProcessIdentity,
    #[serde(default)]
    pub core: ProcessIdentity,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<Stats>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<Event>,
}

impl State {
    pub fn stopped() -> Self {
        Self {
            schema_version: PROTOCOL_VERSION,
            phase: "stopped".into(),
            ..Default::default()
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Response {
    #[serde(rename = "schemaVersion")]
    pub schema_version: i32,
    pub ok: bool,
    pub state: State,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    pub fn new(state: State, error: Option<String>) -> Self {
        Self {
            schema_version: PROTOCOL_VERSION,
            ok: error.is_none(),
            state,
            error,
        }
    }
}
