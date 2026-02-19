use chrono::{DateTime, Datelike, Local, Timelike, Utc};
use std::{process::Command, time::Duration};

use crate::types::{KeychainCreds, UsageLimits, UsageResponse};

fn get_keychain_credentials() -> Option<String> {
    // Try keyring crate first
    if let Ok(entry) = keyring::Entry::new("Claude Code-credentials", "credentials") {
        if let Ok(password) = entry.get_password() {
            return Some(password);
        }
    }

    // Fallback to security command on macOS
    if cfg!(target_os = "macos") {
        let output = Command::new("security")
            .args([
                "find-generic-password",
                "-s",
                "Claude Code-credentials",
                "-w",
            ])
            .output()
            .ok()?;

        if output.status.success() {
            return Some(String::from_utf8_lossy(&output.stdout).trim().to_string());
        }
    }

    None
}

pub fn load_usage_data() -> UsageLimits {
    let mut result = UsageLimits {
        five_hour: -1,
        weekly: -1,
        sonnet: -1,
        ..Default::default()
    };

    let creds = match get_keychain_credentials() {
        Some(c) => c,
        None => return result,
    };

    let keychain_data: KeychainCreds = match serde_json::from_str(&creds) {
        Ok(d) => d,
        Err(_) => return result,
    };

    let token = &keychain_data.claude_ai_oauth.access_token;
    if token.is_empty() {
        return result;
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap();

    let response = client
        .get("https://api.anthropic.com/api/oauth/usage")
        .header("Authorization", format!("Bearer {}", token))
        .header("anthropic-beta", "oauth-2025-04-20")
        .send();

    if let Ok(resp) = response {
        if let Ok(usage) = resp.json::<UsageResponse>() {
            result.five_hour = usage.five_hour.utilization as i32;
            if let Some(ref r) = usage.five_hour.resets_at {
                result.five_hour_reset = calculate_reset_time(r);
            }
            result.weekly = usage.seven_day.utilization as i32;
            if let Some(ref r) = usage.seven_day.resets_at {
                result.weekly_reset = format_reset_day(r);
            }
            if let Some(sonnet) = usage.seven_day_sonnet {
                result.sonnet = sonnet.utilization as i32;
            }
        }
    }

    result
}

fn calculate_reset_time(resets_at: &str) -> String {
    let reset_time = match DateTime::parse_from_rfc3339(resets_at) {
        Ok(dt) => dt.with_timezone(&Utc),
        Err(_) => return String::new(),
    };

    let now = Utc::now();
    let diff = reset_time.signed_duration_since(now);

    if diff <= chrono::Duration::zero() {
        return "soon".to_string();
    }

    let hours = diff.num_hours();
    let mins = diff.num_minutes() % 60;

    if hours > 0 {
        format!("{}h{}m", hours, mins)
    } else {
        format!("{}m", mins)
    }
}

fn format_reset_day(resets_at: &str) -> String {
    let reset_time = match DateTime::parse_from_rfc3339(resets_at) {
        Ok(dt) => dt.with_timezone(&Local),
        Err(_) => return String::new(),
    };

    let weekdays = ["日", "月", "火", "水", "木", "金", "土"];
    let weekday_num = reset_time.weekday().num_days_from_sunday() as usize;
    let weekday = weekdays[weekday_num];

    format!("{}{}:{:02}", weekday, reset_time.hour(), reset_time.minute())
}
