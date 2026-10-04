---
name: documentation
description: Technical documentation standards for code, APIs, and project docs
version: "1.0"
tags: [documentation, writing, standards]
when_to_use: Use when writing or updating technical documentation, READMEs, or API reference
allowed_tools: [Read, Write, Edit]
user_invocable: true
---

# Documentation

## Code Comments

- Write comments that explain *why*, not *what*. The code shows what; comments should explain intent, trade-offs, and non-obvious decisions.
- Don't comment obvious code. `i += 1  // increment i` adds noise.
- Keep comments up to date. A wrong comment is worse than no comment.
- Use TODO/FIXME/HACK markers with context: `TODO(username): reason and conditions for removal`.

## Function and API Documentation

- Document public interfaces: what a function does, its parameters, return values, and exceptions/errors it can raise.
- Include usage examples for non-trivial APIs.
- Document preconditions and postconditions when they're not obvious from types.
- Keep parameter descriptions concise - if you need a paragraph, the API may be too complex.

## Project Documentation

- Every project needs a README with: what it does, how to set it up, and how to use it.
- Keep setup instructions tested and current. Stale setup docs waste hours.
- Document architectural decisions that would surprise a new developer.
- Use diagrams for complex data flows or system architecture - a picture is worth a thousand words of prose.

## Changelog

- Track user-facing changes in a CHANGELOG following Keep a Changelog format.
- Categories: Added, Changed, Deprecated, Removed, Fixed, Security.
- Write entries for the audience (users/consumers), not for the development team.
