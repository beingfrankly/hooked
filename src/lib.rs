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
#[cfg(test)]
mod test_utils;

pub fn run(cli: cli::Cli) -> anyhow::Result<i32> {
    let fmt = cli.effective_format();
    use cli::Command::*;
    match cli.command {
        Summary(a) => { cmd::summary(&a, &fmt)?; Ok(0) }
        Sessions(a) => { cmd::sessions(&a, &fmt)?; Ok(0) }
        Session(a) => { cmd::session(&a, &fmt)?; Ok(0) }
        Last(a) => { cmd::last(&a, &fmt)?; Ok(0) }
        Chain(a) => { cmd::chain(&a, &fmt)?; Ok(0) }
        Tools(a) => { cmd::tools(&a, &fmt)?; Ok(0) }
        Agents(a) => { cmd::agents(&a, &fmt)?; Ok(0) }
        Skills(a) => { cmd::skills(&a, &fmt)?; Ok(0) }
        Failures(a) => { cmd::failures(&a, &fmt)?; Ok(0) }
        BeforeStop(a) => { cmd::before_stop(&a, &fmt)?; Ok(0) }
        Compactions(a) => { cmd::compactions(&a, &fmt)?; Ok(0) }
        Search(a) => { cmd::search(&a, &fmt)?; Ok(0) }
        Configs(a) => { cmd::configs(&a, &fmt)?; Ok(0) }
        Health(a) => { cmd::health(&a, &fmt)?; Ok(0) }
        Init(a) => { cmd::init(&a)?; Ok(0) }
        Tail(a) => cmd::tail(&a),
        Diff(a) => { cmd::diff(&a, &fmt)?; Ok(0) }
        Trends(a) => { cmd::trends(&a, &fmt)?; Ok(0) }
        Slow(a) => { cmd::slow(&a, &fmt)?; Ok(0) }
        Tokens(a) => { cmd::tokens(&a, &fmt)?; Ok(0) }
        Ingest(a) => { cmd::ingest(&a)?; Ok(0) }
        Label(a) => { cmd::label(&a)?; Ok(0) }
        Annotate(a) => { cmd::annotate(&a)?; Ok(0) }
        Replay(a) => { cmd::replay(&a, &fmt)?; Ok(0) }
        Prune(a) => { cmd::prune(&a)?; Ok(0) }
        Export(a) => { cmd::export(&a)?; Ok(0) }
        Rebuild(a) => { cmd::rebuild(&a)?; Ok(0) }
        Backup(a) => { cmd::backup(&a)?; Ok(0) }
        Sql(a) => { cmd::sql(&a, &fmt)?; Ok(0) }
        AppendDaily(a) => { cmd::append_daily(&a)?; Ok(0) }
        ImportLegacy(a) => { cmd::import_legacy(&a)?; Ok(0) }
    }
}
