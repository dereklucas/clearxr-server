use anyhow::Result;
use uuid::Uuid;

#[cfg(windows)]
pub fn get_or_create_server_id() -> Result<String> {
    use windows_registry::CURRENT_USER;

    let key = CURRENT_USER.create("SOFTWARE\\CloudXR")?;

    if let Ok(server_id) = key.get_string("ServerID") {
        if !server_id.is_empty() {
            return Ok(server_id);
        }
    }

    let server_id = Uuid::new_v4().simple().to_string();
    key.set_string("ServerID", &server_id)?;
    Ok(server_id)
}

#[cfg(not(windows))]
pub fn get_or_create_server_id() -> Result<String> {
    Ok(Uuid::new_v4().simple().to_string())
}

pub fn get_server_id_with_fallback() -> (String, bool) {
    match get_or_create_server_id() {
        Ok(server_id) => (server_id, false),
        Err(_) => (Uuid::new_v4().simple().to_string(), true),
    }
}
