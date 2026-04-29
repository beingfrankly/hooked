use clap::Parser as _;

fn main() {
    let cli = hooked::cli::Cli::parse();
    match hooked::run(cli) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("{:?}", e);
            std::process::exit(hooked::exit::ERROR);
        }
    }
}
