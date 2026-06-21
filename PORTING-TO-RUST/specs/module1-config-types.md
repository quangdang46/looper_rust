# Module 1: looper-core (Config + Types) — Rust Port Spec

## Source Files
- `internal/config/types.go` — 933 lines
- `internal/config/load.go` — 1655 lines
- `internal/config/validate.go` — 1046 lines
- `internal/config/normalize.go` — 1497 lines
- `internal/config/defaults.go` — 312 lines
- `internal/disclosure/disclosure.go` — 178 lines
- `internal/diffanchor/diffanchor.go` — 435 lines

---

## 1. STRING ENUMS (Go type alias for string → Rust enum with Display/FromStr)

### AgentVendor
```go
type AgentVendor string
const (
    AgentVendorClaudeCode AgentVendor = "claude-code"
    AgentVendorCodex      AgentVendor = "codex"
    AgentVendorOpenCode   AgentVendor = "opencode"
    AgentVendorCursorCLI  AgentVendor = "cursor-cli"
    AgentVendorHermes     AgentVendor = "hermes"
)
```

### LogLevel
```go
type LogLevel string
const (
    LogLevelDebug LogLevel = "debug"
    LogLevelInfo  LogLevel = "info"
    LogLevelWarn  LogLevel = "warn"
    LogLevelError LogLevel = "error"
)
```

### AuthMode
```go
type AuthMode string
const (
    AuthModeNone       AuthMode = "none"
    AuthModeLocalToken AuthMode = "local-token"
)
```

### DaemonMode
```go
type DaemonMode string
const (
    DaemonModeForeground DaemonMode = "foreground"
    DaemonModeLaunchd    DaemonMode = "launchd"
)
```

### DaemonRestartPolicy
```go
type DaemonRestartPolicy string
const (
    DaemonRestartNever     DaemonRestartPolicy = "never"
    DaemonRestartOnFailure DaemonRestartPolicy = "on-failure"
    DaemonRestartAlways    DaemonRestartPolicy = "always"
)
```

### OpenPRStrategy
```go
type OpenPRStrategy string
const (
    OpenPRStrategyAllDone     OpenPRStrategy = "all_done"
    OpenPRStrategyFirstCommit OpenPRStrategy = "first_commit"
    OpenPRStrategyManual      OpenPRStrategy = "manual"
)
```

### AddSnapshotMode
```go
type AddSnapshotMode string
const (
    AddSnapshotModeAsync AddSnapshotMode = "async"
    AddSnapshotModeFull  AddSnapshotMode = "full"
    AddSnapshotModeOff   AddSnapshotMode = "off"
)
```

### LabelMode
```go
type LabelMode string
const (
    LabelModeAll LabelMode = "all"
    LabelModeAny LabelMode = "any"
)
```

### FixerAuthorFilter
```go
type FixerAuthorFilter string
const (
    FixerAuthorFilterCurrentUser FixerAuthorFilter = "current_user"
    FixerAuthorFilterAny         FixerAuthorFilter = "any"
)
```

### ReviewerScope
```go
type ReviewerScope string
const (
    ReviewerScopeFullPR        ReviewerScope = "full_pr"
    ReviewerScopeChangedFiles  ReviewerScope = "changed_files"
    ReviewerScopeChangedRanges ReviewerScope = "changed_ranges"
)
```

### ReviewerPublishMode
```go
type ReviewerPublishMode string
const (
    ReviewerPublishModeSingleReview ReviewerPublishMode = "single_review"
)
```

### ReviewerThreadResolutionMode
```go
type ReviewerThreadResolutionMode string
const (
    ReviewerThreadResolutionModeReportOnly        ReviewerThreadResolutionMode = "report_only"
    ReviewerThreadResolutionModeCommentOnly       ReviewerThreadResolutionMode = "comment_only"
    ReviewerThreadResolutionModeSuggestResolution ReviewerThreadResolutionMode = "suggest_resolution"
    ReviewerThreadResolutionModeResolveObjective  ReviewerThreadResolutionMode = "resolve_objective"
)
```

### ReviewerThreadResolutionScope
```go
type ReviewerThreadResolutionScope string
const (
    ReviewerThreadResolutionScopeLooperAuthoredOnly ReviewerThreadResolutionScope = "looper_authored_only"
)
```

### ReviewerThreadResolutionAutoResolve
```go
type ReviewerThreadResolutionAutoResolve string
const (
    ReviewerThreadResolutionAutoResolveObjectiveOnly ReviewerThreadResolutionAutoResolve = "objective_only"
)
```

### ReviewerReviewEvent
```go
type ReviewerReviewEvent string
const (
    ReviewerReviewEventComment        ReviewerReviewEvent = "COMMENT"
    ReviewerReviewEventApprove        ReviewerReviewEvent = "APPROVE"
    ReviewerReviewEventRequestChanges ReviewerReviewEvent = "REQUEST_CHANGES"
)
```

### ReviewerAutoMergeStrategy
```go
type ReviewerAutoMergeStrategy string
const (
    ReviewerAutoMergeStrategySquash ReviewerAutoMergeStrategy = "squash"
    ReviewerAutoMergeStrategyMerge  ReviewerAutoMergeStrategy = "merge"
    ReviewerAutoMergeStrategyRebase ReviewerAutoMergeStrategy = "rebase"
)
```

### ReviewerAutoMergeScope
```go
type ReviewerAutoMergeScope string
const (
    ReviewerAutoMergeScopeLooperOnly ReviewerAutoMergeScope = "looper-only"
)
```

### NotificationSoundLevel
```go
type NotificationSoundLevel string
const (
    NotificationSoundLevelActionRequired NotificationSoundLevel = "action_required"
    NotificationSoundLevelFailure        NotificationSoundLevel = "failure"
)
```

### WebhookMode
```go
type WebhookMode string
const (
    WebhookModeGHForward WebhookMode = "gh-forward"
    WebhookModeTunnel    WebhookMode = "tunnel"
)
```

### ToolDetectionStatus
```go
type ToolDetectionStatus string
const (
    ToolDetectionStatusConfigured ToolDetectionStatus = "configured"
    ToolDetectionStatusDetected   ToolDetectionStatus = "detected"
    ToolDetectionStatusMissing    ToolDetectionStatus = "missing"
)
```

### NetworkMode
```go
type NetworkMode string
const (
    NetworkModeOff    NetworkMode = "off"
    NetworkModeRouted NetworkMode = "routed"
)
type ProjectNetworkMode = NetworkMode  // alias
```

### DiscoveryCache fields (from validHermesEventFlags, validate.go L704-708)
```go
validHermesEventFlags = map[string]bool{
    "prReady":      true,
    "fixPushed":    true,
    "reviewPosted": true,
}
```

---

## 2. FULL CONFIG STRUCTS (exact fields, types, serde JSON tags)

### ServerConfig
```go
struct ServerConfig {
    Host:       String,           // json:"host"
    Port:       i32,              // json:"port"
    BaseURL:    Option<String>,   // json:"baseUrl,omitempty"
    AuthMode:   AuthMode,         // json:"authMode"
    LocalToken: Option<String>,   // json:"localToken,omitempty"
}
// Default: Host="127.0.0.1", Port=17310, AuthMode=None
```

### StorageConfig
```go
struct StorageConfig {
    Mode:      String,            // json:"mode"
    DBPath:    String,            // json:"dbPath"
    BackupDir: Option<String>,    // json:"backupDir,omitempty"
}
// Default: Mode="sqlite", DBPath="$HOME/.looper/looper.sqlite", BackupDir=Some("$HOME/.looper/backups")
```

