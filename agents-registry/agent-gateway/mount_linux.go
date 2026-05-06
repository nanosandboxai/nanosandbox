//go:build linux

package main

import "syscall"

// sysMount wraps syscall.Mount (only available on Linux).
func sysMount(source, target, fstype string) error {
	return syscall.Mount(source, target, fstype, 0, "")
}

// sysProcAttrForDeveloper returns SysProcAttr that drops to the 'developer'
// user (UID 1000, GID 1000). Agents like Claude Code refuse
// --dangerously-skip-permissions when running as root.
func sysProcAttrForDeveloper() *syscall.SysProcAttr {
	return &syscall.SysProcAttr{
		Credential: &syscall.Credential{
			Uid: 1000,
			Gid: 1000,
		},
	}
}
