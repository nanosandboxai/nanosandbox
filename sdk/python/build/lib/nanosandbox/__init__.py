"""Python SDK for the nanosandbox protocol helper.

Talks the versioned NDJSON control protocol (nanosb-protocol v1.0.0) over a
Unix stream socket. Stdlib only.

    from nanosandbox import SandboxClient, SandboxBuilder

    client = SandboxClient.connect("/path/to/helper.sock")
    box = client.create_with(SandboxBuilder("library/alpine:latest").name("demo"))
    print(box.exec("echo", ["hi"]).stdout)
"""

from .client import (
    NanosbError,
    Sandbox,
    SandboxBuilder,
    SandboxClient,
    VolumeBuilder,
    PROTOCOL_VERSION,
)
from .runtime import ManagedRuntime, find_runtime_bin

__all__ = [
    "NanosbError",
    "Sandbox",
    "SandboxBuilder",
    "SandboxClient",
    "VolumeBuilder",
    "ManagedRuntime",
    "find_runtime_bin",
    "PROTOCOL_VERSION",
]
