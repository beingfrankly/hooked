//! Output rendering for query subcommands.
//! Hand-rolled to maintain byte-for-byte parity with Python query.py.
//!
//! ## Format rules (derived from `_render()` in query.py)
//!
//! ### Table
//! - Column widths: `max(header_len, max(cell_str_len))` per column.
//! - Header line: headers left-padded (`ljust`) to column width, joined with `"  "` (two spaces).
//! - Separator line: `"-" * col_width` per column, joined with `"  "` (two spaces).
//! - Data rows: each cell converted to string (None → `""`), left-padded to column width,
//!   joined with `"  "`. Trailing spaces ARE present on the last column (Python ljust behaviour).
//! - Empty result set: emits `"(no results)"`.
//!
//! ### JSON
//! - `json.dumps(rows, indent=2, default=str)` — list of objects, 2-space indent.
//! - Empty result set: emits `"[]"`.
//! - Null cells serialise as JSON `null`.
//! - Non-string scalar cells serialise as their JSON type (numbers as numbers).
//!
//! ### CSV
//! - Python `csv.DictWriter` with default settings:
//!   - Comma delimiter.
//!   - Quoting: `QUOTE_MINIMAL` — cells containing `,`, `"`, or `\r\n` are double-quoted.
//!   - Embedded quotes escaped by doubling (`""` inside a quoted field).
//!   - Line terminator: `\r\n` (Python csv module default).
//! - Header row written first, then data rows.
//! - Empty result set: still emits the header line.
//!
//! ### Markdown
//! - Column widths: same formula as Table.
//! - Header line: `| h1 | h2 | … |` with headers left-padded to column width.
//! - Separator line: `| ---… | ---… | … |` where each block is `"-" * col_width`.
//! - Data rows: `| cell1 | cell2 | … |` with cells left-padded to column width.
//! - Trailing spaces ARE present on the last column (mirrors Python ljust).
//!
//! ### Null / None handling
//! Python converts `None` values with `str(r.get(h, "") or "")` — None becomes `""`.
//! In `Cell::Null` we output an empty string for all formats except JSON (where it is `null`).
//!
//! ### Unicode width
//! Python uses `len()` (codepoint count), NOT display width.  We mirror this by using
//! `str.chars().count()` rather than any display-width library.

use crate::cli::OutputFormat;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A single typed cell value for rendering.
#[derive(Clone, Debug)]
pub enum Cell {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Null,
}

impl Cell {
    /// Convert to display string (Python `str(v) or ""`).
    /// `None` / `Null` → `""`.
    pub fn display(&self) -> String {
        match self {
            Cell::Str(s) => s.clone(),
            Cell::Int(i) => i.to_string(),
            Cell::Float(f) => f.to_string(),
            Cell::Bool(b) => b.to_string(),
            Cell::Null => String::new(),
        }
    }

    /// Codepoint length of display string (mirrors Python `len()`).
    pub fn display_len(&self) -> usize {
        self.display().chars().count()
    }
}

/// A single row of typed cells for rendering.
pub type Row = Vec<Cell>;

/// A table of rows with named headers.
pub struct Table {
    pub headers: Vec<String>,
    pub rows: Vec<Row>,
}

impl Table {
    /// Create a new table.
    pub fn new(headers: Vec<String>, rows: Vec<Row>) -> Self {
        Table { headers, rows }
    }

    /// Render this table into a string using the given output format.
    ///
    /// The returned string does NOT include a trailing newline — callers
    /// should `println!("{}", table.render(...))` or similar.
    pub fn render(&self, fmt: &OutputFormat) -> String {
        match fmt {
            OutputFormat::Table => self.render_table(),
            OutputFormat::Json => self.render_json(),
            OutputFormat::Csv => self.render_csv(),
            OutputFormat::Markdown => self.render_markdown(),
        }
    }

    // -----------------------------------------------------------------------
    // Table format
    // -----------------------------------------------------------------------

