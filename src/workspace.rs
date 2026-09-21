use anyhow::{Context, Result, anyhow};
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::{fs, path::{Path, PathBuf}};

pub const WORKSPACE_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionKind {
    Live,
    Capture,
    Replay,
}

impl SessionKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Live => "LIVE",
            Self::Capture => "CAPTURE",
            Self::Replay => "REPLAY",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Bookmark {
    pub id: u64,
    pub label: String,
    pub timestamp: String,
    pub source: String,
    pub sequence: Option<u64>,
    pub replay_position_ms: Option<u64>,
    pub service_uuid: String,
    pub characteristic_uuid: String,
    pub hex: String,
}

impl Default for Bookmark {
    fn default() -> Self {
        Self {
            id: 0,
            label: String::new(),
            timestamp: String::new(),
            source: String::new(),
            sequence: None,
            replay_position_ms: None,
            service_uuid: String::new(),
            characteristic_uuid: String::new(),
            hex: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WorkspaceSession {
    pub id: u64,
    pub name: String,
    pub kind: SessionKind,
    pub device_id: Option<String>,
    pub device_name: Option<String>,
    pub source_path: Option<PathBuf>,
    pub bookmarks: Vec<Bookmark>,
    pub notes: String,
    pub created_at: String,
}

impl Default for WorkspaceSession {
    fn default() -> Self {
        Self {
            id: 1,
            name: "Live Session".to_owned(),
            kind: SessionKind::Live,
            device_id: None,
            device_name: None,
            source_path: None,
            bookmarks: Vec::new(),
            notes: String::new(),
            created_at: now_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WorkspaceLayout {
    pub show_devices_gatt: bool,
    pub show_plot: bool,
    pub show_protocol: bool,
    pub show_replay: bool,
    pub show_bookmarks: bool,
    pub show_monitor: bool,
}

impl Default for WorkspaceLayout {
    fn default() -> Self {
        Self {
            show_devices_gatt: true,
            show_plot: true,
            show_protocol: true,
            show_replay: true,
            show_bookmarks: true,
            show_monitor: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceFile {
    pub version: u32,
    pub name: String,
    pub saved_at: String,
    pub active_session_id: Option<u64>,
    pub sessions: Vec<WorkspaceSession>,
    pub layout: WorkspaceLayout,
}

impl Default for WorkspaceFile {
    fn default() -> Self {
        Self {
            version: WORKSPACE_VERSION,
            name: "Untitled Workspace".to_owned(),
            saved_at: String::new(),
            active_session_id: None,
            sessions: vec![WorkspaceSession::default()],
            layout: WorkspaceLayout::default(),
        }
    }
}

pub fn save_workspace(path: impl AsRef<Path>, workspace: &WorkspaceFile) -> Result<()> {
    let path = path.as_ref();
    let mut workspace = workspace.clone();
    workspace.version = WORKSPACE_VERSION;
    workspace.saved_at = now_string();
    let bytes = serde_json::to_vec_pretty(&workspace).context("序列化 Workspace 失败")?;
    fs::write(path, bytes).with_context(|| format!("无法写入 Workspace {}", path.display()))?;
    Ok(())
}

pub fn load_workspace(path: impl AsRef<Path>) -> Result<WorkspaceFile> {
    let path = path.as_ref();
    let bytes = fs::read(path).with_context(|| format!("无法读取 Workspace {}", path.display()))?;
    let workspace = serde_json::from_slice::<WorkspaceFile>(&bytes)
        .with_context(|| format!("无法解析 Workspace {}", path.display()))?;
    if workspace.version > WORKSPACE_VERSION {
        return Err(anyhow!(
            "Workspace version {} 比当前支持的 {} 更新",
            workspace.version,
            WORKSPACE_VERSION
        ));
    }
    Ok(workspace)
}

pub fn now_string() -> String {
    Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_workspace_gets_layout_defaults() {
        let json = r#"{
            "version": 1,
            "name": "test",
            "saved_at": "",
            "active_session_id": null,
            "sessions": []
        }"#;
        let workspace: WorkspaceFile = serde_json::from_str(json).unwrap();
        assert!(workspace.layout.show_monitor);
        assert!(workspace.layout.show_bookmarks);
    }
}
