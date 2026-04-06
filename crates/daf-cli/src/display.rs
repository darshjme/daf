//! Output formatting utilities for the DAF CLI.
//!
//! Handles table rendering, JSON output, human-readable sizes/durations,
//! status icons, and the startup banner.

use std::time::Duration;

use comfy_table::{presets, Attribute, Cell, CellAlignment, Color, ContentArrangement, Table};
use console::style;

// ---------------------------------------------------------------------------
// Banner
// ---------------------------------------------------------------------------

/// Print the DAF startup banner with version and host info.
pub fn banner() {
    let ver = env!("CARGO_PKG_VERSION");
    let host = hostname();

    let art = format!(
        r#"
  {}
  {}   {}
  {}
  {}
"#,
        style("  ____    _    _____").cyan().bold(),
        style(" |  _ \\  / \\  |  ___|").cyan().bold(),
        style(format!("v{ver}")).dim(),
        style(" | | | |/ _ \\ | |_   ").cyan().bold(),
        style(" | |_| / ___ \\|  _|  ").cyan().bold(),
    );

    eprintln!("{art}");
    eprintln!(
        "  {}  {}",
        style("|____/_/   \\_\\_|").cyan().bold(),
        style(format!("node: {host}")).dim(),
    );
    eprintln!("  {}", style("Darshj's Agent Framework").dim());
    eprintln!();
}

/// Best-effort hostname retrieval.
fn hostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("HOST"))
        .unwrap_or_else(|_| whoami())
}

fn whoami() -> String {
    std::env::var("USER").unwrap_or_else(|_| "unknown".into())
}

// ---------------------------------------------------------------------------
// Table helpers
// ---------------------------------------------------------------------------

/// Create a styled table with the given headers.
pub fn format_table(headers: &[&str]) -> Table {
    let mut table = Table::new();
    table
        .load_preset(presets::UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Dynamic);

    let header_cells: Vec<Cell> = headers
        .iter()
        .map(|h| {
            Cell::new(h)
                .set_alignment(CellAlignment::Left)
                .add_attribute(Attribute::Bold)
                .fg(Color::Cyan)
        })
        .collect();
    table.set_header(header_cells);

    table
}

// ---------------------------------------------------------------------------
// JSON helpers
// ---------------------------------------------------------------------------

/// Serialize a value to pretty-printed JSON and print to stdout.
pub fn format_json<T: serde::Serialize>(value: &T) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(value)?;
    println!("{json}");
    Ok(())
}

// ---------------------------------------------------------------------------
// Duration / size formatting
// ---------------------------------------------------------------------------

/// Render a [`Duration`] in human-friendly form: `2h 14m 8s`, `350ms`, etc.
pub fn format_duration(d: Duration) -> String {
    let total_secs = d.as_secs();
    if total_secs == 0 {
        return format!("{}ms", d.as_millis());
    }

    let days = total_secs / 86400;
    let hours = (total_secs % 86400) / 3600;
    let mins = (total_secs % 3600) / 60;
    let secs = total_secs % 60;

    let mut parts = Vec::with_capacity(4);
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if mins > 0 {
        parts.push(format!("{mins}m"));
    }
    if secs > 0 || parts.is_empty() {
        parts.push(format!("{secs}s"));
    }

    parts.join(" ")
}

/// Render a byte count in human-friendly form: `1.2 GiB`, `340 MiB`, etc.
pub fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;
    const TIB: u64 = 1024 * GIB;

    if bytes >= TIB {
        format!("{:.1} TiB", bytes as f64 / TIB as f64)
    } else if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

// ---------------------------------------------------------------------------
// Status icons
// ---------------------------------------------------------------------------

/// Return a colored status icon for terminal display.
///
/// | Status       | Icon |
/// |-------------|------|
/// | `ok`        | green checkmark |
/// | `changed`   | yellow delta |
/// | `failed`    | red cross |
/// | `running`   | blue arrow |
/// | `waiting`   | dim ellipsis |
/// | `skipped`   | dim dash |
/// | _other_     | white question mark |
pub fn status_icon(status: &str) -> String {
    match status.to_lowercase().as_str() {
        "ok" | "completed" | "healthy" | "passed" => {
            format!("{}", style("\u{2714}").green().bold()) // checkmark
        }
        "changed" | "updated" | "modified" => {
            format!("{}", style("\u{0394}").yellow().bold()) // delta
        }
        "failed" | "error" | "unhealthy" => {
            format!("{}", style("\u{2718}").red().bold()) // cross
        }
        "running" | "executing" | "active" => {
            format!("{}", style("\u{25B6}").blue().bold()) // play arrow
        }
        "waiting" | "pending" | "spawning" => {
            format!("{}", style("\u{2026}").dim()) // ellipsis
        }
        "idle" => {
            format!("{}", style("\u{25CB}").dim()) // circle
        }
        "skipped" | "terminated" => {
            format!("{}", style("\u{2014}").dim()) // em dash
        }
        "create" | "add" => {
            format!("{}", style("+").green().bold())
        }
        "destroy" | "remove" | "delete" => {
            format!("{}", style("-").red().bold())
        }
        _ => {
            format!("{}", style("?").white())
        }
    }
}

/// Colorize a status string for terminal output.
pub fn colorize_status(status: &str) -> String {
    match status.to_lowercase().as_str() {
        "ok" | "completed" | "healthy" | "passed" | "idle" => {
            format!("{}", style(status).green())
        }
        "changed" | "updated" | "modified" | "waiting" | "spawning" => {
            format!("{}", style(status).yellow())
        }
        "failed" | "error" | "unhealthy" => {
            format!("{}", style(status).red().bold())
        }
        "running" | "executing" | "active" => {
            format!("{}", style(status).blue().bold())
        }
        "skipped" | "terminated" => {
            format!("{}", style(status).dim())
        }
        _ => status.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Misc
// ---------------------------------------------------------------------------

/// Print a section header line.
pub fn section(title: &str) {
    eprintln!("\n{}", style(format!("--- {title} ---")).bold());
}

/// Print a key-value pair with dimmed key.
pub fn kv(key: &str, value: &str) {
    eprintln!("  {}: {}", style(key).dim(), value);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_duration_millis() {
        assert_eq!(format_duration(Duration::from_millis(350)), "350ms");
    }

    #[test]
    fn test_format_duration_complex() {
        let d = Duration::from_secs(3 * 86400 + 2 * 3600 + 14 * 60 + 8);
        assert_eq!(format_duration(d), "3d 2h 14m 8s");
    }

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert_eq!(format_bytes(1024 * 1024 * 3), "3.0 MiB");
    }
}
