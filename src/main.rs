use std::time::Duration;

const USAGE: &str = "usage: rubrowser [--no-js] [URL]
       rubrowser --screenshot FILE.png [--width N] [--height N] [--scale F] [--timeout SECS] [--no-js] URL";

fn main() {
    let mut args = std::env::args().skip(1);
    let mut url = None;
    let mut shot_file = None;
    let mut shot = rubrowser::platform::native::Shot {
        width: 1280,
        height: 900,
        scale: 1.0,
        timeout: Duration::from_secs(30),
        javascript: true,
    };
    while let Some(a) = args.next() {
        let mut value = |name: &str| {
            args.next().unwrap_or_else(|| {
                eprintln!("rubrowser: {name} needs a value\n{USAGE}");
                std::process::exit(2);
            })
        };
        match a.as_str() {
            "--screenshot" => shot_file = Some(value("--screenshot")),
            "--width" => shot.width = value("--width").parse().unwrap_or(shot.width),
            "--height" => shot.height = value("--height").parse().unwrap_or(shot.height),
            "--scale" => shot.scale = value("--scale").parse().unwrap_or(shot.scale),
            "--timeout" => {
                let secs: u64 = value("--timeout").parse().unwrap_or(30);
                shot.timeout = Duration::from_secs(secs);
            }
            "--no-js" => shot.javascript = false,
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            _ => url = Some(a),
        }
    }
    let result = match shot_file {
        Some(file) => match url {
            Some(u) => rubrowser::platform::native::screenshot(&u, &file, &shot),
            None => {
                eprintln!("{USAGE}");
                std::process::exit(2);
            }
        },
        None => rubrowser::platform::native::run(url, shot.javascript),
    };
    if let Err(e) = result {
        eprintln!("rubrowser: {e}");
        std::process::exit(1);
    }
}