    fn render_table(&self) -> String {
        if self.rows.is_empty() && self.headers.is_empty() {
            return "(no results)".to_string();
        }
        if self.rows.is_empty() {
            return "(no results)".to_string();
        }

        let col_widths = self.column_widths();

        // Header line
        let header_line: String = self
            .headers
            .iter()
            .enumerate()
            .map(|(i, h)| ljust(h, col_widths[i]))
            .collect::<Vec<_>>()
            .join("  ");

        // Separator line
        let sep_line: String = col_widths
            .iter()
            .map(|&w| "-".repeat(w))
            .collect::<Vec<_>>()
            .join("  ");

        // Data rows
        let mut lines = vec![header_line, sep_line];
        for row in &self.rows {
            let cells: String = self
                .headers
                .iter()
                .enumerate()
                .map(|(i, _)| {
                    let s = row.get(i).map(|c| c.display()).unwrap_or_default();
                    ljust(&s, col_widths[i])
                })
                .collect::<Vec<_>>()
                .join("  ");
            lines.push(cells);
        }

        lines.join("\n")
    }

    // -----------------------------------------------------------------------
    // JSON format
    // -----------------------------------------------------------------------

    fn render_json(&self) -> String {
        if self.rows.is_empty() {
            return "[]".to_string();
        }

        // Build a Vec<serde_json::Map> mirroring Python's list-of-dicts
        let json_rows: Vec<serde_json::Value> = self
            .rows
            .iter()
            .map(|row| {
                let mut map = serde_json::Map::new();
                for (i, header) in self.headers.iter().enumerate() {
                    let val = match row.get(i) {
                        Some(Cell::Str(s)) => serde_json::Value::String(s.clone()),
                        Some(Cell::Int(n)) => serde_json::Value::Number((*n).into()),
                        Some(Cell::Float(f)) => serde_json::Number::from_f64(*f)
                            .map(serde_json::Value::Number)
                            .unwrap_or(serde_json::Value::Null),
                        Some(Cell::Bool(b)) => serde_json::Value::Bool(*b),
                        Some(Cell::Null) | None => serde_json::Value::Null,
                    };
                    map.insert(header.clone(), val);
                }
                serde_json::Value::Object(map)
            })
            .collect();

        // Python: json.dumps(rows, indent=2, default=str) → 2-space indent
        serde_json::to_string_pretty(&serde_json::Value::Array(json_rows))
            .unwrap_or_else(|_| "[]".to_string())
    }

    // -----------------------------------------------------------------------
    // CSV format
    // -----------------------------------------------------------------------

    fn render_csv(&self) -> String {
        let mut out = String::new();

        // Header row
        write_csv_row(&mut out, &self.headers);

        // Data rows
        for row in &self.rows {
            let cells: Vec<String> = self
                .headers
                .iter()
                .enumerate()
                .map(|(i, _)| row.get(i).map(|c| c.display()).unwrap_or_default())
                .collect();
            write_csv_row(&mut out, &cells);
        }

        out
    }

    // -----------------------------------------------------------------------
    // Markdown format
    // -----------------------------------------------------------------------

    fn render_markdown(&self) -> String {
        if self.rows.is_empty() {
            // Still render headers for an empty table
            let col_widths = self.header_only_widths();
            let header_line = format_md_row(&self.headers, &col_widths);
            let sep_line = format_md_sep(&col_widths);
            return format!("{}\n{}", header_line, sep_line);
        }

        let col_widths = self.column_widths();

        let header_line = format_md_row(&self.headers, &col_widths);
        let sep_line = format_md_sep(&col_widths);

        let mut lines = vec![header_line, sep_line];
        for row in &self.rows {
            let cells: Vec<String> = self
                .headers
                .iter()
                .enumerate()
                .map(|(i, _)| row.get(i).map(|c| c.display()).unwrap_or_default())
                .collect();
            lines.push(format_md_row(&cells, &col_widths));
        }

        lines.join("\n")
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Compute per-column widths: `max(header_codepoints, max(cell_codepoints))`.
    /// Python: `max(len(h), max(len(str(r.get(h, "") or "")) for r in rows))`
    fn column_widths(&self) -> Vec<usize> {
        self.headers
            .iter()
            .enumerate()
            .map(|(i, h)| {
                let header_len = h.chars().count();
                let max_cell = self
                    .rows
                    .iter()
                    .map(|row| row.get(i).map(|c| c.display_len()).unwrap_or(0))
                    .max()
                    .unwrap_or(0);
                header_len.max(max_cell)
            })
            .collect()
    }

    /// Column widths when there are no rows (header width only).
    fn header_only_widths(&self) -> Vec<usize> {
        self.headers.iter().map(|h| h.chars().count()).collect()
    }
}

// ---------------------------------------------------------------------------
// Format helpers
// ---------------------------------------------------------------------------

/// Left-justify `s` in a field of `width` codepoints (space padding on right).
/// Mirrors Python's `str.ljust(width)`.
fn ljust(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(width - len))
    }
}