### SchedulerConfig
```go
struct SchedulerConfig {
    PollIntervalSeconds:      i32,   // json:"pollIntervalSeconds"     Default: 30
    MaxConcurrentRuns:        i32,   // json:"maxConcurrentRuns"       Default: 3
    RetryMaxAttempts:         i32,   // json:"retryMaxAttempts"        Default: -1 (infinite)
    RetryBaseDelayMS:         i32,   // json:"retryBaseDelayMs"        Default: 5000
    SlowLaneWarnThresholdMS:  i32,   // json:"slowLaneWarnThresholdMs" Default: 5000
    DiscoveryCacheTTLSeconds: i32,   // json:"discoveryCacheTtlSeconds" Default: 30
}
```

### WebhookConfig
```go
struct WebhookConfig {
    Enabled:                     bool,                // json:"enabled"              Default: false
    Mode:                        WebhookMode,         // json:"mode"                 Default: "gh-forward"
    ListenPort:                  i32,                 // json:"listenPort"           Default: 0
    PublicBaseURL:               String,              // json:"publicBaseUrl"        Default: ""
    FallbackPollIntervalSeconds: i32,                 // json:"fallbackPollIntervalSeconds" Default: 300
    Hermes:                      Option<HermesWebhookConfig>, // json:"hermes,omitempty"
}
```

### HermesWebhookConfig
```go
struct HermesWebhookConfig {
    Enabled:    bool,                    // json:"enabled"
    Endpoint:   String,                  // json:"endpoint"
    EventFlags: HashMap<String, bool>,   // json:"eventFlags"  — valid keys: "prReady", "fixPushed", "reviewPosted"
    TimeoutMS:  i32,                     // json:"timeoutMs"
}
```

### AgentConfig
```go
struct AgentConfig {
    Vendor:       Option<AgentVendor>,            // json:"vendor,omitempty"
    Model:        Option<String>,                 // json:"model,omitempty"
    Params:       HashMap<String, Value>,         // json:"params"        Default: empty
    Env:          HashMap<String, String>,         // json:"env"           Default: empty
    Timeouts:     AgentTimeoutConfig,              // json:"timeouts"
    NativeResume: AgentNativeResumeConfig,         // json:"nativeResume"
}
```

### AgentNativeResumeConfig
```go
struct AgentNativeResumeConfig {
    Enabled: bool,   // json:"enabled"  Default: true
}
```

### AgentTimeoutConfig
```go
struct AgentTimeoutConfig {
    PlannerSeconds:                i32,  // Default: 3600 (1h)
    WorkerSeconds:                 i32,  // Default: 10800 (3h)
    ReviewerSeconds:               i32,  // Default: 5400 (90min)
    FixerSeconds:                  i32,  // Default: 7200 (2h)
    PlannerIdleTimeoutSeconds:     i32,  // Default: 600 (10min)
    PlannerMaxRuntimeSeconds:      i32,  // Default: 3600 (1h)
    WorkerIdleTimeoutSeconds:      i32,  // Default: 900 (15min)
    WorkerMaxRuntimeSeconds:       i32,  // Default: 10800 (3h)
    ReviewerIdleTimeoutSeconds:    i32,  // Default: 600 (10min)
    ReviewerMaxRuntimeSeconds:     i32,  // Default: 5400 (90min)
    FixerIdleTimeoutSeconds:       i32,  // Default: 600 (10min)
    FixerMaxRuntimeSeconds:        i32,  // Default: 7200 (2h)
}
// NOTE: mergeAgentTimeoutConfig syncs PlannerSeconds↔PlannerMaxRuntimeSeconds pairs
```

### NotificationConfig
```go
struct NotificationConfig {
    InApp:     bool,                            // json:"inApp"     Default: true
    Osascript: OsascriptNotificationConfig,     // json:"osascript"
}
```

### OsascriptNotificationConfig
```go
struct OsascriptNotificationConfig {
    Enabled:               bool,                    // json:"enabled"  Default: runtime.GOOS == "darwin"
    SoundForLevels:        Vec<NotificationSoundLevel>, // json:"soundForLevels"  Default: [ActionRequired, Failure]
    ThrottleWindowSeconds: i32,                     // json:"throttleWindowSeconds"  Default: 60
}
```

### DisclosureConfig
```go
struct DisclosureConfig {
    Enabled:      bool,                       // json:"enabled"       Default: true
    IncludeAgent: bool,                       // json:"includeAgent"  Default: true
    IncludeOS:    bool,                       // json:"includeOS"     Default: false
    Channels:     DisclosureChannelsConfig,   // json:"channels"
}
```

### DisclosureChannelsConfig
```go
struct DisclosureChannelsConfig {
    GitCommit:            bool,  // json:"gitCommit"            Default: true
    PullRequest:          bool,  // json:"pullRequest"          Default: true
    IssueComment:         bool,  // json:"issueComment"         Default: true
    ReviewComment:        bool,  // json:"reviewComment"        Default: true
    InlineCommentVisible: bool,  // json:"inlineCommentVisible" Default: true
}
```

### InstructionsConfig
```go
struct InstructionsConfig {
    Enabled:  bool,  // json:"enabled"   Default: true
    MaxBytes: i32,   // json:"maxBytes"  Default: 8192
}
```

### RoleConfig
```go
struct RoleConfig {
    Instructions: String,  // json:"instructions,omitempty"
}
```

### LoggingConfig
```go
struct LoggingConfig {
    Level:     LogLevel,  // json:"level"     Default: "info"
    MaxSizeMB: i32,       // json:"maxSizeMB" Default: 10
    MaxFiles:  i32,       // json:"maxFiles"  Default: 5
}
```

### ToolPathsConfig
```go
struct ToolPathsConfig {
    GitPath:       Option<String>,  // json:"gitPath,omitempty"
    GHPath:        Option<String>,  // json:"ghPath,omitempty"
    LooperPath:    Option<String>,  // json:"looperPath,omitempty"
    OsascriptPath: Option<String>,  // json:"osascriptPath,omitempty"
}
```

### DaemonConfig
```go
struct DaemonConfig {
    Mode:                   DaemonMode,             // json:"mode"              Default: "foreground"
    RestartPolicy:          DaemonRestartPolicy,     // json:"restartPolicy"     Default: "on-failure"
    RestartThrottleSeconds: i32,                     // json:"restartThrottleSeconds" Default: 10
    PlistPath:              Option<String>,          // json:"plistPath,omitempty"
    LogDir:                 String,                  // json:"logDir"            Default: "$HOME/.looper/logs"
    ShutdownTimeoutMS:      i32,                     // json:"shutdownTimeoutMs" Default: 1000
    WorkingDirectory:       String,                  // json:"workingDirectory"  Default: CWD at startup
    Environment:            HashMap<String, String>, // json:"environment"       Default: empty
    WorktreeCleanup:        WorktreeCleanupConfig,   // json:"worktreeCleanup"
}
```

### WorktreeCleanupConfig
```go
struct WorktreeCleanupConfig {
    Enabled:        bool,   // json:"enabled"        Default: true
    Interval:       String, // json:"interval"       Default: "24h"  (Go time.Duration)
    RetentionDays:  i32,    // json:"retentionDays"  Default: 7
    MaxPerTick:     i32,    // json:"maxPerTick"     Default: 10
    IncludeOrphans: bool,   // json:"includeOrphans" Default: false
    DryRun:         bool,   // json:"dryRun"         Default: false
}
```

