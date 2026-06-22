//! Diff parsing and anchor validation for review comments.
//!
//! Parses unified diffs into a structured [`Index`] and validates that review
//! anchors (line/path/side) point to real locations in the diff.  Used by the
//! GitHub gateway to normalize inline review comments.

use std::fmt;

/// Side of the diff: "LEFT" (old) or "RIGHT" (new).
pub const SIDE_LEFT: &str = "LEFT";
/// Side of the diff: "RIGHT" (new).
pub const SIDE_RIGHT: &str = "RIGHT";

/// A specific location in a diff, used for inline review comments.
#[derive(Debug, Clone, PartialEq)]
pub struct Anchor {
    pub path: String,
    pub line: i64,
    pub side: String,
    pub start_line: Option<i64>,
    pub start_side: Option<String>,
}

/// A contiguous range of lines in one file of a diff.
#[derive(Debug, Clone, PartialEq)]
pub struct Range {
    pub path: String,
    pub side: String,
    pub start: i64,
    pub end: i64,
    pub excerpt: String,
    pub heading: String,
}

/// A parsed diff index — the set of all ranges across all files.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Index {
    pub ranges: Vec<Range>,
}

/// Outcome of validating an anchor against the diff index.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidationResult {
    pub valid: bool,
    pub reason: String,
    pub location_text: String,
    pub quality_flagged: bool,
}

// ── Parsing ───────────────────────────────────────────────────────────────

/// Parse a unified diff string into an [`Index`].
///
/// Handles the standard `@@ -start,count +start,count @@ heading` format.
/// Tracks the file path and side for each hunk.
pub fn parse(diff: &str) -> Index {
    let mut ranges = Vec::new();
    let mut current_path = String::new();
    let mut current_hunks: Vec<Range> = Vec::new();

    for line in diff.lines() {
        if let Some(_old_path) = path_from_header(line, "--- ") {
            // Track old-path for side-LEFT ranges (currently only used for tracking)
            continue;
        }
        if let Some(path) = path_from_header(line, "+++ ") {
            current_path = path;
            // Flush any pending hunks from the previous file
            if !current_hunks.is_empty() {
                ranges.append(&mut current_hunks);
            }
            continue;
        }
        if let Some((old_start, new_start, heading)) = parse_hunk_header(line) {
            // Close any open range for the new side
            if let Some(last) = current_hunks.last_mut() {
                if last.end == 0 {
                    last.end = new_start;
                }
            }

            let path = current_path.clone();
            let heading = heading.unwrap_or_default();

            // RIGHT side range
            current_hunks.push(Range {
                path: path.clone(),
                side: SIDE_RIGHT.to_string(),
                start: new_start,
                end: new_start,
                excerpt: String::new(),
                heading: heading.clone(),
            });

            // LEFT side range
            current_hunks.push(Range {
                path,
                side: SIDE_LEFT.to_string(),
                start: old_start,
                end: old_start,
                excerpt: String::new(),
                heading,
            });
            continue;
        }

        // Update end for both LEFT and RIGHT ranges
        if current_hunks.len() >= 2 {
            let mut ranges = current_hunks.iter_mut();
            let right = ranges.next().expect("RIGHT range exists");
            let left = ranges.next().expect("LEFT range exists");

            // LEFT side: context lines ( ) and deletions (-)
            if line.starts_with(' ') || line.starts_with('-') {
                left.end += 1;
            }
            // RIGHT side: context lines ( ) and additions (+)
            if line.starts_with(' ') || line.starts_with('+') {
                right.end += 1;
            }

            // Accumulate excerpt on the RIGHT range (bounded)
            if !line.starts_with("--- ") && !line.starts_with("+++ ")
                && !line.starts_with("diff ") && !line.starts_with("index ")
                && right.excerpt.len() < 1024
            {
                if !right.excerpt.is_empty() {
                    right.excerpt.push('\n');
                }
                right.excerpt.push_str(line);
            }
        }
    }

    // Flush remaining hunks
    ranges.append(&mut current_hunks);

    // Deduplicate — keep only RIGHT-side ranges (simpler validation model)
    let right_ranges: Vec<Range> = ranges
        .into_iter()
        .filter(|r| r.side == SIDE_RIGHT)
        .collect();

    Index { ranges: right_ranges }
}

// ── Index methods ─────────────────────────────────────────────────────────

