//! GraphQL query and mutation strings for the GitHub API.

/// GraphQL query to list review threads on a pull request with pagination.
pub const REVIEW_THREADS_QUERY: &str = r#"
query($owner: String!, $name: String!, $prNumber: Int!, $limit: Int!, $after: String) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $prNumber) {
      reviewThreads(first: $limit, after: $after) {
        nodes {
          id
          isResolved
          path
          line
          comments(first: 100) {
            nodes {
              id
              body
              createdAt
              updatedAt
              path
              line
              url
              authorAssociation
              author { login }
              originalCommit { oid }
              commit { oid }
            }
            pageInfo { hasNextPage endCursor }
          }
        }
        pageInfo { hasNextPage endCursor }
      }
    }
  }
}
"#;

/// GraphQL query to view a single review thread by ID with pagination.
pub const REVIEW_THREAD_QUERY: &str = r#"
query($threadId: ID!, $after: String) {
  node(id: $threadId) {
    ... on PullRequestReviewThread {
      comments(first: 100, after: $after) {
        nodes {
          id
          body
          createdAt
          updatedAt
          path
          line
          url
          authorAssociation
          author { login }
          originalCommit { oid }
          commit { oid }
        }
        pageInfo { hasNextPage endCursor }
      }
    }
  }
}
"#;

/// GraphQL mutation to resolve a review thread.
pub const RESOLVE_REVIEW_THREAD_MUTATION: &str = r#"
mutation($threadId: ID!) {
  resolveReviewThread(input: { threadId: $threadId }) {
    thread { id isResolved }
  }
}
"#;

/// GraphQL mutation to add a reply to a review thread.
pub const ADD_REVIEW_THREAD_REPLY_MUTATION: &str = r#"
mutation($threadId: ID!, $body: String!) {
  addPullRequestReviewThreadReply(input: {
    pullRequestReviewThreadId: $threadId,
    body: $body
  }) {
    comment { id }
  }
}
"#;

/// GraphQL query to list pull requests linked to an issue.
pub const LINKED_PULL_REQUESTS_QUERY: &str = r#"
query($owner: String!, $repo: String!, $number: Int!, $after: String) {
  repository(owner: $owner, name: $repo) {
    issue(number: $number) {
      closedByPullRequestsReferences(first: 20, after: $after) {
        nodes {
          number
          state
          mergedAt
          mergeCommit { oid }
        }
        pageInfo { hasNextPage endCursor }
      }
    }
  }
}
"#;

/// GraphQL query to search for PRs by review-requested reviewer.
pub const SEARCH_PRS_BY_REVIEW_REQUESTED_QUERY: &str = r#"
query($searchQuery: String!, $first: Int!) {
  search(type: ISSUE, query: $searchQuery, first: $first) {
    nodes {
      ... on PullRequest {
        number
        title
        url
        state
        updatedAt
        isDraft
        reviewDecision
        labels(first: 20) { nodes { name } }
        headRefName
        baseRefName
        headRefOid
        baseRefOid
        mergeStateStatus
        author { login }
      }
    }
  }
}
"#;

/// GraphQL query to get the current viewer's login.
pub const VIEWER_LOGIN_QUERY: &str = r#"
query { viewer { login } }
"#;