### PackageConfig
```go
struct PackageConfig {
    Distribution:               String,  // json:"distribution"                Default: "github-release"
    AutoUpgradeEnabled:         bool,    // json:"autoUpgradeEnabled"          Default: true
    AutoMigrateOnStartup:       bool,    // json:"autoMigrateOnStartup"        Default: true
    RequireBackupBeforeMigrate: bool,    // json:"requireBackupBeforeMigrate"  Default: false
}
```

### NetworkConfig
```go
struct NetworkConfig {
    Enrolled:         bool,    // json:"enrolled"         Default: false
    LoopernetBaseURL: String,  // json:"loopernetBaseUrl" Default: ""
    NodeName:         String,  // json:"nodeName"         Default: ""
    GitHubLogin:      String,  // json:"githubLogin"      Default: ""
    GitHubUserID:     i64,     // json:"githubUserId,omitempty" Default: 0
}
```

### ProjectNetworkConfig
```go
struct ProjectNetworkConfig {
    Mode: NetworkMode,  // json:"mode,omitempty"  Default: ""→"off"
}
```

### DefaultsConfig
```go
struct DefaultsConfig {
    BaseBranch:         String,          // json:"baseBranch"         Default: "main"
    AllowAutoCommit:    bool,            // json:"allowAutoCommit"    Default: true
    AllowAutoPush:      bool,            // json:"allowAutoPush"      Default: true
    AllowAutoApprove:   bool,            // json:"allowAutoApprove"   Default: true
    AllowAutoMerge:     bool,            // json:"allowAutoMerge"     Default: false
    AllowRiskyFixes:    bool,            // json:"allowRiskyFixes"    Default: false
    FixAllPullRequests: bool,            // json:"fixAllPullRequests" Default: false
    OpenPRStrategy:     OpenPRStrategy,  // json:"openPrStrategy"     Default: "all_done"
    AddSnapshotMode:    AddSnapshotMode, // json:"addSnapshotMode"    Default: "async"
}
```

### ReviewerLoopConfig
```go
struct ReviewerLoopConfig {
    EnabledByDefault:          bool,  // json:"enabledByDefault"          Default: true
    QuietPeriodSeconds:        i32,   // json:"quietPeriodSeconds"        Default: 60
    MinPublishIntervalSeconds: i32,   // json:"minPublishIntervalSeconds" Default: 300
    MaxIterationsPerPR:        i32,   // json:"maxIterationsPerPR"        Default: 20
    MaxIterationsPerHead:      i32,   // json:"maxIterationsPerHead"      Default: 1
    MaxWallClockSeconds:       i32,   // json:"maxWallClockSeconds"       Default: 0 (unlimited)
    MaxConsecutiveFailures:    i32,   // json:"maxConsecutiveFailures"    Default: 3
    MaxAgentExecutionsPerPR:   i32,   // json:"maxAgentExecutionsPerPR"   Default: 25
    StopOnApproved:            bool,  // json:"stopOnApproved"            Default: false
    StopOnReadyLabel:          bool,  // json:"stopOnReadyLabel"          Default: true
    StopOnIdenticalOutput:     bool,  // json:"stopOnIdenticalOutput"     Default: true
}
```

### ReviewerConfig
```go
struct ReviewerConfig {
    Loop:                    ReviewerLoopConfig,                    // json:"loop"
    Retry:                   ReviewerRetryConfig,                   // json:"retry"
    Scope:                   ReviewerScope,                         // json:"scope"         Default: "changed_ranges"
    PublishMode:             ReviewerPublishMode,                   // json:"publishMode"   Default: "single_review"
    ReviewEvents:            ReviewerReviewEventsConfig,            // json:"reviewEvents"
    DetectDuplicateFindings: bool,                                  // json:"detectDuplicateFindings" Default: true
    NativeResume:            ReviewerNativeResumeConfig,            // json:"nativeResume"
    ThreadResolution:        ReviewerThreadResolutionConfig,        // json:"threadResolution"
}
```

### ReviewerRetryConfig
```go
struct ReviewerRetryConfig {
    EnhancedTransientClassification: bool,        // json:"enhancedTransientClassification" Default: false
    ExtraTransientErrorPatterns:     Vec<String>, // json:"extraTransientErrorPatterns"     Default: []
    RecoverExistingMatchedFailures:  bool,        // json:"recoverExistingMatchedFailures"  Default: false
    AutoRecoveryMaxAttempts:         i32,         // json:"autoRecoveryMaxAttempts"         Default: 3
    MaxDelayMS:                      i32,         // json:"maxDelayMs"                      Default: 300000
}
// Default function: DefaultReviewerRetryConfig()
```

### ReviewerReviewEventsConfig
```go
struct ReviewerReviewEventsConfig {
    Clean:    ReviewerReviewEvent,  // json:"clean"    Default: "APPROVE"
    Blocking: ReviewerReviewEvent,  // json:"blocking" Default: "REQUEST_CHANGES"
}
```

### ReviewerNativeResumeConfig
```go
struct ReviewerNativeResumeConfig {
    OnHeadChange:               bool,  // json:"onHeadChange"               Default: false
    ReReviewPromptOnHeadChange: bool,  // json:"reReviewPromptOnHeadChange" Default: false
}
```

### ReviewerThreadResolutionConfig
```go
struct ReviewerThreadResolutionConfig {
    Enabled:                     bool,                                // json:"enabled"                    Default: false
    Mode:                        ReviewerThreadResolutionMode,        // json:"mode"                       Default: "report_only"
    Scope:                       ReviewerThreadResolutionScope,       // json:"scope"                      Default: "looper_authored_only"
    AutoResolve:                 ReviewerThreadResolutionAutoResolve, // json:"autoResolve"                 Default: "objective_only"
    RequireAuditComment:         bool,                                // json:"requireAuditComment"        Default: true
    RequireNewHeadSinceThread:   bool,                                // json:"requireNewHeadSinceThread"  Default: true
    RequireCurrentReviewRequest: bool,                                // json:"requireCurrentReviewRequest" Default: true
    MaxThreadsPerRun:            i32,                                 // json:"maxThreadsPerRun"           Default: 10
}
```

### ReviewerAutoMergeConfig
```go
struct ReviewerAutoMergeConfig {
    Enabled:                 bool,                      // json:"enabled"                 Default: false
    Strategy:                ReviewerAutoMergeStrategy, // json:"strategy"                Default: "squash"
    RequireBranchProtection: bool,                      // json:"requireBranchProtection" Default: true
    TransientRetries:        i32,                       // json:"transientRetries"        Default: 3
    Scope:                   ReviewerAutoMergeScope,    // json:"scope"                   Default: "looper-only"
}
```

### IssueRoleTriggersConfig
```go
struct IssueRoleTriggersConfig {
    Labels:                     Vec<String>,  // json:"labels"
    LabelMode:                  LabelMode,     // json:"labelMode"
    RequireAssigneeCurrentUser: bool,          // json:"requireAssigneeCurrentUser"
}
```

### PullRequestRoleTriggersConfig
```go
struct PullRequestRoleTriggersConfig {
    IncludeDrafts:        bool,  // json:"includeDrafts"
    RequireReviewRequest: bool,  // json:"requireReviewRequest"
}
```

