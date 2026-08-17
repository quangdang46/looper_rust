# Code Review Checklist

## Correctness
- [ ] Does the code do what it claims?
- [ ] Are all edge cases handled?
- [ ] Are error conditions properly handled?
- [ ] Are there any off-by-one errors?
- [ ] Is the logic correct for concurrent access?

## Rust-Specific
- [ ] No `unwrap()` in production code (use `?` or `expect()` with context)
- [ ] Proper error types (no `Box<dyn Error>` in public APIs)
- [ ] No unnecessary `clone()` calls
- [ ] Lifetimes are minimal and correct
- [ ] `Send + Sync` bounds are correct for async code
- [ ] No `unsafe` without clear justification

## Architecture
- [ ] Does the change fit the existing architecture?
- [ ] Are new abstractions justified?
- [ ] Are dependencies minimal and appropriate?
- [ ] Is the module structure maintained?

## Testing
- [ ] Are there adequate unit tests?
- [ ] Are edge cases covered?
- [ ] Are error paths tested?
- [ ] Do tests not depend on external resources?

## Performance
- [ ] Are there any obvious performance issues?
- [ ] Is memory usage reasonable?
- [ ] Are allocations minimized where critical?

## Security
- [ ] No secrets in code or logs?
- [ ] Input validation where needed?
- [ ] SQL injection prevented (parameterized queries)?
- [ ] No path traversal vulnerabilities?

## Documentation
- [ ] Are public APIs documented?
- [ ] Are complex algorithms explained?
- [ ] Are TODOs/FIXMEs tracked?
