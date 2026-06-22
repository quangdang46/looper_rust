use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

// ---------------------------------------------------------------------------
// Helper: case-insensitive string enum with aliases
// ---------------------------------------------------------------------------
//
// Usage:
//   string_enum!(MyEnum [ "default" ] {
//       Foo => "foo",
//       Bar => "bar" | "bar-alt",
//   });
//
// The first pattern is the canonical (used by Display/as_str/serde-into).

macro_rules! string_enum {
    ($name:ident [ $def_variant:ident ] {
        $($variant:ident => $pat:literal $(| $($alias:literal)|+)? ),+ $(,)?
    }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
        #[serde(into = "&str")]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $pat),+
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = String;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                let lower = s.to_lowercase().replace('_', "-");
                $(
                    if lower == $pat $(|| $(lower == $alias)||+)? {
                        return Ok(Self::$variant);
                    }
                )+
                Err(format!("unknown {} variant: `{}`", stringify!($name), s))
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let s = String::deserialize(deserializer)?;
                Self::from_str(&s).map_err(serde::de::Error::custom)
            }
        }

        impl From<$name> for &str {
            fn from(v: $name) -> Self { v.as_str() }
        }

        impl Default for $name {
            fn default() -> Self { $name::$def_variant }
        }
    };
}

// ---------------------------------------------------------------------------
// Daemon / Runtime
// ---------------------------------------------------------------------------

string_enum!(DaemonRestartPolicy [ Always ] {
    Always => "always",
    OnFailure => "on-failure" | "onfailure",
    Never => "never",
});

string_enum!(ToolRuntime [ Host ] {
    Docker => "docker",
    Host => "host",
    Nix => "nix",
    Container => "container",
});

string_enum!(AgentMode [ Auto ] {
    Auto => "auto",
    Manual => "manual",
    Supervised => "supervised",
});

// ---------------------------------------------------------------------------
// GitHub / Git
// ---------------------------------------------------------------------------

string_enum!(OpenPRStrategy [ Create ] {
    Create => "create",
    Update => "update",
    Skip => "skip",
});

string_enum!(AddSnapshotMode [ None ] {
    None => "none",
    All => "all",
    Head => "head",
});

string_enum!(ReviewerScope [ Changed ] {
    Changed => "changed",
    Full => "full",
    Smart => "smart",
});

string_enum!(FixApplyMode [ Direct ] {
    Direct => "direct",
    Branch => "branch",
    Draft => "draft",
});

// ---------------------------------------------------------------------------
// Scheduler
// ---------------------------------------------------------------------------

string_enum!(SchedulerPolicy [ Fifo ] {
    Fifo => "fifo",
    Priority => "priority",
    RoundRobin => "round-robin" | "round_robin",
    Weighted => "weighted",
});

string_enum!(SchedulePeriod [ Hour ] {
    Minute => "minute" | "min",
    Hour => "hour" | "hr",
    Day => "day",
    Week => "week",
    Month => "month",
});

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

string_enum!(StorageBackend [ Sqlite ] {
    Sqlite => "sqlite",
    Postgres => "postgres" | "postgresql",
    Memory => "memory",
});

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

string_enum!(LogFormat [ Text ] {
    Text => "text",
    Json => "json",
    Compact => "compact",
});

string_enum!(LogOutput [ Stdout ] {
    Stdout => "stdout",
    Stderr => "stderr",
    File => "file",
    Syslog => "syslog",
});

string_enum!(LogRotation [ Daily ] {
    Daily => "daily",
    Hourly => "hourly",
    SizeBased => "size-based" | "size_based",
    Never => "never",
});

// ---------------------------------------------------------------------------
// Notifications
// ---------------------------------------------------------------------------

string_enum!(NotificationPriority [ Normal ] {
    Low => "low",
    Normal => "normal",
    High => "high",
    Urgent => "urgent",
});

string_enum!(NotificationChannel [ Log ] {
    Log => "log",
    Desktop => "desktop",
    Email => "email",
    Webhook => "webhook",
    Slack => "slack",
});

// ---------------------------------------------------------------------------
// Disclosure
// ---------------------------------------------------------------------------

string_enum!(DisclosureFormat [ Markdown ] {
    Markdown => "markdown" | "md",
    Html => "html",
    Text => "text",
});

// ---------------------------------------------------------------------------
// Diff / Merge
// ---------------------------------------------------------------------------