### ReviewerRoleTriggersConfig
```go
struct ReviewerRoleTriggersConfig {
    IncludeDrafts:        bool,       // json:"includeDrafts"
    RequireReviewRequest: bool,       // json:"requireReviewRequest"
    EnableSelfReview:     bool,       // json:"enableSelfReview,omitempty"
    Labels:               Vec<String>, // json:"labels"
    LabelMode:            LabelMode,   // json:"labelMode"
}
```

### ReviewerSpecReviewConfig
```go
struct ReviewerSpecReviewConfig {
    IncludeReviewingLabel: bool,   // json:"includeReviewingLabel" Default: true
    ReviewingLabel:        String, // json:"reviewingLabel" Default: "looper:spec-reviewing"
}
```

### ReviewerRoleDiscoveryConfig
```go
struct ReviewerRoleDiscoveryConfig {
    AutoDiscovery: bool,                         // json:"autoDiscovery" Default: true
    Triggers:      ReviewerRoleTriggersConfig,   // json:"triggers"
    SpecReview:    ReviewerSpecReviewConfig,     // json:"specReview"
}
```

### FixerRoleTriggersConfig
```go
struct FixerRoleTriggersConfig {
    IncludeDrafts: bool,              // json:"includeDrafts"
    AuthorFilter:  FixerAuthorFilter, // json:"authorFilter" Default: "current_user"
    Labels:        Vec<String>,        // json:"labels"      Default: []
    LabelMode:     LabelMode,          // json:"labelMode"   Default: "all"
}
```

### PlannerRoleConfig
```go
struct PlannerRoleConfig {
    AutoDiscovery: bool,                       // json:"autoDiscovery" Default: true
    Triggers:      IssueRoleTriggersConfig,    // json:"triggers"
    Instructions:  String,                     // json:"instructions,omitempty"
}
// Default triggers: Labels=["looper:plan"], LabelMode=All, RequireAssigneeCurrentUser=true
```

### WorkerRoleConfig
```go
struct WorkerRoleConfig {
    AutoDiscovery: bool,                       // json:"autoDiscovery" Default: true
    Triggers:      IssueRoleTriggersConfig,    // json:"triggers"
    Instructions:  String,                     // json:"instructions,omitempty"
}
// Default triggers: Labels=["looper:worker-ready"], LabelMode=All, RequireAssigneeCurrentUser=true
```

### ReviewerRoleConfig
```go
struct ReviewerRoleConfig {
    Discovery:    ReviewerRoleDiscoveryConfig,  // json:"discovery"
    Behavior:     ReviewerConfig,               // json:"behavior"
    AutoMerge:    ReviewerAutoMergeConfig,      // json:"autoMerge"
    Instructions: String,                        // json:"instructions,omitempty"
}
```

### FixerRoleConfig
```go
struct FixerRoleConfig {
    AutoDiscovery: bool,                      // json:"autoDiscovery" Default: true
    Triggers:      FixerRoleTriggersConfig,   // json:"triggers"
    Instructions:  String,                    // json:"instructions,omitempty"
}
// Default triggers: AuthorFilter=current_user, IncludeDrafts=false, Labels=[], LabelMode=all
```

### CoordinatorTriageDispositionConfig
```go
struct CoordinatorTriageDispositionConfig {
    OutOfScopeLabel:       String,  // json:"outOfScopeLabel"       Default: "wontfix"
    UnclearLabel:          String,  // json:"unclearLabel"          Default: "needs-info"
    ReTriageOnAuthorReply: bool,    // json:"reTriageOnAuthorReply" Default: true
}
```

### CoordinatorTriageConfig
```go
struct CoordinatorTriageConfig {
    TriagedLabel:    String,                             // json:"triagedLabel"    Default: "triaged"
    MaxIssueAgeDays: i32,                                // json:"maxIssueAgeDays" Default: 7
    MaxPerTick:      i32,                                // json:"maxPerTick"      Default: 5
    Disposition:     CoordinatorTriageDispositionConfig, // json:"disposition"
}
```

### CoordinatorDispatchHumanGateConfig
```go
struct CoordinatorDispatchHumanGateConfig {
    SlashCommands: Vec<String>,  // json:"slashCommands" Default: ["/plan", "/implement"]
    AllowedUsers:  Vec<String>,  // json:"allowedUsers"  Default: []
}
```

### CoordinatorDispatchAutonomousConfig
```go
struct CoordinatorDispatchAutonomousConfig {
    DelayMinutes: i32,    // json:"delayMinutes" Default: 30
    HoldLabel:    String, // json:"holdLabel"    Default: "looper:hold"
}
```

### CoordinatorDispatchConfig
```go
struct CoordinatorDispatchConfig {
    Mode:       String,                              // json:"mode"       Default: "human-gated"
    HumanGate:  CoordinatorDispatchHumanGateConfig,  // json:"humanGate"
    Autonomous: CoordinatorDispatchAutonomousConfig, // json:"autonomous"
    AssignTo:   String,                              // json:"assignTo"   Default: ""
}
```

### CoordinatorDependenciesConfig
```go
struct CoordinatorDependenciesConfig {
    Enabled:           bool,  // json:"enabled"           Default: false
    APITimeoutSeconds: i32,   // json:"apiTimeoutSeconds" Default: 10
    APIRetryAttempts:  i32,   // json:"apiRetryAttempts"  Default: 3
}
```

### CoordinatorMergeWatchConfig
```go
struct CoordinatorMergeWatchConfig {
    TransientRetries:         i32,    // json:"transientRetries"         Default: 3
    MaxIndeterminateDuration: String, // json:"maxIndeterminateDuration" Default: "15m"
}
```

### CoordinatorRoleConfig
```go
struct CoordinatorRoleConfig {
    Enabled:      bool,                            // json:"enabled"      Default: false
    PollInterval: String,                          // json:"pollInterval" Default: "5m"
    Triage:       CoordinatorTriageConfig,         // json:"triage"
    Dispatch:     CoordinatorDispatchConfig,       // json:"dispatch"
    Dependencies: CoordinatorDependenciesConfig,   // json:"dependencies"
    MergeWatch:   CoordinatorMergeWatchConfig,     // json:"mergeWatch"
}
```

### RoleConfigs
```go
struct RoleConfigs {
    Planner:     PlannerRoleConfig,       // json:"planner"
    Reviewer:    ReviewerRoleConfig,      // json:"reviewer"
    Fixer:       FixerRoleConfig,         // json:"fixer"
    Worker:      WorkerRoleConfig,        // json:"worker"
    Coordinator: CoordinatorRoleConfig,   // json:"coordinator"
}
```

### ProjectRefConfig
```go
struct ProjectRefConfig {
    ID:           String,                // json:"id"
    Name:         String,                // json:"name"
    RepoPath:     String,                // json:"repoPath"
    Path:         String,                // json:"path,omitempty"
    BaseBranch:   Option<String>,        // json:"baseBranch,omitempty"
    WorktreeRoot: Option<String>,        // json:"worktreeRoot,omitempty"
    Network:      ProjectNetworkConfig,  // json:"network,omitempty"
    Webhook:      ProjectWebhookConfig,  // json:"webhook,omitempty"
    Roles:        Option<PartialRoleConfigs>, // json:"roles,omitempty"
}
```

### ProjectWebhookConfig
```go
struct ProjectWebhookConfig {
    Mode: WebhookMode,  // json:"mode,omitempty"
}
```

