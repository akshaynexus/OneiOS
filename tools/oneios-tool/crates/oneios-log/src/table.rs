//! Tables of report lines: aligned text, JSON, CSV.

use std::fmt::Write;

#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    /// A field the line does not have (a cut line, an older format).
    None,
    Int(i64),
    /// A number with its count of decimals.
    Float(f64, usize),
    Text(String),
}

impl Cell {
    pub fn int(v: Option<f64>) -> Cell {
        v.map_or(Cell::None, |v| Cell::Int(v as i64))
    }

    pub fn float(v: Option<f64>, decimals: usize) -> Cell {
        v.map_or(Cell::None, |v| Cell::Float(v, decimals))
    }

    pub fn text(v: impl Into<String>) -> Cell {
        Cell::Text(v.into())
    }

    fn shown(&self) -> String {
        match self {
            Cell::None => "-".to_string(),
            Cell::Int(v) => v.to_string(),
            Cell::Float(v, d) => format!("{v:.*}", d),
            Cell::Text(t) => t.clone(),
        }
    }
}

pub struct Table {
    pub name: String,
    pub columns: Vec<&'static str>,
    pub rows: Vec<Vec<Cell>>,
}

impl Table {
    pub fn new(name: &str, columns: &[&'static str]) -> Table {
        Table { name: name.to_string(), columns: columns.to_vec(), rows: Vec::new() }
    }

    pub fn push(&mut self, row: Vec<Cell>) {
        debug_assert_eq!(row.len(), self.columns.len());
        self.rows.push(row);
    }

    pub fn text(&self, out: &mut String) {
        let _ = writeln!(out, "== {} ({} rows)", self.name, self.rows.len());
        if self.rows.is_empty() {
            return;
        }
        let shown: Vec<Vec<String>> = self.rows.iter().map(|r| r.iter().map(Cell::shown).collect()).collect();
        let widths: Vec<usize> = (0..self.columns.len())
            .map(|c| shown.iter().map(|r| r[c].chars().count()).chain([self.columns[c].len()]).max().unwrap_or(0))
            .collect();
        let last = self.columns.len() - 1;
        let mut line = String::new();
        let mut emit = |cells: &[String], left: &dyn Fn(usize) -> bool, out: &mut String| {
            line.clear();
            for (c, cell) in cells.iter().enumerate() {
                if c > 0 {
                    line.push_str("  ");
                }
                let pad = widths[c] - cell.chars().count();
                if left(c) {
                    line.push_str(cell);
                    if c != last {
                        line.extend(std::iter::repeat_n(' ', pad));
                    }
                } else {
                    line.extend(std::iter::repeat_n(' ', pad));
                    line.push_str(cell);
                }
            }
            let _ = writeln!(out, "{}", line.trim_end());
        };
        // A column is left-aligned when it holds text.
        let is_text: Vec<bool> =
            (0..self.columns.len()).map(|c| self.rows.iter().any(|r| matches!(r[c], Cell::Text(_)))).collect();
        let header: Vec<String> = self.columns.iter().map(|c| c.to_string()).collect();
        emit(&header, &|c| is_text[c], out);
        for row in &shown {
            emit(row, &|c| is_text[c], out);
        }
    }

    pub fn csv(&self, out: &mut String) {
        let _ = writeln!(out, "# {}", self.name);
        let quote = |s: &str| {
            if s.contains([',', '"', '\n']) {
                format!("\"{}\"", s.replace('"', "\"\""))
            } else {
                s.to_string()
            }
        };
        let _ = writeln!(out, "{}", self.columns.iter().map(|c| quote(c)).collect::<Vec<_>>().join(","));
        for row in &self.rows {
            let cells: Vec<String> = row
                .iter()
                .map(|c| match c {
                    Cell::None => String::new(),
                    other => quote(&other.shown()),
                })
                .collect();
            let _ = writeln!(out, "{}", cells.join(","));
        }
    }

    pub fn json(&self, out: &mut String) {
        out.push_str("{\"table\": ");
        json_string(out, &self.name);
        out.push_str(", \"rows\": [");
        for (i, row) in self.rows.iter().enumerate() {
            out.push_str(if i == 0 { "\n  {" } else { ",\n  {" });
            for (c, cell) in row.iter().enumerate() {
                if c > 0 {
                    out.push_str(", ");
                }
                json_string(out, self.columns[c]);
                out.push_str(": ");
                match cell {
                    Cell::None => out.push_str("null"),
                    Cell::Int(v) => {
                        let _ = write!(out, "{v}");
                    }
                    Cell::Float(v, d) if v.is_finite() => {
                        let _ = write!(out, "{v:.*}", d);
                    }
                    Cell::Float(..) => out.push_str("null"),
                    Cell::Text(t) => json_string(out, t),
                }
            }
            out.push('}');
        }
        out.push_str(if self.rows.is_empty() { "]}" } else { "\n]}" });
    }
}

pub fn json_string(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Text,
    Json,
    Csv,
}

/// The tables in one format; `notes` are the lines after the tables (text only; JSON gets them as a list).
pub fn render(tables: &[Table], notes: &[String], format: Format) -> String {
    let mut out = String::new();
    match format {
        Format::Text => {
            for (i, t) in tables.iter().enumerate() {
                if i > 0 {
                    out.push('\n');
                }
                t.text(&mut out);
            }
            if !notes.is_empty() {
                out.push('\n');
            }
            for note in notes {
                let _ = writeln!(out, "{note}");
            }
        }
        Format::Csv => {
            for (i, t) in tables.iter().enumerate() {
                if i > 0 {
                    out.push('\n');
                }
                t.csv(&mut out);
            }
        }
        Format::Json => {
            out.push_str("{\"tables\": [\n");
            for (i, t) in tables.iter().enumerate() {
                if i > 0 {
                    out.push_str(",\n");
                }
                t.json(&mut out);
            }
            out.push_str("\n], \"notes\": [");
            for (i, note) in notes.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                json_string(&mut out, note);
            }
            out.push_str("]}\n");
        }
    }
    out
}
