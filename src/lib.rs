//! `hooked` — telemetry ingest & query for Claude Code.
//!
//! ## Conventions
//! - CLI boundaries use `anyhow::Result<T>` and `anyhow::Context`.
//! - Library modules define `thiserror` enums where error-type discrimination matters.
//! - Stderr messages use the `[component] LEVEL: msg` format via [`logging`].
//! - Exit codes from [`exit`]: `SUCCESS` (0), `ERROR` (1), `INTERRUPTED` (130).

pub mod cli;
pub mod cmd;
pub mod dbh;
pub mod enrich;
pub mod envelope;
pub mod exit;
pub mod ingest;
pub mod logging;
pub mod parity;
pub mod paths;
pub mod render;
pub mod schema;
// pub mod error;   // add when we introduce the shared error enum

pub fn run(cli: cli::Cli) -> anyhow::Result<()> {
    let fmt = cli.effective_format();
    use cli::Command::*;
    match cli.command {
        Summary(a) => cmd::summary(&a, &fmt),
        Sessions(a) => cmd::sessions(&a, &fmt),
        Session(a) => cmd::session(&a, &fmt),
        Last(a) => cmd::last(&a, &fmt),
        Chain(a) => cmd::chain(&a, &fmt),
        Tools(a) => cmd::tools(&a, &fmt),
        Agents(a) => cmd::agents(&a, &fmt),
        Skills(a) => cmd::skills(&a, &fmt),
        Failures(a) => cmd::failures(&a, &fmt),
        BeforeStop(a) => cmd::before_stop(&a, &fmt),
        Compactions(a) => cmd::compactions(&a, &fmt),
        Search(a) => cmd::search(&a, &fmt),
        Configs(a) => cmd::configs(&a, &fmt),
        Health(a) => cmd::health(&a, &fmt),
        Tail(a) => cmd::tail(&a),
        Diff(a) => cmd::diff(&a, &fmt),
        Trends(a) => cmd::trends(&a, &fmt),
        Slow(a) => cmd::slow(&a, &fmt),
        Tokens(a) => cmd::tokens(&a, &fmt),
        Ingest(a) => cmd::ingest(&a),
        Label(a) => cmd::label(&a),
        Annotate(a) => cmd::annotate(&a),
        Replay(a) => cmd::replay(&a, &fmt),
        Prune(a) => cmd::prune(&a),
        Export(a) => cmd::export(&a),
        Rebuild(a) => cmd::rebuild(&a),
        Backup(a) => cmd::backup(&a),
        Sql(a) => cmd::sql(&a, &fmt),
        AppendDaily(a) => cmd::append_daily(&a),
        ImportLegacy(a) => cmd::import_legacy(&a),
    }
}