### Config (TOP-LEVEL)
```go
struct Config {
    Server:        ServerConfig,        // json:"server"
    Storage:       StorageConfig,       // json:"storage"
    Scheduler:     SchedulerConfig,     // json:"scheduler"
    Webhook:       WebhookConfig,       // json:"webhook"
    Network:       NetworkConfig,       // json:"network"
    Agent:         AgentConfig,         // json:"agent"
    Logging:       LoggingConfig,       // json:"logging"
    Notifications: NotificationConfig,  // json:"notifications"
    Disclosure:    DisclosureConfig,    // json:"disclosure"
    Tools:         ToolPathsConfig,     // json:"tools"
    Daemon:        DaemonConfig,        // json:"daemon"
    Package:       PackageConfig,       // json:"package"
    Defaults:      DefaultsConfig,      // json:"defaults"
    Instructions:  InstructionsConfig,  // json:"instructions"
    Roles:         RoleConfigs,         // json:"roles"
    Projects:      Vec<ProjectRefConfig>, // json:"projects"
}
```

---

## 3. PARTIAL CONFIG STRUCTS (all fields Option-wrapped, serde skip_serializing_if = "Option::is_none")

These are the "partial" versions where every field is `Option<T>`. They mirror the full config exactly with `#[serde(skip_serializing_if = "Option::is_none")]`.

