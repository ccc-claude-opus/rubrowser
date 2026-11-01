fn main() {
    let url = std::env::args().nth(1);
    if let Err(e) = rubrowser::platform::native::run(url) {
        eprintln!("rubrowser: {e}");
        std::process::exit(1);
    }
}
