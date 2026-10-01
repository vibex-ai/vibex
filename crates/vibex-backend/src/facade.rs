use std::sync::{Arc, Mutex};

use crate::{
    AgentBackend, BackendCapabilitySnapshot, BrowserBackend, DeviceBackend, FileBackend,
    GitBackend, ManagementBackend, SidebarBackend, TerminalBackend, WorkspaceBackend,
};

#[derive(Clone)]
pub struct BackendFacade {
    capabilities: Arc<Mutex<BackendCapabilitySnapshot>>,
    agent: Arc<dyn AgentBackend>,
    workspace: Arc<dyn WorkspaceBackend>,
    file: Arc<dyn FileBackend>,
    git: Arc<dyn GitBackend>,
    terminal: Arc<dyn TerminalBackend>,
    browser: Arc<dyn BrowserBackend>,
    management: Arc<dyn ManagementBackend>,
    device: Arc<dyn DeviceBackend>,
    sidebar: Arc<dyn SidebarBackend>,
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
        browser: Arc<dyn BrowserBackend>,
        management: Arc<dyn ManagementBackend>,
        device: Arc<dyn DeviceBackend>,
        sidebar: Arc<dyn SidebarBackend>,
    ) -> Self {
        Self::new_shared(
            Arc::new(Mutex::new(capabilities)),
            agent,
            workspace,
            file,
            git,
            terminal,
            browser,
            management,
            device,
            sidebar,
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
        browser: Arc<dyn BrowserBackend>,
        management: Arc<dyn ManagementBackend>,
        device: Arc<dyn DeviceBackend>,
        sidebar: Arc<dyn SidebarBackend>,
    ) -> Self {
        Self {
            capabilities,
            agent,
            workspace,
            file,
            git,
            terminal,
            browser,
            management,
            device,
            sidebar,
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

    pub fn browser(&self) -> &Arc<dyn BrowserBackend> {
        &self.browser
    }

    pub fn management(&self) -> &Arc<dyn ManagementBackend> {
        &self.management
    }

    pub fn device(&self) -> &Arc<dyn DeviceBackend> {
        &self.device
    }

    pub fn sidebar(&self) -> &Arc<dyn SidebarBackend> {
        &self.sidebar
    }
}
