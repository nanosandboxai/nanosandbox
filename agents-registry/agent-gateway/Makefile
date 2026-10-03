# agent-gateway standalone Makefile
#
# Build the Go HTTP server that runs as PID 1 inside nanosandbox VMs.

BINARY    = agent-gateway
GO        = go
GOFLAGS   = -ldflags="-s -w"
CGO       = 0

.PHONY: build build-static test clean fmt vet

build:
	$(GO) build $(GOFLAGS) -o $(BINARY) .

build-static:
	CGO_ENABLED=$(CGO) GOOS=linux $(GO) build $(GOFLAGS) -o $(BINARY) .

test:
	$(GO) test -v ./...

fmt:
	$(GO) fmt ./...

vet:
	$(GO) vet ./...

clean:
	rm -f $(BINARY)
