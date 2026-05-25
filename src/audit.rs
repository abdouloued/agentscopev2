use anyhow::Result;
use chrono::{DateTime, Duration, Utc};

use crate::config::ACTIVITY_LOG;
use crate::output::Printer;

pub async fn run(range: String, session_id: Option<String>) -> Result<()> {
    let p = Printer::new();

    let entries = load_activity_log()?;

    if entries.is_empty() {
        p.hint("No activity log found. Run some sessions first.");
        return Ok(());
    }

    let filtered = filter_entries(&entries, &range, session_id.as_deref());

    if filtered.is_empty() {
        p.hint(&format!("No sessions found for range: {}", range));
        return Ok(());
    }

    println!();
    println!(
        "  {} {}  {}",
        console::style("agentscope").color256(135),
        console::style("audit").bold(),
        console::style(&range).dim(),
    );
    println!();

    // Header row
    println!(
        "  {:<12}  {:<12}  {:<8}  {:<6}  {}",
        console::style("session").dim(),
        console::style("agent").dim(),
        console::style("event").dim(),
        console::style("time").dim(),
        console::style("mission").dim(),
    );
    println!("  {}", console::style("─".repeat(72)).dim());

    for entry in &filtered {
        let ts = DateTime::parse_from_rfc3339(&entry.timestamp)
            .map(|dt| format_relative(dt.with_timezone(&Utc)))
            .unwrap_or_else(|_| "?".into());

        let mission_preview = if entry.session.mission.len() > 40 {
            format!("{}…", &entry.session.mission[..40])
        } else {
            entry.session.mission.clone()
        };

        let event_styled = match entry.event.as_str() {
            "session_start" => console::style("start").green(),
            "session_check" => console::style("check").blue(),
            _ => console::style(entry.event.as_str()).dim(),
        };

        println!(
            "  {:<12}  {:<12}  {:<8}  {:<6}  {}",
            console::style(&entry.session.id[..12]).cyan(),
            console::style(&entry.session.agent).color256(245),
            event_styled,
            console::style(&ts).dim(),
            console::style(&mission_preview).white(),
        );
    }

    println!();
    println!(
        "  {}  {} sessions shown",
        console::style("→").dim(),
        filtered.len(),
    );
    println!();

    Ok(())
}

// ── Activity log parsing ──────────────────────────────────────────────────────

#[derive(Debug, serde::Deserialize)]
struct ActivityEntry {
    event: String,
    timestamp: String,
    session: crate::session::Session,
}

fn load_activity_log() -> Result<Vec<ActivityEntry>> {
    let path = std::path::Path::new(ACTIVITY_LOG);
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = std::fs::read_to_string(path)?;
    let entries: Vec<ActivityEntry> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();

    Ok(entries)
}

fn filter_entries<'a>(
    entries: &'a [ActivityEntry],
    range: &str,
    session_id: Option<&str>,
) -> Vec<&'a ActivityEntry> {
    let now = Utc::now();

    let mut filtered: Vec<&ActivityEntry> = entries
        .iter()
        .filter(|e| {
            if let Some(id) = session_id {
                return e.session.id.starts_with(id);
            }

            match range {
                "last-5" => true, // handled by take() below
                "last-10" => true,
                "today" => DateTime::parse_from_rfc3339(&e.timestamp)
                    .map(|dt| {
                        let age = now - dt.with_timezone(&Utc);
                        age < Duration::hours(24)
                    })
                    .unwrap_or(false),
                "this-week" => DateTime::parse_from_rfc3339(&e.timestamp)
                    .map(|dt| {
                        let age = now - dt.with_timezone(&Utc);
                        age < Duration::days(7)
                    })
                    .unwrap_or(false),
                _ => true,
            }
        })
        .collect();

    // Sort newest first
    filtered.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

    // Apply take limits
    let limit = match range {
        "last-5" => 5,
        "last-10" => 10,
        _ => usize::MAX,
    };

    filtered.into_iter().take(limit).collect()
}

fn format_relative(dt: DateTime<Utc>) -> String {
    let age = Utc::now() - dt;
    if age.num_minutes() < 1 {
        "now".into()
    } else if age.num_minutes() < 60 {
        format!("{}m", age.num_minutes())
    } else if age.num_hours() < 24 {
        format!("{}h", age.num_hours())
    } else {
        format!("{}d", age.num_days())
    }
}
