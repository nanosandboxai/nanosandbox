---
name: code-review
description: Code review practices focused on correctness, clarity, and maintainability
version: "1.0"
tags: [review, quality, collaboration]
---

# Code Review

## What to Look For

Review code in this priority order:

1. **Correctness** - Does the code do what it claims? Are there logic errors, off-by-ones, race conditions, or unhandled edge cases?
2. **Security** - Are there injection vulnerabilities, exposed secrets, missing input validation, or broken auth checks?
3. **Clarity** - Can you understand the code without the author explaining it? Are names descriptive? Is the flow obvious?
4. **Simplicity** - Is there a simpler way to achieve the same result? Is anything over-engineered for hypothetical future needs?
5. **Tests** - Are the changes tested? Do the tests cover meaningful scenarios, not just happy paths?

## Giving Feedback

- Be specific: point to the exact line and explain the concern
- Explain *why* something is a problem, not just that it is
- Suggest alternatives when possible
- Distinguish between blocking issues and suggestions (prefix with "nit:" for minor style points)
- Acknowledge good patterns when you see them

## Receiving Feedback

- Feedback is about the code, not about you
- If you disagree, explain your reasoning - don't just dismiss
- If you don't understand feedback, ask for clarification
- Apply feedback consistently - if you fix it in one place, fix similar issues elsewhere

## Self-Review Checklist

Before submitting code for review, check:

- [ ] I've read my own diff line by line
- [ ] I've removed debug logging and commented-out code
- [ ] I've run the tests locally and they pass
- [ ] I haven't introduced any hardcoded secrets or credentials
- [ ] Error messages are clear and actionable
- [ ] New public APIs have clear interfaces
