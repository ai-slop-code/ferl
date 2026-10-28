use crate::telemetry::metrics;

pub struct Classification {
    pub device: &'static str,
    pub browser: String,
    pub os: &'static str,
}

pub enum ParseOutcome {
    Parsed(Classification),
    Unrecognized,
}

/// Woothee 0.13.0 ships its maintained classification dataset in the crate;
/// parsing is local and never downloads rules or enriches requests externally.
pub fn classify(raw: Option<&str>) -> ParseOutcome {
    let Some(raw) = raw.filter(|value| !value.is_empty() && *value != "-") else {
        return ParseOutcome::Parsed(unknown());
    };
    let parser = woothee::parser::Parser::new();
    let Some(parsed) = parser.parse(raw) else {
        metrics::record_analytics(metrics::AnalyticsOutcome::ParserUnrecognized, None);
        return ParseOutcome::Unrecognized;
    };
    let device = match parsed.category {
        "pc" => "desktop",
        "smartphone" | "mobilephone" => "mobile",
        "tablet" => "tablet",
        "UNKNOWN" => "unknown",
        _ => "other",
    };
    let browser = if parsed.browser_type == "browser" && parsed.name != "UNKNOWN" {
        parsed.name.to_owned()
    } else {
        "unknown".to_owned()
    };
    let os = os_family(parsed.os);
    ParseOutcome::Parsed(Classification {
        device,
        browser,
        os,
    })
}

pub fn unknown() -> Classification {
    Classification {
        device: "unknown",
        browser: "unknown".to_owned(),
        os: "unknown",
    }
}

fn os_family(value: &str) -> &'static str {
    if value.starts_with("Windows") {
        return "Windows";
    }
    match value {
        "UNKNOWN" | "" => "unknown",
        "Android" => "Android",
        "Chrome OS" | "ChromeOS" => "Chrome OS",
        "BSD" => "BSD",
        "Debian" => "Debian",
        "Fedora" => "Fedora",
        "FreeBSD" => "FreeBSD",
        "GNU/Linux" | "Linux" => "Linux",
        "iOS" | "iPhone" | "iPad" | "iPod" => "iOS",
        "Mac OS X" | "Mac OSX" => "Mac OS X",
        "Mac OS Classic" => "Mac OS",
        "Ubuntu" => "Ubuntu",
        "BlackBerry OS" | "BlackBerry 10" => "BlackBerry",
        "Firefox OS" => "Firefox OS",
        "SymbianOS" => "Symbian",
        "PlayStation 4" | "PlayStation Vita" | "PlayStation Portable" => "PlayStation",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::{ParseOutcome, classify};

    #[test]
    fn maps_local_user_agent_rules_to_bounded_families() {
        let ParseOutcome::Parsed(desktop) = classify(Some(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/120.0.0.0 Safari/537.36",
        )) else {
            panic!("desktop browser parses locally");
        };
        assert_eq!(desktop.device, "desktop");
        assert_eq!(desktop.browser, "Chrome");
        assert_eq!(desktop.os, "Windows");

        let ParseOutcome::Parsed(mobile) = classify(Some(
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1",
        )) else {
            panic!("mobile browser parses locally");
        };
        assert_eq!(mobile.device, "mobile");
        assert_eq!(mobile.browser, "Safari");
        assert_eq!(mobile.os, "iOS");

        let ParseOutcome::Parsed(missing) = classify(None) else {
            panic!("missing UA is a valid unknown classification");
        };
        assert_eq!(
            (missing.device, missing.browser, missing.os),
            ("unknown", "unknown".to_owned(), "unknown")
        );

        assert!(matches!(
            classify(Some("totally unrecognized client")),
            ParseOutcome::Unrecognized
        ));
    }
}
