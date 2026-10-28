#!/usr/bin/env python3
"""
Enable Docker Tier-2 netfilter/bridge/NAT kernel options in libkrunfw configs.

Epic 8 (#143) T1 (#144): Docker's default bridge networking requires the
netfilter/bridge/NAT stack. The upstream libkrunfw configs disable it.

This script is idempotent: it replaces "not set" lines with "=y", flips "=m"
to "=y" (libkrunfw bundles a monolithic kernel; modules are not loaded), and
appends options that are absent entirely. Transitive dependencies are resolved
by the kernel's `olddefconfig` during the build.

Usage:
  python3 enable-docker-netfilter.py <config-file> [<config-file> ...]
  python3 enable-docker-netfilter.py --all   # apply to all 5 known configs
"""

import sys
import os

# Epic 8 section 5: exact options to enable.
OPTIONS = [
    "CONFIG_BRIDGE",
    "CONFIG_BRIDGE_NETFILTER",
    "CONFIG_NETFILTER",
    "CONFIG_NETFILTER_ADVANCED",
    "CONFIG_NF_CONNTRACK",
    "CONFIG_NF_DEFRAG_IPV4",
    "CONFIG_NF_DEFRAG_IPV6",
    "CONFIG_NETFILTER_XTABLES",
    "CONFIG_NETFILTER_XT_MARK",
    "CONFIG_NETFILTER_XT_MATCH_ADDRTYPE",
    "CONFIG_NETFILTER_XT_MATCH_CONNTRACK",
    "CONFIG_NETFILTER_XT_MATCH_IPVS",
    # IPVS is the dependency chain for NETFILTER_XT_MATCH_IPVS; without it
    # olddefconfig silently drops the match module (Epic 8 T1).
    "CONFIG_IP_VS",
    "CONFIG_IP_VS_NFCT",
    "CONFIG_IP_VS_PROTO_TCP",
    "CONFIG_IP_VS_PROTO_UDP",
    "CONFIG_IP_VS_RR",
    "CONFIG_IP_NF_IPTABLES",
    "CONFIG_IP_NF_FILTER",
    "CONFIG_IP_NF_MANGLE",
    "CONFIG_IP_NF_RAW",
    "CONFIG_IP_NF_NAT",
    "CONFIG_IP_NF_TARGET_MASQUERADE",
    "CONFIG_IP_NF_TARGET_REDIRECT",
    "CONFIG_NF_NAT",
    "CONFIG_IP6_NF_IPTABLES",
    "CONFIG_IP6_NF_FILTER",
    "CONFIG_IP6_NF_MANGLE",
    "CONFIG_IP6_NF_RAW",
    "CONFIG_IP6_NF_NAT",
    "CONFIG_IP6_NF_TARGET_MASQUERADE",
    "CONFIG_NET_SCHED",
    "CONFIG_NET_CLS_CGROUP",
    "CONFIG_CGROUP_NET_CLASSID",
    "CONFIG_NF_TABLES",
    "CONFIG_NF_TABLES_INET",  # selects NF_TABLES_IPV4/IPV6, which NFT_NAT requires
    "CONFIG_NFT_NAT",
    "CONFIG_NFT_MASQ",
    "CONFIG_NFT_CT",
    # Docker check-config "Generally Necessary"; present in most variants but
    # disabled in the x86_64 base config (Epic 8 T1).
    "CONFIG_POSIX_MQUEUE",
    "CONFIG_POSIX_MQUEUE_SYSCTL",
]

CONFIGS = [
    "config-libkrunfw_aarch64",
    "config-libkrunfw_x86_64",
    "config-libkrunfw_riscv64",
    "config-libkrunfw-sev_x86_64",
    "config-libkrunfw-tdx_x86_64",
]


def apply_config(path):
    with open(path, "r") as f:
        lines = f.readlines()

    changed = 0
    seen = set()

    for i, line in enumerate(lines):
        stripped = line.strip()
        for opt in OPTIONS:
            if stripped == f"# {opt} is not set":
                lines[i] = f"{opt}=y\n"
                changed += 1
                seen.add(opt)
                break
            if stripped.startswith(f"{opt}="):
                seen.add(opt)
                # Flip module to built-in; keep =y as-is.
                if stripped != f"{opt}=y":
                    lines[i] = f"{opt}=y\n"
                    changed += 1
                break

    # Append options that were absent entirely.
    absent = [opt for opt in OPTIONS if opt not in seen]
    if absent:
        lines.append("\n# Epic 8 (#143) T1: Docker Tier-2 netfilter/bridge/NAT\n")
        for opt in absent:
            lines.append(f"{opt}=y\n")
        changed += len(absent)

    with open(path, "w") as f:
        f.writelines(lines)

    return changed, absent


def main():
    args = sys.argv[1:]
    if not args:
        print(f"Usage: {sys.argv[0]} <config-file> [...] | --all", file=sys.stderr)
        sys.exit(1)

    if args == ["--all"]:
        base = os.path.normpath(
            os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "deps", "libkrunfw")
        )
        targets = [os.path.join(base, c) for c in CONFIGS]
    else:
        targets = args

    failed = []
    for path in targets:
        if not os.path.exists(path):
            print(f"  SKIP (missing): {path}", file=sys.stderr)
            failed.append(path)
            continue
        changed, absent = apply_config(path)
        name = os.path.basename(path)
        print(f"  {name}: {changed} change(s), {len(absent)} appended")
        if absent:
            print(f"    appended: {', '.join(absent)}")

    if failed:
        sys.exit(1)
    print("Done. Run `make olddefconfig` (via the libkrunfw Makefile) to resolve deps.")


if __name__ == "__main__":
    main()