impl Index {
    /// Format the index as a compact prompt section for LLM context, limited to
    /// `limit` characters.
    pub fn format_prompt_section(&self, limit: usize) -> String {
        let mut out = String::from("Diff index:\n");
        for range in &self.ranges {
            let line = format!(
                "  {}:{}-{} {}",
                range.path, range.start, range.end, range.heading
            );
            if out.len() + line.len() + 1 > limit {
                out.push_str("  ... (truncated)\n");
                break;
            }
            out.push_str(&line);
            out.push('\n');
        }
        out
    }

    /// Validate that `anchor` points to a real location in this diff index.
    pub fn validate(&self, anchor: &Anchor) -> ValidationResult {
        let normalized_side = normalize_side(&anchor.side);

        // Find matching range(s) by path
        let matching: Vec<&Range> = self
            .ranges
            .iter()
            .filter(|r| {
                r.path == anchor.path || r.path.ends_with(&anchor.path)
                // ^ Allow suffix match for a/b prefixes
            })
            .collect();

        if matching.is_empty() {
            return ValidationResult {
                valid: false,
                reason: format!(
                    "path '{}' not found in diff (available: {})",
                    anchor.path,
                    self.paths_summary()
                ),
                location_text: fallback_location(anchor),
                quality_flagged: false,
            };
        }

        // Check if the anchor line falls within any matching range
        let mut best_range: Option<&&Range> = None;
        for r in &matching {
            if normalized_side == SIDE_RIGHT {
                // For RIGHT side, line must be within range
                if anchor.line >= r.start && anchor.line <= r.end {
                    best_range = Some(r);
                    break;
                }
            } else {
                // LEFT side — range defines deleted lines
                if anchor.line >= r.start && anchor.line <= r.end {
                    best_range = Some(r);
                    break;
                }
            }
        }

        if let Some(range) = best_range {
            let quality_flagged = anchor.start_line.is_some() && anchor.start_line != Some(range.start);
            ValidationResult {
                valid: true,
                reason: "anchor matches diff location".to_string(),
                location_text: format!(
                    "{}:{}-{} [{}]",
                    anchor.path, range.start, range.end, normalized_side
                ),
                quality_flagged,
            }
        } else {
            // Line not in range — suggest the closest range
            let suggestion = matching
                .iter()
                .min_by_key(|r| (anchor.line - r.start).abs())
                .map(|r| format!("closest range: {}:{}-{}", r.path, r.start, r.end))
                .unwrap_or_default();

            ValidationResult {
                valid: false,
                reason: format!(
                    "line {} not in any range for '{}'. {}",
                    anchor.line, anchor.path, suggestion
                ),
                location_text: fallback_location(anchor),
                quality_flagged: false,
            }
        }
    }

    fn paths_summary(&self) -> String {
        let mut paths: Vec<&str> = self.ranges.iter().map(|r| r.path.as_str()).collect();
        paths.sort();
        paths.dedup();
        paths.join(", ")
    }
}

// ── Top-level validation helpers ──────────────────────────────────────────

/// Validate a location given in free-text form (e.g. from a PR comment body).
pub fn validate_top_level_location(body: &str) -> ValidationResult {
    let body_trimmed = body.trim();
    if body_trimmed.is_empty() {
        return ValidationResult {
            valid: false,
            reason: "empty location".to_string(),
            location_text: String::new(),
            quality_flagged: false,
        };
    }
    ValidationResult {
        valid: true,
        reason: "top-level location (not validated against diff)".to_string(),
        location_text: body_trimmed.to_string(),
        quality_flagged: true,
    }
}

/// Generate a fallback comment body that includes the original anchor context.
pub fn fallback_body(body: &str, anchor: &Anchor, reason: &str) -> String {
    format!(
        "[anchor invalid — {reason}]\n\nBody:\n{body}\n\n---\nOriginal anchor: {}:{} ({})",
        anchor.path, anchor.line, anchor.side
    )
}

/// Generate a fallback location string from an anchor.
pub fn fallback_location(anchor: &Anchor) -> String {
    format!("{}:{} [{}]", anchor.path, anchor.line, anchor.side)
}

