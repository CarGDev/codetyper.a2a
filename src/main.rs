#![allow(dead_code)]
mod check_models;
mod cli;
mod dispatcher;
mod eval;
mod hub;
mod launcher;
mod model_guard;
mod models;
mod ollama;
mod opencode;
mod paths;
mod stats_view;
mod sys;
mod targets;
mod trace;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Invoked through an `oc` symlink: behave as the `oc` launcher.
    let as_oc = std::env::args_os()
        .next()
        .and_then(|a| std::path::Path::new(&a).file_name().map(|n| n == "oc"))
        .unwrap_or(false);
    if as_oc {
        std::process::exit(launcher::oc::run(&args));
    }
    let rest = args.get(1..).unwrap_or(&[]);
    let code = match args.first().map(String::as_str) {
        Some("init") => cli::init(rest),
        Some("update") => cli::init(&[rest, &["--md-only".to_string()]].concat()),
        Some("hub") => hub::server::run(),
        Some("dispatcher") => dispatcher::dispatcher::run(),
        Some("oc") => launcher::oc::run(rest),
        Some("models") => models::run(rest),
        Some("check-models") => check_models::run(rest),
        Some("e2e-test") => hub::e2etest::run(),
        Some("eval") => eval::run(rest),
        Some("version" | "--version" | "-V") => {
            println!("codetypera2a {}", env!("CARGO_PKG_VERSION"));
            0
        }
        None | Some("help" | "--help" | "-h") => cli::help(),
        Some(other) => {
            eprintln!("unknown command: {other}\n");
            eprint!("{}", cli::HELP);
            2
        }
    };
    std::process::exit(code);
}
