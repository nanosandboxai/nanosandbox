package main

import "testing"

func TestResolveSessionUserDeveloper(t *testing.T) {
	orig := runAsRoot
	runAsRoot = false
	t.Cleanup(func() { runAsRoot = orig })

	su := resolveSessionUser()
	if su.user != "developer" {
		t.Fatalf("user=%q, want developer", su.user)
	}
	if su.home != "/home/developer" {
		t.Fatalf("home=%q, want /home/developer", su.home)
	}
	if su.procAttr == nil {
		t.Fatal("procAttr is nil")
	}
}

func TestResolveSessionUserRoot(t *testing.T) {
	orig := runAsRoot
	runAsRoot = true
	t.Cleanup(func() { runAsRoot = orig })

	su := resolveSessionUser()
	if su.user != "root" {
		t.Fatalf("user=%q, want root", su.user)
	}
	if su.home != "/root" {
		t.Fatalf("home=%q, want /root", su.home)
	}
	if su.procAttr == nil {
		t.Fatal("procAttr is nil")
	}
}

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
