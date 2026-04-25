//! Implementation of the `trends` subcommand.
//!
//! Mirrors Python `cmd_trends` in query.py.
//!
//! SQL: aggregate sessions table by date(started_at) for the last `window` days,
//! applying the aggregation expression for the chosen metric:
//!   sessions  → COUNT(DISTINCT session_id)
//!   tool_calls → SUM(total_tool_calls)
//!   failures  → SUM(total_failures)
//!   prompts   → SUM(total_prompts)
//!   duration  → AVG(CAST((julianday(ended_at)-julianday(started_at))*86400000 AS INTEGER))
//!
//! For table/csv/markdown output: prints metric name + sparkline header, then a
//! two-column table (date, <metric>).
//! For JSON output: prints the raw array directly (mirrors Python).

use crate::cli::{OutputFormat, TrendsArgs, TrendsMetric};
use crate::dbh;
use crate::render::{Cell, Row, Table, sparkline};
use rusqlite::Connection;

struct TrendRow {
    date: String,
    value: f64,
}

fn metric_sql(metric: &TrendsMetric) -> &'static str {
    match metric {
        TrendsMetric::Sessions => "COUNT(DISTINCT session_id)",
        TrendsMetric::ToolCalls => "SUM(total_tool_calls)",
        TrendsMetric::Failures => "SUM(total_failures)",
        TrendsMetric::Prompts => "SUM(total_prompts)",
        TrendsMetric::Duration => {
            "AVG(CAST((julianday(ended_at) - julianday(started_at)) * 86400000 AS INTEGER))"
        }
    }
}

fn metric_name(metric: &TrendsMetric) -> &'static str {
    match metric {
        TrendsMetric::Sessions => "sessions",
        TrendsMetric::ToolCalls => "tool_calls",
        TrendsMetric::Failures => "failures",
        TrendsMetric::Prompts => "prompts",
        TrendsMetric::Duration => "duration",
    }
}

fn run_query(conn: &Connection, args: &TrendsArgs) -> anyhow::Result<Vec<TrendRow>> {
    let since = {
        let d = chrono::Utc::now() - chrono::Duration::days(i64::from(args.window));
        d.format("%Y-%m-%d").to_string()
    };

    let agg_expr = metric_sql(&args.metric);
    let sql = format!(
        "SELECT
            date(started_at) AS date,
            {agg_expr} AS value
        FROM sessions
        WHERE date(started_at) >= ?1
        GROUP BY date(started_at)
        ORDER BY date(started_at)"
    );

    let mut stmt = conn.prepare(&sql)?;

    let rows = stmt
        .query_map(rusqlite::params![since], |row| {
            Ok(TrendRow {
                date: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                value: row.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(rows)
}

fn build_table(rows: Vec<TrendRow>, metric: &TrendsMetric) -> Table {
    let m_name = metric_name(metric).to_string();
    let headers = vec!["date".to_string(), m_name.clone()];

    let data_rows: Vec<Row> = rows
        .into_iter()
        .map(|r| {
            vec![
                Cell::Str(r.date),
                // Mirror Python: value shown as-is (float or int)
                Cell::Float(r.value),
            ]
        })
        .collect();

    Table::new(headers, data_rows)
}

pub fn trends(args: &TrendsArgs, fmt: &OutputFormat) -> anyhow::Result<()> {
    let _ = dbh::auto_ingest()?;
    let conn = dbh::open_db()?;
    let rows = run_query(&conn, args)?;

    if rows.is_empty() {
        println!("No data.");
        return Ok(());
    }

    // Mirror Python: for JSON, print raw array; for table/csv/md, print sparkline header first
    match fmt {
        OutputFormat::Json => {
            // Build JSON directly
            let m_name = metric_name(&args.metric);
            let json_rows: Vec<serde_json::Value> = rows
                .iter()
                .map(|r| {
                    let mut map = serde_json::Map::new();
                    map.insert(
                        "date".to_string(),
                        serde_json::Value::String(r.date.clone()),
                    );
                    map.insert(
                        m_name.to_string(),
                        serde_json::Number::from_f64(r.value)
                            .map(serde_json::Value::Number)
                            .unwrap_or(serde_json::Value::Null),
                    );
                    serde_json::Value::Object(map)
                })
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::Value::Array(json_rows))
                    .unwrap_or_else(|_| "[]".to_string())
            );
        }
        _ => {
            let values: Vec<f64> = rows.iter().map(|r| r.value).collect();
            let spark = sparkline(&values);
            println!(
                "Metric: {} (last {} days)",
                metric_name(&args.metric),
                args.window
            );
            println!("Sparkline: {}", spark);
            println!();
            let table = build_table(rows, &args.metric);
            print!("{}", table.render(fmt));
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{TrendsArgs, TrendsMetric};
    use crate::schema::SCHEMA_V4_DDL;
    use rusqlite::Connection;

    fn in_memory_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory DB");
        conn.execute_batch(SCHEMA_V4_DDL).expect("apply schema DDL");
        conn
    }

    fn insert_session(
        conn: &Connection,
        session_id: &str,
        started_at: &str,
        tool_calls: i64,
        failures: i64,
    ) {
        conn.execute(
            "INSERT INTO sessions (session_id, started_at, total_tool_calls, total_failures)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![session_id, started_at, tool_calls, failures],
        )
        .expect("insert session");
    }

    fn default_args(metric: TrendsMetric) -> TrendsArgs {
        TrendsArgs {
            metric,
            window: 9999,
        }
    }

    #[test]
    fn returns_empty_when_no_data() {
        let conn = in_memory_conn();
        let args = default_args(TrendsMetric::ToolCalls);
        let rows = run_query(&conn, &args).expect("run_query");
        assert!(rows.is_empty());
    }

    #[test]
    fn aggregates_tool_calls_by_date() {
        let conn = in_memory_conn();
        insert_session(&conn, "s1", "2024-01-15T10:00:00Z", 5, 1);
        insert_session(&conn, "s2", "2024-01-15T14:00:00Z", 3, 0);
        insert_session(&conn, "s3", "2024-01-16T09:00:00Z", 7, 2);

        let args = default_args(TrendsMetric::ToolCalls);
        let rows = run_query(&conn, &args).expect("run_query");

        // Should be ordered ASC by date
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].date, "2024-01-15");
        assert_eq!(rows[0].value, 8.0); // 5 + 3
        assert_eq!(rows[1].date, "2024-01-16");
        assert_eq!(rows[1].value, 7.0);
    }

    #[test]
    fn builds_table_with_metric_column_name() {
        let conn = in_memory_conn();
        insert_session(&conn, "s1", "2024-01-15T10:00:00Z", 5, 1);

        let args = default_args(TrendsMetric::Failures);
        let rows = run_query(&conn, &args).expect("run_query");
        let table = build_table(rows, &args.metric);

        assert_eq!(table.headers, vec!["date", "failures"]);
    }
}