/// Write one CSV row (fields + `\r\n` terminator) into `buf`.
///
/// Python `csv.writer` default: `QUOTE_MINIMAL` — fields that contain
/// commas, double-quotes, or line-ending characters are wrapped in `"…"`.
/// Embedded double-quotes are escaped by doubling (`"` → `""`).
fn write_csv_row(buf: &mut String, fields: &[String]) {
    for (i, field) in fields.iter().enumerate() {
        if i > 0 {
            buf.push(',');
        }
        if needs_csv_quoting(field) {
            buf.push('"');
            for ch in field.chars() {
                if ch == '"' {
                    buf.push('"'); // double the quote
                }
                buf.push(ch);
            }
            buf.push('"');
        } else {
            buf.push_str(field);
        }
    }
    buf.push_str("\r\n"); // Python csv default line terminator
}

/// Return true if the field requires CSV quoting.
fn needs_csv_quoting(s: &str) -> bool {
    s.contains(',') || s.contains('"') || s.contains('\r') || s.contains('\n')
}

/// Format one Markdown table row: `| c1 | c2 | … |`
fn format_md_row(cells: &[String], widths: &[usize]) -> String {
    let parts: String = cells
        .iter()
        .enumerate()
        .map(|(i, c)| ljust(c, widths[i]))
        .collect::<Vec<_>>()
        .join(" | ");
    format!("| {} |", parts)
}

/// Format the Markdown separator row: `| --- | --- | … |`
fn format_md_sep(widths: &[usize]) -> String {
    let parts: String = widths
        .iter()
        .map(|&w| "-".repeat(w))
        .collect::<Vec<_>>()
        .join(" | ");
    format!("| {} |", parts)
}

// ---------------------------------------------------------------------------
// Sparkline helper
// -----------------------------------------------------------------------
// Python: _SPARK_CHARS = " ▁▂▃▄▅▆▇█"  (space + 8 block elements = 9 chars)
// `idx = int((v / max_v) * (len(_SPARK_CHARS) - 1))`

