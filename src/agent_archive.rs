use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivedAgentSession {
    pub archive_id: String,
    pub source: Option<String>,
    pub agent: String,
    pub session_ref: Option<ArchivedAgentSessionRef>,
    pub title: Option<String>,
    pub label: Option<String>,
    pub agent_name: Option<String>,
    pub cwd: PathBuf,
    pub project_root: Option<PathBuf>,
    pub workspace_id: String,
    pub workspace_name: Option<String>,
    pub project_identity: Option<crate::workspace::WorkspaceProjectIdentity>,
    pub worktree_space: Option<crate::workspace::WorktreeSpaceMembership>,
    pub tab_name: Option<String>,
    pub last_user_activity_at: Option<u64>,
    pub last_agent_activity_at: Option<u64>,
    pub closed_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_pane_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivedAgentSessionRef {
    pub kind: crate::agent_resume::AgentSessionRefKind,
    pub value: String,
}

impl ArchivedAgentSession {
    pub fn dedupe_key(&self) -> Option<String> {
        let source = self.source.as_deref()?;
        let session_ref = self.session_ref.as_ref()?;
        Some(crate::agent_resume::dedupe_key(
            source,
            &self.agent,
            &crate::agent_resume::AgentSessionRef {
                kind: session_ref.kind,
                value: session_ref.value.clone(),
            },
        ))
    }

    pub fn resume_plan(&self) -> Option<crate::agent_resume::AgentResumePlan> {
        let source = self.source.as_deref()?;
        let session_ref = self.session_ref.as_ref()?;
        crate::agent_resume::plan(
            source,
            &self.agent,
            &crate::agent_resume::AgentSessionRef {
                kind: session_ref.kind,
                value: session_ref.value.clone(),
            },
        )
    }

    pub fn is_closed(&self) -> bool {
        self.active_pane_id.is_none()
    }
}

pub fn unix_ms_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}
