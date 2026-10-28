#!/usr/bin/env python3
"""
Assert Docker Tier-2 netfilter/bridge/NAT options are enabled in libkrunfw configs.

Epic 8 (#143) T2 (#145) reproducibility check + T5 (#148) kernel validation.
The carry mechanism is the idempotent overlay script
(`enable-docker-netfilter.py`). This verifier proves the overlay actually took
effect and that the kernel's `olddefconfig` pass kept every required option.

Modes:
  --configs [DIR]   Check the five source config files (default: deps/libkrunfw).
  --built PATH      Check a resolved kernel `.config` produced by the build.

Exit status is non-zero when any required option is missing or built as a
module (=m) instead of built-in (=y); libkrunfw bundles a monolithic kernel.

Usage:
  python3 check-docker-netfilter-config.py --configs
  python3 check-docker-netfilter-config.py --built linux-6.12.76/.config
"""

import os
import sys

# Docker contrib/check-config.sh "Generally Necessary" set plus the Epic 8 §5
# netfilter/bridge/NAT additions. Every option must end up "=y" in a monolithic
# kernel; "=m" is treated as missing because modules are not loaded in the VM.
REQUIRED = [
    # Docker "Generally Necessary" core set.
    "CONFIG_NAMESPACES",
    "CONFIG_NET_NS",
    "CONFIG_PID_NS",
    "CONFIG_IPC_NS",
    "CONFIG_UTS_NS",
    "CONFIG_CGROUPS",
    "CONFIG_CGROUP_CPUACCT",
    "CONFIG_CGROUP_DEVICE",
    "CONFIG_CGROUP_FREEZER",
    "CONFIG_CGROUP_SCHED",
    "CONFIG_CPUSETS",
    "CONFIG_MEMCG",
    "CONFIG_KEYS",
    "CONFIG_VETH",
    "CONFIG_BRIDGE",
    "CONFIG_BRIDGE_NETFILTER",
    "CONFIG_IP_NF_FILTER",
    "CONFIG_IP_NF_MANGLE",
    "CONFIG_IP_NF_TARGET_MASQUERADE",
    "CONFIG_IP6_NF_FILTER",
    "CONFIG_IP6_NF_MANGLE",
    "CONFIG_IP6_NF_TARGET_MASQUERADE",
    "CONFIG_NETFILTER_XT_MATCH_ADDRTYPE",
    "CONFIG_NETFILTER_XT_MATCH_CONNTRACK",
    "CONFIG_NETFILTER_XT_MATCH_IPVS",
    "CONFIG_NETFILTER_XT_MARK",
    "CONFIG_IP_NF_RAW",
    "CONFIG_IP_NF_NAT",
    "CONFIG_NF_NAT",
    "CONFIG_IP6_NF_RAW",
    "CONFIG_IP6_NF_NAT",
    "CONFIG_POSIX_MQUEUE",
    "CONFIG_CGROUP_BPF",
    # Epic 8 §5 netfilter/bridge/NAT stack.
    "CONFIG_NETFILTER",
    "CONFIG_NETFILTER_ADVANCED",
    "CONFIG_NF_CONNTRACK",
    "CONFIG_NF_DEFRAG_IPV4",
    "CONFIG_NF_DEFRAG_IPV6",
    "CONFIG_NETFILTER_XTABLES",
    "CONFIG_IP_NF_IPTABLES",
    "CONFIG_IP_NF_TARGET_REDIRECT",
    "CONFIG_IP6_NF_IPTABLES",
    "CONFIG_NET_SCHED",
    "CONFIG_NET_CLS_CGROUP",
    "CONFIG_CGROUP_NET_CLASSID",
    "CONFIG_NF_TABLES",
    "CONFIG_NF_TABLES_INET",
    "CONFIG_NFT_NAT",
    "CONFIG_NFT_MASQ",
    "CONFIG_NFT_CT",
    # IPVS dependency chain for NETFILTER_XT_MATCH_IPVS.
    "CONFIG_IP_VS",
    "CONFIG_IP_VS_NFCT",
    "CONFIG_IP_VS_PROTO_TCP",
    "CONFIG_IP_VS_PROTO_UDP",
    "CONFIG_IP_VS_RR",
]

CONFIGS = [
    "config-libkrunfw_aarch64",
    "config-libkrunfw_x86_64",
    "config-libkrunfw_riscv64",
    "config-libkrunfw-sev_x86_64",
    "config-libkrunfw-tdx_x86_64",
]


def read_config(path):
    values = {}
    with open(path, "r", errors="replace") as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                # "# CONFIG_X is not set" means explicitly disabled.
                if line.startswith("# CONFIG_") and line.endswith(" is not set"):
                    name = line[len("# ") : -len(" is not set")]
                    values[name] = "n"
                continue
            if "=" in line:
                name, _, value = line.partition("=")
                values[name.strip()] = value.strip()
    return values


def check(path, label):
    if not os.path.exists(path):
        print(f"  MISSING FILE: {path}", file=sys.stderr)
        return ["<file missing>"]

    values = read_config(path)
    missing = []
    modules = []
    for opt in REQUIRED:
        value = values.get(opt)
        if value is None or value == "n":
            missing.append(opt)
        elif value == "m":
            modules.append(opt)

    name = os.path.basename(path)
    if missing or modules:
        print(f"  {label}/{name}: FAIL")
        if missing:
            print(f"    not enabled: {', '.join(missing)}")
        if modules:
            print(f"    built as module (=m): {', '.join(modules)}")
    else:
        print(f"  {label}/{name}: OK ({len(REQUIRED)} options =y)")
    return missing + modules


def main():
    args = sys.argv[1:]
    if not args:
        print(
            f"Usage: {sys.argv[0]} --configs [DIR] | --built <path/to/.config>",
            file=sys.stderr,
        )
        sys.exit(2)

    failures = []
    if args[0] == "--configs":
        base = args[1] if len(args) > 1 else os.path.normpath(
            os.path.join(
                os.path.dirname(os.path.abspath(__file__)), "..", "..", "deps", "libkrunfw"
            )
        )
        for config in CONFIGS:
            failures.extend(check(os.path.join(base, config), "configs"))
    elif args[0] == "--built":
        if len(args) < 2:
            print("--built requires a path to a resolved .config", file=sys.stderr)
            sys.exit(2)
        failures.extend(check(args[1], "built"))
    else:
        print(f"Unknown argument: {args[0]}", file=sys.stderr)
        sys.exit(2)

    if failures:
        print(
            f"\nFAILED: {len(failures)} required option(s) missing or built as modules.",
            file=sys.stderr,
        )
        sys.exit(1)
    print("\nAll Docker Tier-2 netfilter/bridge/NAT options are enabled (=y).")


if __name__ == "__main__":
    main()