const SPARK_CHARS: &[char] = &[' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Render a slice of floats as an ASCII sparkline.
/// Mirrors Python's `_sparkline()` in query.py.
pub fn sparkline(values: &[f64]) -> String {
    if values.is_empty() {
        return String::new();
    }
    let max_v = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let max_v = if max_v == 0.0 { 1.0 } else { max_v };
    values
        .iter()
        .map(|&v| {
            let idx = ((v / max_v) * (SPARK_CHARS.len() - 1) as f64) as usize;
            SPARK_CHARS[idx.min(SPARK_CHARS.len() - 1)]
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::OutputFormat;

    fn make_table() -> Table {
        Table::new(
            vec!["ID".into(), "Name".into(), "Count".into()],
            vec![
                vec![Cell::Int(1), Cell::Str("Alice".into()), Cell::Int(42)],
                vec![Cell::Int(2), Cell::Str("Bob".into()), Cell::Int(7)],
                vec![Cell::Int(3), Cell::Str("Charlie".into()), Cell::Int(100)],
            ],
        )
    }

    // -----------------------------------------------------------------------
    // Table tests
    // -----------------------------------------------------------------------

    #[test]
    fn table_basic() {
        let t = make_table();
        let out = t.render(&OutputFormat::Table);
        // col widths: ID=2, Name=7(Charlie), Count=5
        let expected = "\
ID  Name     Count\n\
--  -------  -----\n\
1   Alice    42   \n\
2   Bob      7    \n\
3   Charlie  100  ";
        assert_eq!(out, expected);
    }

    #[test]
    fn table_empty_rows() {
        let t = Table::new(vec!["ID".into(), "Name".into()], vec![]);
        let out = t.render(&OutputFormat::Table);
        assert_eq!(out, "(no results)");
    }

    #[test]
    fn table_long_cell() {
        // Cell longer than header → column expands to cell width
        let t = Table::new(
            vec!["X".into()],
            vec![
                vec![Cell::Str("short".into())],
                vec![Cell::Str("a very long cell value".into())],
            ],
        );
        let out = t.render(&OutputFormat::Table);
        // col width = max(1, 22) = 22
        let expected = "\
X                     \n\
----------------------\n\
short                 \n\
a very long cell value";
        assert_eq!(out, expected);
    }

    #[test]
    fn table_unicode_widths() {
        // Python len("日本語") == 3 (codepoints), NOT display width 6
        // Python len("é") == 1 (single codepoint U+00E9)
        let t = Table::new(
            vec!["Word".into()],
            vec![
                vec![Cell::Str("日本語".into())],
                vec![Cell::Str("é".into())],
            ],
        );
        let out = t.render(&OutputFormat::Table);
        // col width = max(4, max(3, 1)) = 4
        // "日本語" has 3 codepoints → padded to 4 → "日本語 "
        // "é" has 1 codepoint → padded to 4 → "é   "
        let expected = "Word\n----\n日本語 \né   ";
        assert_eq!(out, expected);
    }

    #[test]
    fn table_null_cell() {
        let t = Table::new(
            vec!["A".into(), "B".into()],
            vec![vec![Cell::Null, Cell::Str("x".into())]],
        );
        let out = t.render(&OutputFormat::Table);
        // col widths: A=1, B=1
        let expected = "A  B\n-  -\n   x";
        assert_eq!(out, expected);
    }

    // -----------------------------------------------------------------------
    // JSON tests
    // -----------------------------------------------------------------------

    #[test]
    fn json_basic() {
        let t = Table::new(
            vec!["id".into(), "name".into()],
            vec![
                vec![Cell::Int(1), Cell::Str("Alice".into())],
                vec![Cell::Int(2), Cell::Null],
            ],
        );
        let out = t.render(&OutputFormat::Json);
        let expected = r#"[
  {
    "id": 1,
    "name": "Alice"
  },
  {
    "id": 2,
    "name": null
  }
]"#;
        assert_eq!(out, expected);
    }

    #[test]
    fn json_empty() {
        let t = Table::new(vec!["id".into()], vec![]);
        let out = t.render(&OutputFormat::Json);
        assert_eq!(out, "[]");
    }

    // -----------------------------------------------------------------------
    // CSV tests
    // -----------------------------------------------------------------------

    #[test]
    fn csv_basic() {
        let t = Table::new(
            vec!["id".into(), "name".into(), "count".into()],
            vec![
                vec![Cell::Int(1), Cell::Str("Alice".into()), Cell::Int(42)],
                vec![Cell::Int(2), Cell::Str("Bob".into()), Cell::Int(7)],
            ],
        );
        let out = t.render(&OutputFormat::Csv);
        let expected = "id,name,count\r\n1,Alice,42\r\n2,Bob,7\r\n";
        assert_eq!(out, expected);
    }

    #[test]
    fn csv_quoted_field() {
        // Cell with comma → field must be quoted
        let t = Table::new(
            vec!["phrase".into()],
            vec![
                vec![Cell::Str("hello, world".into())],
                vec![Cell::Str("say \"hi\"".into())],
            ],
        );
        let out = t.render(&OutputFormat::Csv);
        let expected = "phrase\r\n\"hello, world\"\r\n\"say \"\"hi\"\"\"\r\n";
        assert_eq!(out, expected);
    }

    // -----------------------------------------------------------------------
    // Markdown tests
    // -----------------------------------------------------------------------

    #[test]
    fn markdown_basic() {
        let t = make_table();
        let out = t.render(&OutputFormat::Markdown);
        // col widths: ID=2, Name=7, Count=5
        let expected = "\
| ID | Name    | Count |\n\
| -- | ------- | ----- |\n\
| 1  | Alice   | 42    |\n\
| 2  | Bob     | 7     |\n\
| 3  | Charlie | 100   |";
        assert_eq!(out, expected);
    }

    #[test]
    fn markdown_empty_rows() {
        let t = Table::new(vec!["ID".into(), "Name".into()], vec![]);
        let out = t.render(&OutputFormat::Markdown);
        let expected = "| ID | Name |\n| -- | ---- |";
        assert_eq!(out, expected);
    }

    // -----------------------------------------------------------------------
    // Sparkline tests
    // -----------------------------------------------------------------------

    #[test]
    fn sparkline_basic() {
        let values = vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        let s = sparkline(&values);
        assert_eq!(s, " ▁▂▃▄▅▆▇█");
    }

    #[test]
    fn sparkline_empty() {
        assert_eq!(sparkline(&[]), "");
    }

    #[test]
    fn sparkline_all_zero() {
        // max_v = 0 → clamped to 1, all indices = 0 → all spaces
        let s = sparkline(&[0.0, 0.0, 0.0]);
        assert_eq!(s, "   ");
    }
}
