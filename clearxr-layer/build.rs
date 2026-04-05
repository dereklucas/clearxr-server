fn main() {
    // Embed a human-readable UTC build timestamp so the log always shows
    // exactly which compiled DLL is loaded.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_secs();

    // Format as YYYY-MM-DD HH:MM:SS UTC without pulling in chrono.
    let (y, mo, d) = days_to_ymd(secs / 86400);
    let tod = secs % 86400;
    let (h, mi, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let ts = format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02} UTC");

    println!("cargo:rustc-env=CLEARXR_BUILD_TIME={ts}");
    println!("cargo:rerun-if-changed=build.rs");
}

fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}
