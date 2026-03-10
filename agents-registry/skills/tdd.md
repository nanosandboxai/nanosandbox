---
name: tdd
description: Test-driven development workflow - red, green, refactor
version: "1.0"
tags: [testing, tdd, quality]
---

# Test-Driven Development

## The Cycle

Follow the Red-Green-Refactor cycle strictly:

1. **Red** - Write a failing test that describes the behavior you want
2. **Green** - Write the minimum code to make the test pass
3. **Refactor** - Clean up the code while keeping tests green

Never write production code without a failing test first. Never skip the refactor step.

## Writing Good Tests

- Test behavior, not implementation details
- Each test should verify one specific thing
- Use descriptive test names that read as specifications: `test_user_cannot_login_with_expired_token`
- Arrange-Act-Assert pattern in every test
- Tests should be independent - no shared mutable state between tests

## Test Granularity

- **Unit tests**: Test individual functions/methods in isolation. Mock external dependencies. Fast, many of these.
- **Integration tests**: Test components working together with real dependencies. Slower, fewer of these.
- **End-to-end tests**: Test full user flows. Slowest, fewest of these.

Aim for a testing pyramid: many unit tests, fewer integration tests, very few E2E tests.

## What to Test

- Happy path (expected inputs produce expected outputs)
- Edge cases (empty inputs, boundary values, nil/null)
- Error cases (invalid input, network failures, permission denied)
- State transitions (before/after side effects)

## What NOT to Test

- Framework internals or standard library functions
- Trivial getters/setters with no logic
- Private implementation details that may change
- Third-party library behavior (trust their tests)

## When Tests Fail

- Read the error message carefully before changing anything
- A failing test after a change means the change broke something - fix the code, not the test
- Only change a test if the *requirement* changed, not the implementation