```rust
// All fields Optional — naming: Partial*
struct PartialServerConfig {
    host: Option<String>,
    port: Option<i32>,
    base_url: Option<String>,
    auth_mode: Option<AuthMode>,
    local_token: Option<String>,
}

struct PartialStorageConfig {
    mode: Option<String>,
    db_path: Option<String>,
    backup_dir: Option<String>,
}

struct PartialSchedulerConfig {
    poll_interval_seconds: Option<i32>,
    max_concurrent_runs: Option<i32>,
    retry_max_attempts: Option<i32>,
    retry_base_delay_ms: Option<i32>,
    slow_lane_warn_threshold_ms: Option<i32>,
    discovery_cache_ttl_seconds: Option<i32>,
}

struct PartialWebhookConfig {
    enabled: Option<bool>,
    mode: Option<WebhookMode>,
    listen_port: Option<i32>,
    public_base_url: Option<String>,
    fallback_poll_interval_seconds: Option<i32>,
    hermes: Option<HermesWebhookConfig>,
}

struct PartialAgentConfig {
    vendor: Option<AgentVendor>,
    model: Option<String>,
    params: Option<HashMap<String, Value>>,
    env: Option<HashMap<String, String>>,
    timeouts: Option<PartialAgentTimeoutConfig>,
    native_resume: Option<PartialAgentNativeResumeConfig>,
}

struct PartialAgentNativeResumeConfig {
    enabled: Option<bool>,
}

struct PartialAgentTimeoutConfig {
    planner_seconds: Option<i32>,
    worker_seconds: Option<i32>,
    reviewer_seconds: Option<i32>,
    fixer_seconds: Option<i32>,
    planner_idle_timeout_seconds: Option<i32>,
    planner_max_runtime_seconds: Option<i32>,
    worker_idle_timeout_seconds: Option<i32>,
    worker_max_runtime_seconds: Option<i32>,
    reviewer_idle_timeout_seconds: Option<i32>,
    reviewer_max_runtime_seconds: Option<i32>,
    fixer_idle_timeout_seconds: Option<i32>,
    fixer_max_runtime_seconds: Option<i32>,
}

struct PartialNotificationConfig {
    in_app: Option<bool>,
    osascript: Option<PartialOsascriptNotificationConfig>,
}

struct PartialDisclosureConfig {
    enabled: Option<bool>,
    include_agent: Option<bool>,
    include_os: Option<bool>,
    channels: Option<PartialDisclosureChannelsConfig>,
}

struct PartialDisclosureChannelsConfig {
    git_commit: Option<bool>,
    pull_request: Option<bool>,
    issue_comment: Option<bool>,
    review_comment: Option<bool>,
    inline_comment_visible: Option<bool>,
}

struct PartialOsascriptNotificationConfig {
    enabled: Option<bool>,
    sound_for_levels: Option<Vec<NotificationSoundLevel>>,
    throttle_window_seconds: Option<i32>,
}

struct PartialLoggingConfig {
    level: Option<LogLevel>,
    max_size_mb: Option<i32>,
    max_files: Option<i32>,
}

struct PartialToolPathsConfig {
    git_path: Option<String>,
    gh_path: Option<String>,
    looper_path: Option<String>,
    osascript_path: Option<String>,
}

struct PartialDaemonConfig {
    mode: Option<DaemonMode>,
    restart_policy: Option<DaemonRestartPolicy>,
    restart_throttle_seconds: Option<i32>,
    plist_path: Option<String>,
    log_dir: Option<String>,
    shutdown_timeout_ms: Option<i32>,
    working_directory: Option<String>,
    environment: Option<HashMap<String, String>>,
    worktree_cleanup: Option<PartialWorktreeCleanupConfig>,
}

struct PartialWorktreeCleanupConfig {
    enabled: Option<bool>,
    interval: Option<String>,
    retention_days: Option<i32>,
    max_per_tick: Option<i32>,
    include_orphans: Option<bool>,
    dry_run: Option<bool>,
}

struct PartialPackageConfig {
    distribution: Option<String>,
    auto_upgrade_enabled: Option<bool>,
    auto_migrate_on_startup: Option<bool>,
    require_backup_before_migrate: Option<bool>,
}

struct PartialNetworkConfig {
    enrolled: Option<bool>,
    loopernet_base_url: Option<String>,
    node_name: Option<String>,
    github_login: Option<String>,
    github_user_id: Option<i64>,
}

struct PartialDefaultsConfig {
    base_branch: Option<String>,
    allow_auto_commit: Option<bool>,
    allow_auto_push: Option<bool>,
    allow_auto_approve: Option<bool>,
    allow_auto_merge: Option<bool>,
    allow_risky_fixes: Option<bool>,
    fix_all_pull_requests: Option<bool>,
    open_pr_strategy: Option<OpenPRStrategy>,
    add_snapshot_mode: Option<AddSnapshotMode>,
}

struct PartialReviewerLoopConfig {
    enabled_by_default: Option<bool>,
    quiet_period_seconds: Option<i32>,
    min_publish_interval_seconds: Option<i32>,
    max_iterations_per_pr: Option<i32>,
    max_iterations_per_head: Option<i32>,
    max_wall_clock_seconds: Option<i32>,
    max_consecutive_failures: Option<i32>,
    max_agent_executions_per_pr: Option<i32>,
    stop_on_approved: Option<bool>,
    stop_on_ready_label: Option<bool>,
    stop_on_identical_output: Option<bool>,
}

struct PartialReviewerConfig {
    loop: Option<PartialReviewerLoopConfig>,
    retry: Option<PartialReviewerRetryConfig>,
    scope: Option<ReviewerScope>,
    publish_mode: Option<ReviewerPublishMode>,
    review_events: Option<PartialReviewerReviewEventsConfig>,
    detect_duplicate_findings: Option<bool>,
    dedupe_findings: Option<bool>,   // NOTE: exists in PartialReviewerConfig but not in ReviewerConfig
    native_resume: Option<PartialReviewerNativeResumeConfig>,
    thread_resolution: Option<PartialReviewerThreadResolutionConfig>,
}

struct PartialReviewerRetryConfig {
    enhanced_transient_classification: Option<bool>,
    extra_transient_error_patterns: Option<Vec<String>>,
    recover_existing_matched_failures: Option<bool>,
    auto_recovery_max_attempts: Option<i32>,
    max_delay_ms: Option<i32>,
}

struct PartialReviewerReviewEventsConfig {
    clean: Option<ReviewerReviewEvent>,
    blocking: Option<ReviewerReviewEvent>,
}

struct PartialReviewerNativeResumeConfig {
    on_head_change: Option<bool>,
    re_review_prompt_on_head_change: Option<bool>,
}

struct PartialReviewerThreadResolutionConfig {
    enabled: Option<bool>,
    mode: Option<ReviewerThreadResolutionMode>,
    scope: Option<ReviewerThreadResolutionScope>,
    auto_resolve: Option<ReviewerThreadResolutionAutoResolve>,
    require_audit_comment: Option<bool>,
    require_new_head_since_thread: Option<bool>,
    require_current_review_request: Option<bool>,
    max_threads_per_run: Option<i32>,
}

struct PartialReviewerAutoMergeConfig {
    enabled: Option<bool>,
    strategy: Option<ReviewerAutoMergeStrategy>,
    require_branch_protection: Option<bool>,
    transient_retries: Option<i32>,
    scope: Option<ReviewerAutoMergeScope>,
}

struct PartialInstructionsConfig {
    enabled: Option<bool>,
    max_bytes: Option<i32>,
}

struct PartialIssueRoleTriggersConfig {
    labels: Option<Vec<String>>,
    label_mode: Option<LabelMode>,
    require_assignee_current_user: Option<bool>,
}

struct PartialPullRequestRoleTriggersConfig {
    include_drafts: Option<bool>,
    require_review_request: Option<bool>,
}

struct PartialReviewerRoleTriggersConfig {
    include_drafts: Option<bool>,
    require_review_request: Option<bool>,
    enable_self_review: Option<bool>,
    labels: Option<Vec<String>>,
    label_mode: Option<LabelMode>,
}

struct PartialReviewerSpecReviewConfig {
    include_reviewing_label: Option<bool>,
    reviewing_label: Option<String>,
}

struct PartialReviewerRoleDiscoveryConfig {
    auto_discovery: Option<bool>,
    triggers: Option<PartialReviewerRoleTriggersConfig>,
    spec_review: Option<PartialReviewerSpecReviewConfig>,
}

struct PartialFixerRoleTriggersConfig {
    include_drafts: Option<bool>,
    author_filter: Option<FixerAuthorFilter>,
    labels: Option<Vec<String>>,
    label_mode: Option<LabelMode>,
}

struct PartialPlannerRoleConfig {
    auto_discovery: Option<bool>,
    triggers: Option<PartialIssueRoleTriggersConfig>,
    instructions: Option<String>,
}

struct PartialWorkerRoleConfig {
    auto_discovery: Option<bool>,
    triggers: Option<PartialIssueRoleTriggersConfig>,
    instructions: Option<String>,
}

struct PartialReviewerRoleConfig {
    discovery: Option<PartialReviewerRoleDiscoveryConfig>,
    behavior: Option<PartialReviewerConfig>,
    auto_merge: Option<PartialReviewerAutoMergeConfig>,
    instructions: Option<String>,
    // Legacy flatten
    auto_discovery: Option<bool>,
    triggers: Option<PartialReviewerRoleTriggersConfig>,
    spec_review: Option<PartialReviewerSpecReviewConfig>,
}

struct PartialFixerRoleConfig {
    auto_discovery: Option<bool>,
    triggers: Option<PartialFixerRoleTriggersConfig>,
    instructions: Option<String>,
}

struct PartialCoordinatorTriageDispositionConfig {
    out_of_scope_label: Option<String>,
    unclear_label: Option<String>,
    re_triage_on_author_reply: Option<bool>,
}

struct PartialCoordinatorTriageConfig {
    triaged_label: Option<String>,
    max_issue_age_days: Option<i32>,
    max_per_tick: Option<i32>,
    disposition: Option<PartialCoordinatorTriageDispositionConfig>,
}

struct PartialCoordinatorDispatchHumanGateConfig {
    slash_commands: Option<Vec<String>>,
    allowed_users: Option<Vec<String>>,
}

struct PartialCoordinatorDispatchAutonomousConfig {
    delay_minutes: Option<i32>,
    hold_label: Option<String>,
}

struct PartialCoordinatorDispatchConfig {
    mode: Option<String>,
    human_gate: Option<PartialCoordinatorDispatchHumanGateConfig>,
    autonomous: Option<PartialCoordinatorDispatchAutonomousConfig>,
    assign_to: Option<String>,
}

struct PartialCoordinatorDependenciesConfig {
    enabled: Option<bool>,
    api_timeout_seconds: Option<i32>,
    api_retry_attempts: Option<i32>,
}

struct PartialCoordinatorMergeWatchConfig {
    transient_retries: Option<i32>,
    max_indeterminate_duration: Option<String>,
}

struct PartialCoordinatorRoleConfig {
    enabled: Option<bool>,
    poll_interval: Option<String>,
    triage: Option<PartialCoordinatorTriageConfig>,
    dispatch: Option<PartialCoordinatorDispatchConfig>,
    dependencies: Option<PartialCoordinatorDependenciesConfig>,
    merge_watch: Option<PartialCoordinatorMergeWatchConfig>,
}

struct PartialRoleConfigs {
    planner: Option<PartialPlannerRoleConfig>,
    reviewer: Option<PartialReviewerRoleConfig>,
    fixer: Option<PartialFixerRoleConfig>,
    worker: Option<PartialWorkerRoleConfig>,
    coordinator: Option<PartialCoordinatorRoleConfig>,
    // Deprecated: sweeper → ignored
    sweeper: Option<HashMap<String, Value>>, // json:"sweeper,omitempty" — ignored on read
}

struct PartialProjectRefConfig {
    id: String,
    name: String,
    repo_path: String,
    path: String,
    base_branch: Option<String>,
    worktree_root: Option<String>,
    network: Option<PartialProjectNetworkConfig>,
    webhook: Option<PartialProjectWebhookConfig>,
    instructions: Option<HashMap<String, String>>,  // role → instruction text
    roles: Option<PartialRoleConfigs>,
}

struct PartialProjectNetworkConfig {
    mode: Option<NetworkMode>,
}

struct PartialProjectWebhookConfig {
    mode: Option<WebhookMode>,
}

struct PartialConfig {  // TOP-LEVEL PARTIAL
    server: Option<PartialServerConfig>,
    storage: Option<PartialStorageConfig>,
    scheduler: Option<PartialSchedulerConfig>,
    webhook: Option<PartialWebhookConfig>,
    network: Option<PartialNetworkConfig>,
    agent: Option<PartialAgentConfig>,
    logging: Option<PartialLoggingConfig>,
    notifications: Option<PartialNotificationConfig>,
    disclosure: Option<PartialDisclosureConfig>,
    tools: Option<PartialToolPathsConfig>,
    daemon: Option<PartialDaemonConfig>,
    package: Option<PartialPackageConfig>,
    defaults: Option<PartialDefaultsConfig>,
    legacy_reviewer: Option<PartialReviewerConfig>,  // json:"reviewer,omitempty" — top-level "reviewer" section
    instructions: Option<PartialInstructionsConfig>,
    roles: Option<PartialRoleConfigs>,
    projects: Option<Vec<PartialProjectRefConfig>>,
}
```

---

## 4. PUBLIC FUNCTIONS

### Load module (load.go)

