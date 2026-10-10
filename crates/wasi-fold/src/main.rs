//! `krate-wasi-fold <in.wasm> <out.wasm>`: fold a linked core module's
//! leftover WASI imports into the answers krate-c-runtime links into it, then
//! componentize the result with `wasm-tools component new` (no adapter). See
//! the library's documentation for why.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: krate-wasi-fold <in.wasm> <out.wasm>");
        std::process::exit(2);
    }
    let wasm = match std::fs::read(&args[1]) {
        Ok(bytes) => bytes,
        Err(err) => {
            eprintln!("krate-wasi-fold: {}: {err}", args[1]);
            std::process::exit(1);
        }
    };
    match krate_wasi_fold::fold(&wasm) {
        Ok((out, folded)) => {
            if let Err(err) = std::fs::write(&args[2], out) {
                eprintln!("krate-wasi-fold: {}: {err}", args[2]);
                std::process::exit(1);
            }
            if folded.folded.is_empty() {
                println!("no WASI imports left; nothing to fold");
            } else {
                println!("folded {}", folded.folded.join(", "));
            }
        }
        Err(err) => {
            eprintln!("krate-wasi-fold: {err}");
            std::process::exit(1);
        }
    }
}
