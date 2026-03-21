fn main() {
    match clearxr_streamer_lib::handle_startup_mode() {
        Ok(true) => {}
        Ok(false) => clearxr_streamer_lib::run(),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
