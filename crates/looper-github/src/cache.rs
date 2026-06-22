//! Discovery cache with TTL expiry.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use crate::types::{IssueSummary, PullRequestSummary};

/// Cache entry for pull request discovery lists.
#[derive(Clone, Debug)]
pub struct DiscoveryPullRequestListCacheEntry {
    pub expires_at: Instant,
    pub items: Vec<PullRequestSummary>,
}

/// Cache entry for issue discovery lists.
#[derive(Clone, Debug)]
pub struct DiscoveryIssueListCacheEntry {
    pub expires_at: Instant,
    pub items: Vec<IssueSummary>,
}

/// Read-through cache for discovery operations.
///
/// Cache key format: `"<repo>|<filters>"` (e.g. `"owner/repo||"` or with label/author).
/// TTL is configurable via the gateway's `discovery_cache_ttl` (default 30s).
pub struct DiscoveryCache {
    inner: Mutex<DiscoveryCacheInner>,
}

struct DiscoveryCacheInner {
    pr_cache: HashMap<String, DiscoveryPullRequestListCacheEntry>,
    review_pr_cache: HashMap<String, DiscoveryPullRequestListCacheEntry>,
    issue_cache: HashMap<String, DiscoveryIssueListCacheEntry>,
}

impl DiscoveryCache {
    /// Creates an empty discovery cache.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(DiscoveryCacheInner {
                pr_cache: HashMap::new(),
                review_pr_cache: HashMap::new(),
                issue_cache: HashMap::new(),
            }),
        }
    }

    /// Returns cached PR list items if a valid (non-expired) entry exists.
    pub fn get_prs(&self, key: &str, _ttl: Duration) -> Option<Vec<PullRequestSummary>> {
        let inner = self.inner.lock().ok()?;
        let entry = inner.pr_cache.get(key)?;
        if entry.expires_at > Instant::now() {
            Some(entry.items.clone())
        } else {
            None
        }
    }

    /// Stores a PR list in the cache with the given TTL.
    pub fn set_prs(&self, key: String, items: Vec<PullRequestSummary>, ttl: Duration) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.pr_cache.insert(
                key,
                DiscoveryPullRequestListCacheEntry {
                    expires_at: Instant::now() + ttl,
                    items,
                },
            );
        }
    }

    /// Returns cached review-requested PR list items if a valid entry exists.
    pub fn get_review_prs(&self, key: &str, _ttl: Duration) -> Option<Vec<PullRequestSummary>> {
        let inner = self.inner.lock().ok()?;
        let entry = inner.review_pr_cache.get(key)?;
        if entry.expires_at > Instant::now() {
            Some(entry.items.clone())
        } else {
            None
        }
    }

    /// Stores a review-requested PR list in the cache.
    pub fn set_review_prs(
        &self,
        key: String,
        items: Vec<PullRequestSummary>,
        ttl: Duration,
    ) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.review_pr_cache.insert(
                key,
                DiscoveryPullRequestListCacheEntry {
                    expires_at: Instant::now() + ttl,
                    items,
                },
            );
        }
    }

    /// Returns cached issue list items if a valid entry exists.
    pub fn get_issues(&self, key: &str, _ttl: Duration) -> Option<Vec<IssueSummary>> {
        let inner = self.inner.lock().ok()?;
        let entry = inner.issue_cache.get(key)?;
        if entry.expires_at > Instant::now() {
            Some(entry.items.clone())
        } else {
            None
        }
    }

    /// Stores an issue list in the cache.
    pub fn set_issues(&self, key: String, items: Vec<IssueSummary>, ttl: Duration) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.issue_cache.insert(
                key,
                DiscoveryIssueListCacheEntry {
                    expires_at: Instant::now() + ttl,
                    items,
                },
            );
        }
    }
}

impl Default for DiscoveryCache {
    fn default() -> Self {
        Self::new()
    }
}
