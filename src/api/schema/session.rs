use serde::{Deserialize, Serialize};

use super::agents::{AgentInfo, ArchivedAgentInfo};
use super::panes::{PaneInfo, PaneLayoutSnapshot};
use super::tabs::TabInfo;
use super::workspaces::WorkspaceInfo;
use super::{EventEnvelope, RuntimeIdentity};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct SessionWatchParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_cursor: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SequencedEventEnvelope {
    pub cursor: u64,
    #[serde(flatten)]
    pub event: EventEnvelope,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionSnapshot {
    pub identity: RuntimeIdentity,
    pub version: String,
    pub protocol: u32,
    pub event_cursor: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_pane_id: Option<String>,
    pub workspaces: Vec<WorkspaceInfo>,
    pub tabs: Vec<TabInfo>,
    pub panes: Vec<PaneInfo>,
    pub layouts: Vec<PaneLayoutSnapshot>,
    pub agents: Vec<AgentInfo>,
    #[serde(default)]
    pub archived_agents: Vec<ArchivedAgentInfo>,
}