/// Normalize a side string to canonical "LEFT" or "RIGHT".
pub fn normalize_side(side: &str) -> String {
    match side.to_uppercase().as_str() {
        "LEFT" | "OLD" | "REMOVED" | "DELETION" => SIDE_LEFT.to_string(),
        _ => SIDE_RIGHT.to_string(),
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────

fn path_from_header(line: &str, prefix: &str) -> Option<String> {
    if let Some(rest) = line.strip_prefix(prefix) {
        let path = rest.trim();
        if !path.is_empty() && path != "/dev/null" {
            // Strip common prefixes: a/, b/, i/, w/
            let cleaned = path
                .strip_prefix("a/")
                .or_else(|| path.strip_prefix("b/"))
                .or_else(|| path.strip_prefix("i/"))
                .or_else(|| path.strip_prefix("w/"))
                .unwrap_or(path);
            return Some(cleaned.to_string());
        }
    }
    None
}

fn parse_hunk_header(line: &str) -> Option<(i64, i64, Option<String>)> {
    let line = line.trim();
    if !line.starts_with("@@") {
        return None;
    }
    let rest = line.strip_prefix("@@")?.trim_start();
    // Split at the second @@
    let (coord_str, heading) = if let Some(idx) = rest.find("@@") {
        let (coords, head) = rest.split_at(idx);
        let head = head.strip_prefix("@@").map(|s| s.trim().to_string());
        (coords.trim(), head.filter(|s| !s.is_empty()))
    } else {
        return None;
    };

    // Parse coordinates: -old_start[,count] +new_start[,count]
    let parts: Vec<&str> = coord_str.split_whitespace().collect();
    if parts.len() < 2 {
        return None;
    }

    let old_part = parts[0].strip_prefix('-')?;
    let new_part = parts[1].strip_prefix('+')?;

    let old_start = old_part.split(',').next()?.parse::<i64>().ok()?;
    let new_start = new_part.split(',').next()?.parse::<i64>().ok()?;

    Some((old_start, new_start, heading))
}

// ── Display ───────────────────────────────────────────────────────────────

impl fmt::Display for Anchor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}[{}]", self.path, self.line, self.side)
    }
}

impl fmt::Display for Range {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}-{} [{}] {}",
            self.path, self.start, self.end, self.side, self.heading
        )
    }
}

impl fmt::Display for ValidationResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}] {} — {}",
            if self.valid { "OK" } else { "INVALID" },
            self.location_text,
            self.reason
        )
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_diff() -> &'static str {
        r#"diff --git a/src/main.rs b/src/main.rs
index abc..def 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -10,7 +10,7 @@ impl App {
         // old line
-        let x = 1;
+        let x = 2;
         // unchanged
     }
@@ -30,5 +30,9 @@ impl App {
     fn new() -> Self {
         Self { field: 42 }
     }
+
+    fn added_method() -> bool {
+        true
+    }
 }
"#
    }

    fn minimal_diff() -> &'static str {
        r#"--- a/README.md
+++ b/README.md
@@ -1,3 +1,4 @@
 # Title
