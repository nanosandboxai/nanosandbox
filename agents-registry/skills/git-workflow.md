---
name: git-workflow
description: Git workflow best practices with conventional commits and clean history
version: "1.0"
tags: [git, workflow, collaboration]
---

# Git Workflow

## Commit Messages

Follow conventional commits format for all commit messages:

- `feat:` new feature or capability
- `fix:` bug fix
- `refactor:` code change that neither fixes a bug nor adds a feature
- `docs:` documentation changes only
- `test:` adding or updating tests
- `chore:` maintenance tasks (dependencies, CI, tooling)
- `perf:` performance improvement

Keep the subject line under 72 characters. Use the body to explain *why*, not *what*.

Good: `feat: add retry logic for transient API failures`
Bad: `updated api.py`

## Branch Strategy

- Create feature branches from main: `feature/<short-description>`
- Use `fix/<short-description>` for bug fixes
- Keep branches short-lived (< 1 week)
- Rebase onto main before merging to maintain linear history
- Delete branches after merge

## Pull Requests

- Keep PRs small and focused (< 400 lines changed)
- One logical change per PR
- Write a clear description explaining what and why
- Link related issues with `Fixes #123` or `Relates to #123`
- Ensure CI passes before requesting review

## Working with Changes

- Stage specific files, not `git add .`
- Review your diff before committing: `git diff --staged`
- Never force-push to shared branches
- Use `git stash` for work-in-progress when switching context
- Amend only your most recent unpushed commit