string_enum!(DiffAlgorithm [ Histogram ] {
    Patience => "patience",
    Histogram => "histogram",
    Myers => "myers",
});

// ---------------------------------------------------------------------------
// Misc
// ---------------------------------------------------------------------------

string_enum!(RetryBackoff [ Constant ] {
    Constant => "constant",
    Exponential => "exponential",
});

string_enum!(ServerProtocol [ Http ] {
    Http => "http",
    Unix => "unix",
});

string_enum!(AuthMode [ Token ] {
    Token => "token",
    Basic => "basic",
    OAuth => "oauth" | "oauth2",
    None => "none",
});

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! test_enum {
        ($ty:ident, $variant:ident, $str:literal) => {
            paste::paste! {
                #[test]
                fn [<enum_ $ty:snake _ $variant:snake>]() {
                    let v = $ty::$variant;
                    assert_eq!(v.as_str(), $str);
                    assert_eq!(format!("{}", v), $str);
                    let parsed = $ty::from_str($str).unwrap();
                    assert_eq!(parsed, v);
                    // case insensitive
                    let upper = $ty::from_str(&$str.to_uppercase()).unwrap();
                    assert_eq!(upper, v);
                }
            }
        };
    }

    test_enum!(DaemonRestartPolicy, Always, "always");
    test_enum!(DaemonRestartPolicy, OnFailure, "on-failure");
    test_enum!(DaemonRestartPolicy, Never, "never");

    test_enum!(ToolRuntime, Docker, "docker");
    test_enum!(ToolRuntime, Host, "host");

    test_enum!(SchedulerPolicy, Fifo, "fifo");
    test_enum!(SchedulerPolicy, RoundRobin, "round-robin");

    test_enum!(StorageBackend, Sqlite, "sqlite");
    test_enum!(StorageBackend, Postgres, "postgres");

    test_enum!(LogFormat, Text, "text");
    test_enum!(LogFormat, Json, "json");

    test_enum!(LogOutput, Stdout, "stdout");
    test_enum!(LogOutput, Stderr, "stderr");

    test_enum!(LogRotation, Daily, "daily");
    test_enum!(LogRotation, Hourly, "hourly");
    test_enum!(LogRotation, SizeBased, "size-based");
    test_enum!(LogRotation, Never, "never");

    test_enum!(NotificationPriority, Low, "low");
    test_enum!(NotificationPriority, Normal, "normal");
    test_enum!(NotificationPriority, Urgent, "urgent");

    test_enum!(NotificationChannel, Log, "log");
    test_enum!(NotificationChannel, Slack, "slack");

    test_enum!(DisclosureFormat, Markdown, "markdown");
    test_enum!(DisclosureFormat, Html, "html");
    test_enum!(DisclosureFormat, Text, "text");

    test_enum!(RetryBackoff, Constant, "constant");
    test_enum!(RetryBackoff, Exponential, "exponential");

    test_enum!(AgentMode, Auto, "auto");
    test_enum!(AgentMode, Supervised, "supervised");

    test_enum!(ServerProtocol, Http, "http");
    test_enum!(ServerProtocol, Unix, "unix");

    #[test]
    fn test_round_robin_from_alias() {
        let v = SchedulerPolicy::from_str("round_robin").unwrap();
        assert_eq!(v, SchedulerPolicy::RoundRobin);
    }

    #[test]
    fn test_on_failure_from_alias() {
        let v = DaemonRestartPolicy::from_str("onfailure").unwrap();
        assert_eq!(v, DaemonRestartPolicy::OnFailure);
    }

    #[test]
    fn test_serde_roundtrip_json() {
        let v = DisclosureFormat::Markdown;
        let json = serde_json::to_string(&v).unwrap();
        assert_eq!(json, "\"markdown\"");
        let back: DisclosureFormat = serde_json::from_str(&json).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn test_default_is_specified_default() {
        assert_eq!(DaemonRestartPolicy::default(), DaemonRestartPolicy::Always);
        assert_eq!(ToolRuntime::default(), ToolRuntime::Host);
        assert_eq!(StorageBackend::default(), StorageBackend::Sqlite);
    }

    #[test]
    fn test_invalid_variant_error() {
        let err = DaemonRestartPolicy::from_str("bogus").unwrap_err();
        assert!(err.contains("unknown"));
    }
}
