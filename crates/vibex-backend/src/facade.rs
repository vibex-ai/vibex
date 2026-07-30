use std::sync::{Arc, Mutex};

use crate::{
    AgentBackend, BackendCapabilitySnapshot, DeviceBackend, FileBackend, GitBackend,
    ManagementBackend, TerminalBackend, WorkspaceBackend,
};

#[derive(Clone)]
pub struct BackendFacade {
    capabilities: Arc<Mutex<BackendCapabilitySnapshot>>,
    agent: Arc<dyn AgentBackend>,
    workspace: Arc<dyn WorkspaceBackend>,
    file: Arc<dyn FileBackend>,
    git: Arc<dyn GitBackend>,
    terminal: Arc<dyn TerminalBackend>,
    management: Arc<dyn ManagementBackend>,
    device: Arc<dyn DeviceBackend>,
}

impl BackendFacade {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        capabilities: BackendCapabilitySnapshot,
        agent: Arc<dyn AgentBackend>,
        workspace: Arc<dyn WorkspaceBackend>,
        file: Arc<dyn FileBackend>,
        git: Arc<dyn GitBackend>,
        terminal: Arc<dyn TerminalBackend>,
        management: Arc<dyn ManagementBackend>,
        device: Arc<dyn DeviceBackend>,
    ) -> Self {
        Self::new_shared(
            Arc::new(Mutex::new(capabilities)),
            agent,
            workspace,
            file,
            git,
            terminal,
            management,
            device,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_shared(
        capabilities: Arc<Mutex<BackendCapabilitySnapshot>>,
        agent: Arc<dyn AgentBackend>,
        workspace: Arc<dyn WorkspaceBackend>,
        file: Arc<dyn FileBackend>,
        git: Arc<dyn GitBackend>,
        terminal: Arc<dyn TerminalBackend>,
        management: Arc<dyn ManagementBackend>,
        device: Arc<dyn DeviceBackend>,
    ) -> Self {
        Self {
            capabilities,
            agent,
            workspace,
            file,
            git,
            terminal,
            management,
            device,
        }
    }

    pub fn capabilities(&self) -> BackendCapabilitySnapshot {
        self.capabilities
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn replace_capabilities(&self, capabilities: BackendCapabilitySnapshot) {
        *self
            .capabilities
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = capabilities;
    }

    pub fn agent(&self) -> &Arc<dyn AgentBackend> {
        &self.agent
    }

    pub fn workspace(&self) -> &Arc<dyn WorkspaceBackend> {
        &self.workspace
    }

    pub fn file(&self) -> &Arc<dyn FileBackend> {
        &self.file
    }

    pub fn git(&self) -> &Arc<dyn GitBackend> {
        &self.git
    }

    pub fn terminal(&self) -> &Arc<dyn TerminalBackend> {
        &self.terminal
    }

    pub fn management(&self) -> &Arc<dyn ManagementBackend> {
        &self.management
    }

    pub fn device(&self) -> &Arc<dyn DeviceBackend> {
        &self.device
    }
}
