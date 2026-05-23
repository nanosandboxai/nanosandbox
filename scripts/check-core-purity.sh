#!/usr/bin/env bash
set -euo pipefail

CRATES=(
  "command-core"
  "pane-core"
  "upload-core"
  "terminal-core"
)

FORBIDDEN=(
  "ratatui"
  "crossterm"
  "tui-term"
  "tui_term"
  "arboard"
  "open"
)

echo "Checking core crate dependency purity..."

for crate in "${CRATES[@]}"; do
  echo "- Inspecting ${crate}"
  tree="$(cargo tree -p "${crate}")"
  for dep in "${FORBIDDEN[@]}"; do
    if [[ "${tree}" == *"${dep} "* ]] || [[ "${tree}" == *"${dep} v"* ]]; then
      echo "ERROR: forbidden dependency '${dep}' found in ${crate}" >&2
      exit 1
    fi
  done
done

echo "Core dependency purity check passed."
