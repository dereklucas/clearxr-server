use std::sync::Arc;

use tokio::sync::Mutex;

use crate::bonjour::BonjourService;
use crate::cloudxr::CloudXrService;
use crate::models::RuntimeSnapshot;
use crate::session_management::SessionManagementService;

#[derive(Clone, Default)]
pub struct AppState {
    inner: Arc<AppStateInner>,
}

#[derive(Default)]
struct AppStateInner {
    snapshot: Mutex<RuntimeSnapshot>,
    bonjour: Mutex<Option<BonjourService>>,
    cloudxr: Mutex<Option<CloudXrService>>,
    session_management: Mutex<Option<SessionManagementService>>,
}

impl AppState {
    pub async fn snapshot(&self) -> RuntimeSnapshot {
        self.inner.snapshot.lock().await.clone()
    }

    pub async fn update<F>(&self, apply: F) -> RuntimeSnapshot
    where
        F: FnOnce(&mut RuntimeSnapshot),
    {
        let mut snapshot = self.inner.snapshot.lock().await;
        apply(&mut snapshot);
        snapshot.clone()
    }

    pub async fn has_session_management(&self) -> bool {
        self.inner.session_management.lock().await.is_some()
    }

    pub async fn has_bonjour(&self) -> bool {
        self.inner.bonjour.lock().await.is_some()
    }

    pub async fn has_cloudxr(&self) -> bool {
        self.inner.cloudxr.lock().await.is_some()
    }

    pub async fn cloudxr(&self) -> Option<CloudXrService> {
        self.inner.cloudxr.lock().await.clone()
    }

    pub async fn replace_bonjour(&self, service: Option<BonjourService>) -> Option<BonjourService> {
        let mut current = self.inner.bonjour.lock().await;
        std::mem::replace(&mut *current, service)
    }

    pub async fn replace_cloudxr(&self, service: Option<CloudXrService>) -> Option<CloudXrService> {
        let mut current = self.inner.cloudxr.lock().await;
        std::mem::replace(&mut *current, service)
    }

    pub async fn replace_session_management(
        &self,
        service: Option<SessionManagementService>,
    ) -> Option<SessionManagementService> {
        let mut current = self.inner.session_management.lock().await;
        std::mem::replace(&mut *current, service)
    }
}