-old line
+new line
+another line
"#
    }

    #[test]
    fn test_parse_basic() {
        let idx = parse(sample_diff());
        // Should have ranges for both hunks
        assert!(!idx.ranges.is_empty(), "expected at least one range");
        // All ranges should be RIGHT side
        assert!(
            idx.ranges.iter().all(|r| r.side == SIDE_RIGHT),
            "all ranges should be RIGHT side"
        );
        // Path should be src/main.rs
        assert_eq!(idx.ranges[0].path, "src/main.rs");
    }

    #[test]
    fn test_parse_minimal() {
        let idx = parse(minimal_diff());
        assert_eq!(idx.ranges.len(), 1);
        assert_eq!(idx.ranges[0].path, "README.md");
        assert_eq!(idx.ranges[0].start, 1);
        assert!(idx.ranges[0].end >= 4);
    }

    #[test]
    fn test_parse_empty_diff() {
        let idx = parse("");
        assert!(idx.ranges.is_empty());
    }

    #[test]
    fn test_validate_valid_anchor() {
        let idx = parse(sample_diff());
        let anchor = Anchor {
            path: "src/main.rs".to_string(),
            line: 12,
            side: SIDE_RIGHT.to_string(),
            start_line: None,
            start_side: None,
        };
        let result = idx.validate(&anchor);
        assert!(
            result.valid,
            "expected valid, got: {}",
            result.reason
        );
    }

    #[test]
    fn test_validate_invalid_path() {
        let idx = parse(sample_diff());
        let anchor = Anchor {
            path: "nonexistent.rs".to_string(),
            line: 1,
            side: SIDE_RIGHT.to_string(),
            start_line: None,
            start_side: None,
        };
        let result = idx.validate(&anchor);
        assert!(!result.valid);
        assert!(result.reason.contains("not found in diff"));
    }

    #[test]
    fn test_validate_invalid_line() {
        let idx = parse(sample_diff());
        let anchor = Anchor {
            path: "src/main.rs".to_string(),
            line: 999,
            side: SIDE_RIGHT.to_string(),
            start_line: None,
            start_side: None,
        };
        let result = idx.validate(&anchor);
        assert!(!result.valid);
        assert!(result.reason.contains("not in any range"));
    }

    #[test]
    fn test_validate_suffix_path_match() {
        let idx = parse(sample_diff());
        let anchor = Anchor {
            path: "main.rs".to_string(),
            line: 12,
            side: SIDE_RIGHT.to_string(),
            start_line: None,
            start_side: None,
        };
        let result = idx.validate(&anchor);
        assert!(result.valid, "suffix match should work: {}", result.reason);
    }

    #[test]
    fn test_format_prompt_section() {
        let idx = parse(sample_diff());
        let section = idx.format_prompt_section(500);
        assert!(section.starts_with("Diff index:"));
        assert!(section.contains("src/main.rs:"));
        assert!(section.len() <= 500);
    }

    #[test]
    fn test_format_prompt_section_truncated() {
        let idx = parse(sample_diff());
        let section = idx.format_prompt_section(10);
        assert!(section.contains("truncated"));
    }

    #[test]
    fn test_validate_top_level_location() {
        let result = validate_top_level_location("src/main.rs:42");
        assert!(result.valid);
        assert!(result.quality_flagged);

        let empty = validate_top_level_location("");
        assert!(!empty.valid);
    }

    #[test]
    fn test_fallback_body_and_location() {
        let anchor = Anchor {
            path: "foo.rs".to_string(),
            line: 42,
            side: SIDE_RIGHT.to_string(),
            start_line: None,
            start_side: None,
        };
        let body = fallback_body("some comment", &anchor, "test");
        assert!(body.contains("some comment"));
        assert!(body.contains("foo.rs:42"));

        let loc = fallback_location(&anchor);
        assert_eq!(loc, "foo.rs:42 [RIGHT]");
    }

    #[test]
    fn test_normalize_side() {
        assert_eq!(normalize_side("LEFT"), "LEFT");
        assert_eq!(normalize_side("RIGHT"), "RIGHT");
        assert_eq!(normalize_side("right"), "RIGHT");
        assert_eq!(normalize_side("old"), "LEFT");
        assert_eq!(normalize_side("removed"), "LEFT");
        assert_eq!(normalize_side("anything else"), "RIGHT");
    }

    #[test]
    fn test_display_traits() {
        let anchor = Anchor {
            path: "f.rs".to_string(),
            line: 1,
            side: SIDE_LEFT.to_string(),
            start_line: None,
            start_side: None,
        };
        assert_eq!(anchor.to_string(), "f.rs:1[LEFT]");

        let range = Range {
            path: "f.rs".to_string(),
            side: SIDE_RIGHT.to_string(),
            start: 10,
            end: 20,
            excerpt: String::new(),
            heading: "func".to_string(),
        };
        let disp = range.to_string();
        assert!(disp.contains("10-20"));
        assert!(disp.contains("func"));

        let vr = ValidationResult {
            valid: true,
            reason: "ok".to_string(),
            location_text: "f.rs:10".to_string(),
            quality_flagged: false,
        };
        assert!(vr.to_string().contains("[OK]"));
    }

    #[test]
    fn test_hunk_header_parsing() {
        let (old, new, heading) = parse_hunk_header("@@ -10,7 +12,9 @@ impl App")
            .expect("should parse hunk header");
        assert_eq!(old, 10);
        assert_eq!(new, 12);
        assert_eq!(heading, Some("impl App".to_string()));

        assert!(parse_hunk_header("normal line").is_none());
        assert!(parse_hunk_header("@@ -1 +1 @@").is_some());
    }

    #[test]
    fn test_path_from_header() {
        assert_eq!(
            path_from_header("--- a/src/main.rs", "--- "),
            Some("src/main.rs".to_string())
        );
        assert_eq!(
            path_from_header("+++ b/src/main.rs", "+++ "),
            Some("src/main.rs".to_string())
        );
        assert_eq!(path_from_header("--- /dev/null", "--- "), None);
        assert_eq!(path_from_header("--- ", "--- "), None);
    }
}
