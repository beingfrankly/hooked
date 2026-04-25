use clap::Parser as _;

fn main() -> anyhow::Result<()> {
    let cli = hooked::cli::Cli::parse();
    hooked::run(cli)
}
