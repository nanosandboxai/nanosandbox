#!/usr/bin/env python3
"""Thin CI entrypoint for the models registry sync.

Equivalent to ``python3 scripts/models/sync.py`` (see that module for details).
The scheduled workflow and maintainers call this file.
"""

from __future__ import annotations

import sys

try:
    from . import sync
except ImportError:  # running as a plain script
    import sync


if __name__ == "__main__":
    sys.exit(sync.main(sys.argv[1:]))
