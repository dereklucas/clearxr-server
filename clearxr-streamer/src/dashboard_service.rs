//! Dashboard rendering service — wraps clearxr-dashboard.
//!
//! Started when a CloudXR session becomes active, stopped when it ends.
//! Renders the dashboard overlay and shares frames with the layer via SHM.

use clearxr_dashboard::DashboardService;

pub fn start() -> Result<DashboardService, String> {
    log::info!("Starting dashboard rendering service.");
    DashboardService::start()
}
