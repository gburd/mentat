//! Tiny REPL-eval bin for comparing the port against the mino oracle:
//! `cargo run -q -p mino-rs --example mino-eval -- '(EXPR)'` prints `(pr-str EXPR)`.
use mino_rs::eval::Interp;
use mino_rs::printer::print_str;

fn main() {
    let src: String = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let mut it = Interp::new();
    match it.eval_str(&src) {
        Ok(v) => println!("{}", print_str(&v)),
        Err(t) => eprintln!("THROW: {}", print_str(&t.0)),
    }
}
