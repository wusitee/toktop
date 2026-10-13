//! Compact number formatting for display.

/// `1234` -> `1.2K`, `5_600_000` -> `5.6M`.
pub fn tokens(value: u64) -> String {
    const UNITS: [(u64, &str); 4] = [
        (1_000_000_000_000, "T"),
        (1_000_000_000, "B"),
        (1_000_000, "M"),
        (1_000, "K"),
    ];
    for (scale, unit) in UNITS {
        if value >= scale {
            let scaled = value as f64 / scale as f64;
            return if scaled >= 100.0 {
                format!("{scaled:.0}{unit}")
            } else {
                format!("{scaled:.1}{unit}")
            };
        }
    }
    value.to_string()
}

/// `1435810966` -> `1,435,810,966`.
pub fn grouped(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// Output speed, as `84 tok/s` or `8.2 tok/s`.
pub fn speed(tokens_per_second: f64) -> String {
    if tokens_per_second >= 10.0 {
        format!("{tokens_per_second:.0} tok/s")
    } else {
        format!("{tokens_per_second:.1} tok/s")
    }
}

pub fn cost(usd: f64) -> String {
    if usd >= 1000.0 {
        format!("${usd:.0}")
    } else if usd >= 0.01 || usd == 0.0 {
        format!("${usd:.2}")
    } else {
        "<$0.01".to_owned()
    }
}

pub fn percent(fraction: f64) -> String {
    format!("{:.0}%", fraction * 100.0)
}

/// Seconds until a time, as `3d 4h`, `2h 5m` or `12m`.
pub fn duration_until(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let (days, hours, minutes) = (
        seconds / 86_400,
        seconds % 86_400 / 3600,
        seconds % 3600 / 60,
    );
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

/// Elapsed time since `ts_ms`, as `now`, `5m ago`, `3h ago` or `2d ago`.
pub fn ago(ts_ms: i64, now_ms: i64) -> String {
    let seconds = (now_ms - ts_ms).max(0) / 1000;
    match seconds {
        0..60 => "now".to_owned(),
        60..3600 => format!("{}m ago", seconds / 60),
        3600..86_400 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}
