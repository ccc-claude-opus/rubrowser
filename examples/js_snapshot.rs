//! Writes `src/lang/prelude.rjss`: the rujse heap after `prelude.js`, which
//! js-rujse.wasm restores for every page instead of running the prelude.
//! Run by `scripts/build.sh` (on the build machine, natively).

fn main() {
    match rubrowser::lang::js_rujse::make_snapshot() {
        Ok(bytes) => {
            std::fs::write("src/lang/prelude.rjss", &bytes).expect("write src/lang/prelude.rjss");
            println!("src/lang/prelude.rjss: {} bytes", bytes.len());
        }
        Err(e) => {
            eprintln!("prelude snapshot failed: {e}");
            std::process::exit(1);
        }
    }
}
