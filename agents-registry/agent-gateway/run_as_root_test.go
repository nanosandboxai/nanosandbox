package main

import "testing"

func TestResolveRunAsRootFromEnv(t *testing.T) {
	t.Setenv(runAsRootEnv, "1")
	if !resolveRunAsRoot() {
		t.Fatal("resolveRunAsRoot() = false, want true for NANOSB_RUN_AS_ROOT=1")
	}
}

func TestResolveRunAsRootFalseFromEnv(t *testing.T) {
	t.Setenv(runAsRootEnv, "false")
	if resolveRunAsRoot() {
		t.Fatal("resolveRunAsRoot() = true, want false for NANOSB_RUN_AS_ROOT=false")
	}
}