```rust
// Types
struct LoadFileMetadata {
    config_path: String,
    config_file_present: bool,
    tool_detection: HashMap<String, ToolDetectionStatus>,
}

struct LoadedFileConfig {
    config: Config,
    metadata: LoadFileMetadata,
    partial: PartialConfig,
    warnings: Vec<String>,
    notices: Vec<String>,
}

struct LoadFileOptions {
    cwd: String,
    config_path: String,
    default_config_path: String,
    args: Vec<String>,
    lookup_env: fn(String) -> Option<String>,
    look_path: fn(String) -> Option<String>,
}

// Functions
fn resolve_config_path(path: &str, cwd: &str) -> String
fn load_file(options: LoadFileOptions) -> Result<LoadedFileConfig, ConfigValidationError>
fn discover_default_config_path() -> Result<String, ConfigError>
fn read_config_file(path: &str) -> Result<(PartialConfig, bool), ConfigError>
fn validate_config_file_suffix(path: &str) -> Result<(), ConfigError>
fn decode_config_file(path: &str, raw: &[u8]) -> Result<PartialConfig, ConfigError>
fn build_env_overrides(lookup_env: EnvLookupFunc) -> Result<PartialConfig, ConfigError>
fn collect_deprecated_env_warnings(lookup_env: EnvLookupFunc) -> Vec<String>
fn collect_deprecated_cli_warnings(args: &[String]) -> Vec<String>
fn collect_mixed_schema_warnings(partial: &PartialConfig) -> Vec<String>
fn collect_config_load_notices(path: &str, present: bool) -> Result<Vec<String>, ConfigError>

// LookPath function type
type LookPathFunc = fn(String) -> Result<String, Error>;
```

### Validate module (validate.go)

```rust
struct ValidationIssue {
    path: String,
    message: String,
}

struct ConfigValidationError {
    issues: Vec<ValidationIssue>,
}

struct ValidateOptions {
    default_worktree_root: String,
}

fn validate(config: &Config) -> Result<(), ConfigValidationError>
fn validate_with_options(config: &Config, options: ValidateOptions) -> Result<(), ConfigValidationError>
```

### Normalize module (normalize.go)

```rust
fn normalize(cwd: &str, partials: &[PartialConfig]) -> Result<Config, ConfigValidationError>
fn canonicalize_partial_for_migration(partial: PartialConfig) -> PartialConfig
fn normalize_layer_partial(partial: PartialConfig) -> PartialConfig
fn merge_config(config: &mut Config, partial: PartialConfig)
fn merge_server_config(config: &mut ServerConfig, partial: PartialServerConfig)
// ... plus merge_* for every sub-config
```

### Defaults module (defaults.go)

```rust
const DEFAULT_SERVER_PORT: i32 = 17310;
const DEFAULT_REVIEWER_AUTO_RECOVERY_MAX_ATTEMPTS: i32 = 3;
const DEFAULT_REVIEWER_RETRY_MAX_DELAY_MS: i32 = 300000;

fn default_looper_home() -> Result<String, Error>
fn default_config_path() -> Result<String, Error>
fn default_worktree_root() -> Result<String, Error>
fn default_project_worktree_root(project_id: &str, repo_identity: &str) -> Result<String, Error>
fn default_config(cwd: &str) -> Result<Config, Error>
fn default_reviewer_retry_config() -> ReviewerRetryConfig
fn default_disclosure_config() -> DisclosureConfig
```

### Disclosure (disclosure.go)

```rust
const MARKER: &str = "<!-- looper:stamp v=1 -->";
const REPO_URL: &str = "https://github.com/nexu-io/looper";
const LEGACY_REPO_URL: &str = "https://github.com/powerformer/looper";
const SLOGAN: &str = "An autonomous AI dev team for your GitHub repos.";
const EMOJI: &str = "🔁";

const CHANNEL_GIT_COMMIT: &str = "gitCommit";
const CHANNEL_PULL_REQUEST: &str = "pullRequest";
const CHANNEL_ISSUE_COMMENT: &str = "issueComment";
const CHANNEL_REVIEW_COMMENT: &str = "reviewComment";

struct Stamper {
    config: DisclosureConfig,
    version: String,
    agent: String,
    model: String,
}

fn from_config(cfg: &Config) -> Stamper
fn (s: &Stamper) commit_message(message: &str, runner: &str) -> String
fn (s: &Stamper) markdown(body: &str, runner: &str, channel: &str) -> String
fn (s: &Stamper) markdown_stamp(runner: &str) -> String
fn (s: &Stamper) review_comment(body: &str, runner: &str) -> String
fn (s: &Stamper) enabled(channel: &str) -> bool
fn (s: &Stamper) markdown_footer(runner: &str) -> String
fn (s: &Stamper) commit_trailer(runner: &str) -> String
fn (s: &Stamper) attributes(runner: &str) -> Vec<String>

fn strip_markdown_stamp(body: &str) -> String
fn has_markdown_stamp(body: &str) -> bool
fn os_family(goos: &str) -> &'static str
fn safe_value(value: &str) -> String
```

### DiffAnchor (diffanchor.go)

```rust
const SIDE_RIGHT: &str = "RIGHT";
const SIDE_LEFT: &str = "LEFT";

struct Anchor {
    path: String,
    line: i64,
    side: String,
    start_line: i64,
    start_side: String,
}

struct Range {
    path: String,
    side: String,
    start: i64,
    end: i64,
    excerpt: String,
    heading: String,
}

struct Index {
    ranges: Vec<Range>,
}

struct ValidationResult {
    valid: bool,
    reason: String,
    location_text: String,
    quality_flagged: bool,
}

fn parse(diff: &str) -> Index
fn (idx: &Index) format_prompt_section(limit: usize) -> String
fn (idx: &Index) validate(anchor: Anchor) -> ValidationResult
fn validate_top_level_location(body: &str) -> ValidationResult
fn fallback_body(body: &str, anchor: Anchor, reason: &str) -> String
fn fallback_location(anchor: Anchor) -> String
fn normalize_side(side: &str) -> String
```

---

## 5. VALIDATION RULES (from validate.go)

