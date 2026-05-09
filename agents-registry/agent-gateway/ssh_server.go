package main

import (
	"crypto/ed25519"
	"crypto/x509"
	"encoding/pem"
	"fmt"
	"io"
	"log"
	"os"
	"os/exec"
	"strings"
	"sync"

	"github.com/creack/pty"
	gossh "golang.org/x/crypto/ssh"

	"github.com/gliderlabs/ssh"
)

// ---------------------------------------------------------------------------
// Secrets env store — populated via POST /api/v1/secrets/env
// ---------------------------------------------------------------------------

var (
	secretsEnv   = make(map[string]string)
	secretsEnvMu sync.RWMutex
)

// SetSecretsEnv replaces the secrets env store (called from HTTP endpoint).
func SetSecretsEnv(env map[string]string) {
	secretsEnvMu.Lock()
	defer secretsEnvMu.Unlock()
	secretsEnv = env
}

// getSecretsEnv returns a copy of the current secrets.
func getSecretsEnv() map[string]string {
	secretsEnvMu.RLock()
	defer secretsEnvMu.RUnlock()
	cp := make(map[string]string, len(secretsEnv))
	for k, v := range secretsEnv {
		cp[k] = v
	}
	return cp
}

// ---------------------------------------------------------------------------
// Environment helpers
// ---------------------------------------------------------------------------

// setEnvVar sets or replaces an env var in a slice.
func setEnvVar(env []string, key, value string) []string {
	prefix := key + "="
	for i, e := range env {
		if strings.HasPrefix(e, prefix) {
			env[i] = prefix + value
			return env
		}
	}
	return append(env, prefix+value)
}

// buildSessionEnv constructs the full environment for an SSH session.
func buildSessionEnv(s ssh.Session) []string {
	env := os.Environ()

	// Add secrets
	for k, v := range getSecretsEnv() {
		env = setEnvVar(env, k, v)
	}

	// Add client-sent env vars
	for _, e := range s.Environ() {
		env = append(env, e)
	}

	// Force developer user context
	env = setEnvVar(env, "HOME", "/home/developer")
	env = setEnvVar(env, "USER", "developer")
	env = setEnvVar(env, "LOGNAME", "developer")

	return env
}

// ---------------------------------------------------------------------------
// SSH session handler
// ---------------------------------------------------------------------------

func sshSessionHandler(s ssh.Session) {
	env := buildSessionEnv(s)

	ptyReq, winCh, isPty := s.Pty()

	if isPty {
		// Interactive session with PTY
		cmd := exec.Command("bash", "--login")
		cmd.Env = env
		cmd.Dir = "/home/developer"
		cmd.SysProcAttr = sysProcAttrForDeveloper()

		f, err := pty.Start(cmd)
		if err != nil {
			log.Printf("[ssh] pty start failed: %v", err)
			s.Exit(1)
			return
		}
		defer f.Close()

		// Set initial window size
		pty.Setsize(f, &pty.Winsize{
			Rows: uint16(ptyReq.Window.Height),
			Cols: uint16(ptyReq.Window.Width),
		})

		// Handle window resize
		go func() {
			for win := range winCh {
				pty.Setsize(f, &pty.Winsize{
					Rows: uint16(win.Height),
					Cols: uint16(win.Width),
				})
			}
		}()

		// Bidirectional copy
		go io.Copy(f, s) // stdin -> pty
		io.Copy(s, f)    // pty -> stdout

		cmd.Wait()

	} else if len(s.Command()) > 0 {
		// Exec session (non-interactive)
		args := s.Command()
		cmd := exec.Command(args[0], args[1:]...)
		cmd.Env = env
		cmd.Dir = "/home/developer"
		cmd.SysProcAttr = sysProcAttrForDeveloper()
		cmd.Stdin = s
		cmd.Stdout = s
		cmd.Stderr = s.Stderr()

		if err := cmd.Run(); err != nil {
			if exitErr, ok := err.(*exec.ExitError); ok {
				s.Exit(exitErr.ExitCode())
				return
			}
			log.Printf("[ssh] exec failed: %v", err)
			s.Exit(1)
			return
		}
		s.Exit(0)

	} else {
		// No command, no PTY — just exit
		fmt.Fprintln(s, "No command specified and no PTY requested")
		s.Exit(1)
	}
}

// ---------------------------------------------------------------------------
// Authorized key loading
// ---------------------------------------------------------------------------

// loadAuthorizedKeys reads authorized_keys files and returns the parsed keys.
func loadAuthorizedKeys() []ssh.PublicKey {
	var keys []ssh.PublicKey

	for _, path := range []string{
		"/home/developer/.ssh/authorized_keys",
		"/root/.ssh/authorized_keys",
	} {
		data, err := os.ReadFile(path)
		if err != nil {
			continue
		}
		for len(data) > 0 {
			key, _, _, rest, err := gossh.ParseAuthorizedKey(data)
			if err != nil {
				break
			}
			keys = append(keys, key)
			data = rest
		}
	}

	return keys
}

// ---------------------------------------------------------------------------
// Host key generation
// ---------------------------------------------------------------------------

const hostKeyPath = "/etc/nanosandbox/ssh_host_ed25519_key"

// generateHostKey loads an existing PEM host key or generates a new ed25519 one.
func generateHostKey() []byte {
	// Try to load existing key
	if data, err := os.ReadFile(hostKeyPath); err == nil {
		return data
	}

	// Generate new ed25519 key
	_, priv, err := ed25519.GenerateKey(nil)
	if err != nil {
		log.Fatalf("[ssh] failed to generate host key: %v", err)
	}

	privBytes, err := x509.MarshalPKCS8PrivateKey(priv)
	if err != nil {
		log.Fatalf("[ssh] failed to marshal host key: %v", err)
	}

	pemBlock := pem.EncodeToMemory(&pem.Block{
		Type:  "PRIVATE KEY",
		Bytes: privBytes,
	})

	// Try to persist for reuse across restarts
	if err := os.MkdirAll("/etc/nanosandbox", 0755); err == nil {
		_ = os.WriteFile(hostKeyPath, pemBlock, 0600)
	}

	return pemBlock
}

// ---------------------------------------------------------------------------
// Server startup
// ---------------------------------------------------------------------------

// startSSHServer starts the embedded SSH server on port 22.
func startSSHServer() error {
	authorizedKeys := loadAuthorizedKeys()

	server := &ssh.Server{
		Addr:    ":22",
		Handler: sshSessionHandler,
		PublicKeyHandler: func(ctx ssh.Context, key ssh.PublicKey) bool {
			if len(authorizedKeys) == 0 {
				log.Println("[ssh] WARNING: no authorized keys loaded, rejecting all")
				return false
			}
			for _, ak := range authorizedKeys {
				if ssh.KeysEqual(key, ak) {
					return true
				}
			}
			return false
		},
	}

	// Generate or load host key
	server.SetOption(ssh.HostKeyPEM(generateHostKey()))

	log.Printf("[ssh] starting SSH server on %s", server.Addr)
	return server.ListenAndServe()
}
