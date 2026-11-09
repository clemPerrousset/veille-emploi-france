//! Point d'entrée : analyse des arguments puis délégation à `veille_emploi::app`.

use veille_emploi::schedule::{parse_args, Command};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = match parse_args(&args) {
        Ok(c) => c,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };
    veille_emploi::app::init_tracing();
    let code = match cmd {
        Command::Serve => veille_emploi::app::serve().await,
        Command::Once(job) => veille_emploi::app::once(job).await,
    };
    std::process::exit(code);
}
