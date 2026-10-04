//go:build linux

package main

import (
	"syscall"
)

// Linux prctl(2) op codes (not exported by Go's syscall package).
const (
	prSetDumpable = 4
)

// setNonDumpable applies PR_SET_DUMPABLE=0 to the calling process. After
// this call ptrace(PTRACE_ATTACH, ...) and process_vm_readv against this
// process are rejected with EPERM, even by other root processes in the same
// PID namespace.
func setNonDumpable() error {
	_, _, errno := syscall.RawSyscall6(syscall.SYS_PRCTL, prSetDumpable, 0, 0, 0, 0, 0)
	if errno != 0 {
		return errno
	}
	return nil
}

// sysMount wraps syscall.Mount (only available on Linux).
func sysMount(source, target, fstype string) error {
	return syscall.Mount(source, target, fstype, 0, "")
}

// sysProcAttrForDeveloper returns SysProcAttr that drops to the 'developer'
// user (UID 1000, GID 1000). Agents like Claude Code refuse to run as root.
func sysProcAttrForDeveloper() *syscall.SysProcAttr {
	return &syscall.SysProcAttr{
		Credential: &syscall.Credential{
			Uid: 1000,
			Gid: 1000,
		},
	}
}

// sysProcAttrForRoot returns SysProcAttr that keeps uid 0 but isolates the
// spawned process from agent-gateway by placing it in a new PID namespace.
//
// CLONE_NEWPID makes the child believe it is PID 1 of its own namespace, and
// hides every process outside that namespace (including agent-gateway itself).
// As a result the child cannot kill(1, ...) the gateway, cannot ptrace it,
// and cannot read /proc/<gateway_pid>/* because those PIDs are no longer
// addressable. The VM boundary still isolates the sandbox from the host and
// from other sandboxes.
//
// We do NOT clear capabilities here: many root-user workflows depend on
// CAP_DAC_OVERRIDE, CAP_NET_*, etc. The PID namespace plus the read-only
// bind-mount of /usr/local/bin/agent-gateway (set up in nanosb-init.sh)
// are the layered mitigations.
func sysProcAttrForRoot() *syscall.SysProcAttr {
	return &syscall.SysProcAttr{
		Cloneflags: syscall.CLONE_NEWPID,
	}
}
