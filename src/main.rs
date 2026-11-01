fn main() {
    if let Err(e) = rubrowser::platform::native::run() {
        eprintln!("rubrowser: {e}");
        std::process::exit(1);
    }
}
