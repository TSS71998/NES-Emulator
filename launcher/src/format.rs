//! Turning sizes and times into short text for the game list.

pub fn size(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;

    if bytes >= MIB {
        let mib = bytes as f64 / MIB as f64;
        if bytes % MIB == 0 {
            format!("{mib:.0} MiB")
        } else {
            format!("{mib:.1} MiB")
        }
    } else {
        format!("{} KiB", bytes.div_ceil(1024))
    }
}

pub fn play_time(secs: u64) -> String {
    match secs {
        0 => "—".to_string(),
        1..60 => "< 1 min".to_string(),
        60..3600 => format!("{} min", secs / 60),
        _ => format!("{} h {:02} min", secs / 3600, secs / 60 % 60)
    }
}

pub fn ago(now: u64, then: Option<u64>) -> String {
    let Some(then) = then else {
        return "Never".to_string();
    };
    let secs = now.saturating_sub(then);
    match secs {
        0..60 => "Just now".to_string(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86400 => format!("{} h ago", secs / 3600),
        86400..172800 => "Yesterday".to_string(),
        _ if secs < 60 * 86400 => format!("{} days ago", secs / 86400),
        _ if secs < 730 * 86400 => format!("{} months ago", secs / (30 * 86400)),
        _ => format!("{} years ago", secs / (365 * 86400))
    }
}

pub fn clock(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_size() {
        assert_eq!(size(40976), "41 KiB");
        assert_eq!(size(262160), "257 KiB");
        assert_eq!(size(1024 * 1024), "1 MiB");
        assert_eq!(size(1536 * 1024), "1.5 MiB");
    }

    #[test]
    fn test_times() {
        assert_eq!(play_time(0), "—");
        assert_eq!(play_time(59), "< 1 min");
        assert_eq!(play_time(3 * 3600 + 5 * 60), "3 h 05 min");
        assert_eq!(ago(1000, None), "Never");
        assert_eq!(ago(1000, Some(990)), "Just now");
        assert_eq!(ago(100_000, Some(100_000 - 90_000)), "Yesterday");
        assert_eq!(ago(10_000_000, Some(10_000_000 - 5 * 86400)), "5 days ago");
        assert_eq!(clock(247), "4:07");
        assert_eq!(clock(3753), "1:02:33");
    }
}