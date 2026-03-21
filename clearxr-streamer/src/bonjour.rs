use std::net::IpAddr;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use log::info;
use mdns_sd::{ServiceDaemon, ServiceInfo};

use crate::models::AppConfig;
use crate::protocol::BUNDLE_ID_KEY;

pub const SERVICE_TYPE: &str = "_apple-foveated-streaming._tcp.local.";
const APPLE_SERVICE_NAME_LEN_MAX: u8 = 30;

pub struct BonjourService {
    daemon: ServiceDaemon,
    fullname: String,
}

impl BonjourService {
    pub fn start(config: &AppConfig) -> Result<Self> {
        let advertise_addr: IpAddr = config
            .host_address
            .parse()
            .with_context(|| format!("invalid host address '{}'", config.host_address))?;

        if advertise_addr.is_unspecified() {
            bail!("Bonjour requires a concrete host address, not 0.0.0.0 or ::");
        }

        let daemon = ServiceDaemon::new().context("failed to create the mDNS daemon")?;
        daemon
            .set_service_name_len_max(APPLE_SERVICE_NAME_LEN_MAX)
            .context("failed to override the mDNS service name length limit")?;
        let instance_name = hostname_label();
        let host_name = format!("{instance_name}.local.");
        let properties = [(BUNDLE_ID_KEY, config.bundle_id.as_str())];
        let service = ServiceInfo::new(
            SERVICE_TYPE,
            &instance_name,
            &host_name,
            &config.host_address,
            config.port,
            &properties[..],
        )
        .context("failed to build the mDNS service advertisement")?;
        let fullname = service.get_fullname().to_string();

        daemon
            .register(service)
            .context("failed to register the mDNS service")?;

        info!(
            "Advertising {} at {}:{} for bundle {}",
            SERVICE_TYPE, config.host_address, config.port, config.bundle_id
        );

        Ok(Self { daemon, fullname })
    }

    pub fn stop(self) {
        info!("Stopping Bonjour advertisement {}", self.fullname);

        if let Ok(receiver) = self.daemon.unregister(&self.fullname) {
            let _ = receiver.recv_timeout(Duration::from_secs(1));
        }

        if let Ok(receiver) = self.daemon.shutdown() {
            let _ = receiver.recv_timeout(Duration::from_secs(1));
        }
    }
}

fn hostname_label() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "streaming-session".to_string())
}