| Path | Rule |
|------|------|
| `server.host` | non-empty string |
| `server.port` | 1-65535 |
| `server.authMode` | one of: none, local-token |
| `server.localToken` | required when authMode=local-token |
| `storage.mode` | must be "sqlite" |
| `storage.dbPath` | non-empty path |
| `scheduler.pollIntervalSeconds` | >= 10 |
| `scheduler.maxConcurrentRuns` | >= 1 |
| `scheduler.retryMaxAttempts` | -1 or positive |
| `scheduler.retryBaseDelayMs` | >= 1 |
| `scheduler.slowLaneWarnThresholdMs` | >= 1 |
| `scheduler.discoveryCacheTtlSeconds` | >= 0 |
| `webhook.fallbackPollIntervalSeconds` | >= 60 |
| `webhook.mode` | one of: gh-forward, tunnel |
| `webhook.listenPort` (tunnel) | 1024-65535 |
| `webhook.publicBaseUrl` (tunnel) | valid https URL |
| `webhook.hermes.endpoint` | absolute http(s) URL |
| `agent.vendor` | one of 5 vendors |
| `agent.timeouts.*` | positive ints, must fit in time.Duration |
| `logging.level` | one of: debug, info, warn, error |
| `logging.maxSizeMB` | >= 1 |
| `logging.maxFiles` | >= 1 |
| `notifications.osascript.throttleWindowSeconds` | >= 1 |
| `notifications.osascript.soundForLevels` | valid enum values |
| `daemon.mode` | one of: foreground, launchd |
| `daemon.restartPolicy` | one of: never, on-failure, always |
| `daemon.restartThrottleSeconds` | >= 1 |
| `daemon.logDir` | non-empty |
| `daemon.shutdownTimeoutMs` | >= 1 |
| `daemon.workingDirectory` | non-empty |
| `daemon.worktreeCleanup.interval` | valid positive duration |
| `daemon.worktreeCleanup.retentionDays` | >= 0 |
| `daemon.worktreeCleanup.maxPerTick` | >= 1 |
| `package.distribution` | non-empty |
| `defaults.baseBranch` | non-empty |
| `defaults.openPrStrategy` | one of 3 |
| `defaults.addSnapshotMode` | one of 3 |
| `roles.reviewer.behavior.loop.quietPeriodSeconds` | >= 0 |
| `roles.reviewer.behavior.loop.minPublishIntervalSeconds` | >= 0 |
| `roles.reviewer.behavior.retry.autoRecoveryMaxAttempts` | >= 1 |
| `roles.reviewer.behavior.retry.maxDelayMs` | >= 1 |
| `roles.reviewer.behavior.scope` | one of 3 |
| `roles.reviewer.behavior.publishMode` | must be "single_review" |
| `roles.reviewer.behavior.threadResolution.mode` | one of 4 |
| `roles.reviewer.behavior.threadResolution.scope` | must be "looper_authored_only" |
| `roles.reviewer.behavior.threadResolution.autoResolve` | must be "objective_only" |
| `roles.reviewer.behavior.threadResolution.maxThreadsPerRun` | >= 1 |
| `roles.reviewer.behavior.threadResolution.requireAuditComment` | true when mode=resolve_objective |
| `roles.reviewer.autoMerge.strategy` | one of 3 |
| `roles.reviewer.autoMerge.transientRetries` | >= 1 |
| `roles.reviewer.autoMerge.scope` | must be "looper-only" |
| `roles.reviewer.behavior.reviewEvents.clean` | COMMENT or APPROVE |
| `roles.reviewer.behavior.reviewEvents.blocking` | COMMENT or REQUEST_CHANGES |
| `instructions.maxBytes` | >= 1 |
| `roles.*.instructions` | <= maxBytes, no protected phrases |
| `projects[*].id` | non-empty, no path separators, unique |
| `projects[*].name` | non-empty |
| `projects[*].repoPath` | non-empty |
| `projects[*].path` | must match repoPath when both set |
| `projects[*].webhook.mode` | one of 2 or empty |
| `projects[*].network.mode` | one of 2 |
| `coordinator.pollInterval` | valid duration |
| `coordinator.triage.maxIssueAgeDays` | >= 1 |
| `coordinator.triage.maxPerTick` | >= 1 |
| `coordinator.triage.triagedLabel` | non-empty, no whitespace |
| `coordinator.triage.disposition.outOfScopeLabel` | non-empty, no whitespace |
| `coordinator.triage.disposition.unclearLabel` | non-empty, no whitespace |
| `coordinator.dispatch.mode` | "human-gated" or "autonomous" |
| `coordinator.dispatch.humanGate.slashCommands` | at least 1, only /plan or /implement |
| `coordinator.dispatch.autonomous.delayMinutes` | >= 1 |
| `coordinator.dispatch.autonomous.holdLabel` | non-empty |
| `coordinator.dependencies.apiTimeoutSeconds` | >= 1 when enabled |
| `coordinator.dependencies.apiRetryAttempts` | >= 1 when enabled |
| `coordinator.mergeWatch.transientRetries` | >= 1 |
| `coordinator.mergeWatch.maxIndeterminateDuration` | valid duration |
| Writable path check | `storage.dbPath` parent writable, `daemon.logDir` writable, `daemon.workingDirectory` writable, `defaults.worktreeRoot` writable |

Protected instruction phrases (blocked in custom instructions):
`systemprompt`, `system prompt`, `__looper_result__`, `completion marker`, `git_pr_lifecycle`, `summary field`, `commits field`, `result json`, `allowautopush`, `allowautoapprove`, `allow auto push`, `allow auto approve`, `auto approve`, `auto push`, `pr creation policy`, `review submission policy`, `looper review submit`, `review submit wrapper`, `gh pr review`, `disclosure stamping`, `auth requirement`, `permission boundary`, `state transition`, `state machine`, `ignore lifecycle`, `override lifecycle`, `custom completion`.

---

## 6. ENVIRONMENT VARIABLE MAPPINGS

| Env Var | Config Path |
|---------|-------------|
| `LOOPER_CONFIG` | Config file path override |
| `LOOPER_HOST` | server.host |
| `LOOPER_PORT` | server.port |
| `LOOPER_DB_PATH` | storage.dbPath |
| `LOOPER_LOG_DIR` | daemon.logDir |
| `LOOPER_DAEMON_MODE` | daemon.mode |
| `LOOPER_DAEMON_RESTART_POLICY` | daemon.restartPolicy |
| `LOOPER_DAEMON_RESTART_THROTTLE_SECONDS` | daemon.restartThrottleSeconds |
| `LOOPER_WORKING_DIRECTORY` | daemon.workingDirectory |
| `LOOPER_IN_APP_NOTIFICATIONS` | notifications.inApp |
| `LOOPER_OSASCRIPT_ENABLED` | notifications.osascript.enabled |
| `LOOPER_AUTO_UPGRADE_ENABLED` | package.autoUpgradeEnabled |
| `LOOPER_ALLOW_AUTO_COMMIT` | defaults.allowAutoCommit |
| `LOOPER_ALLOW_AUTO_PUSH` | defaults.allowAutoPush |
| `LOOPER_ALLOW_AUTO_APPROVE` | defaults.allowAutoApprove + reviewEvents.clean |
| `LOOPER_FIX_ALL_PULL_REQUESTS` | defaults.fixAllPullRequests + authorFilter |
| `LOOPER_GIT_PATH` | tools.gitPath |
| `LOOPER_GH_PATH` | tools.ghPath |
| `LOOPER_LOOPER_PATH` | tools.looperPath |
| `LOOPER_OSASCRIPT_PATH` | tools.osascriptPath |
| `LOOPER_AGENT_NATIVE_RESUME_ENABLED` | agent.nativeResume.enabled |
| `LOOPER_AGENT_TIMEOUTS_*` | agent.timeouts.* |
| `LOOPER_ROLES_REVIEWER_BEHAVIOR_*` | roles.reviewer.behavior.* |
| `LOOPER_ROLES_REVIEWER_DISCOVERY_*` | roles.reviewer.discovery.* |
| `LOOPER_ROLES_*_AUTO_DISCOVERY` | roles.*.autoDiscovery |
| `LOOPER_ROLES_*_TRIGGERS_*` | roles.*.triggers.* |

---

## 7. CONFIG FILE FORMAT

- Supported formats: `.toml`, `.yaml`, `.yml`, `.json`
- Precedence: Defaults → Config File → Env Vars → CLI Flags
- Default config directory: `$HOME/.looper/`
- Default config names (in order): `config.toml`, `config.yaml`, `config.yml`, `config.json`
- Config loading: Read top-level sections case-insensitively
- JSON decoder uses `DisallowUnknownFields` for strict parsing
- YAML/TOML are decoded into `map[string]any` then re-encoded as JSON for uniform section decoding
- `PartialConfig` is the intermediate representation (all Option fields)
- `Normalize()` applies defaults then merges partials in order: file → env → CLI
