//go:build !linux

package main

import (
	"fmt"
	"syscall"
)

// sysMount is a stub for non-Linux platforms where syscall.Mount is unavailable.
func sysMount(source, target, fstype string) error {
	return fmt.Errorf("mount not supported on this platform")
}

// sysProcAttrForDeveloper is a no-op on non-Linux platforms.
// The agent-gateway only runs inside Linux VMs.
func sysProcAttrForDeveloper() *syscall.SysProcAttr {
	return nil
}

// sysProcAttrForRoot is a no-op on non-Linux platforms.
// The agent-gateway only runs inside Linux VMs.
func sysProcAttrForRoot() *syscall.SysProcAttr {
	return nil
}

// setNonDumpable is a no-op on non-Linux platforms.
func setNonDumpable() error {
	return nil
}
