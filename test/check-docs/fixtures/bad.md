# Docs accuracy gate — negative fixture

This fixture exercises the docs-accuracy gate (scripts/check-docs.sh and the
scripts/docs/ helpers). It intentionally contains two defects that the gate
MUST catch:

1. a `nanosb` invocation for a subcommand that does not exist in
   `scripts/docs/cli-surface.json`, and
2. a relative markdown link whose target does not exist.

If the gate exits 0 on this directory, the gate is broken.

## Bogus subcommand

The following fenced block references `nanosb frobnicate`, which is not a real
`nanosb` subcommand:

```sh
$ nanosb frobnicate --bogus-flag
```

## Dead link

The link below points to a file that does not exist:

See [missing document](./does-not-exist.md).
