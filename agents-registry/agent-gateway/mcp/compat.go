package mcp

// packageAlt describes a known MCP server available on both PyPI (uvx) and npm (npx).
type packageAlt struct {
	UvxPkg  string   // uvx <pkg>
	UvxArgs []string // extra args after package
	NpxPkg  string   // npx -y <pkg>
	NpxArgs []string // extra args after package
}

// knownPackageAlternatives maps uvx ↔ npx equivalents for MCP servers that
// exist on BOTH registries (verified). Packages that only exist on one
// registry are NOT listed — those get excluded for incompatible agents.
var knownPackageAlternatives = []packageAlt{
	// Servers available on both PyPI and npm:
	{UvxPkg: "mcp-server-filesystem", NpxPkg: "@modelcontextprotocol/server-filesystem"},
	{UvxPkg: "mcp-server-github", NpxPkg: "@modelcontextprotocol/server-github"},
	{UvxPkg: "mcp-server-gitlab", NpxPkg: "@modelcontextprotocol/server-gitlab"},
	{UvxPkg: "mcp-server-postgres", NpxPkg: "@modelcontextprotocol/server-postgres"},
	{UvxPkg: "mcp-server-memory", NpxPkg: "@modelcontextprotocol/server-memory"},
}

// uvxOnlyPackages lists uvx packages that have NO npm equivalent.
// Agents that can't run uvx (codex, cursor) will have these excluded.
var uvxOnlyPackages = map[string]bool{
	"mcp-server-fetch":              true,
	"mcp-server-brave-search":       true,
	"mcp-server-puppeteer":          true,
	"mcp-server-sequential-thinking": true,
	"mcp-server-slack":              true,
}

// agentsRequiringNpx lists agents whose MCP clients work reliably with npx
// but fail with uvx (Python-based MCP servers).
var agentsRequiringNpx = map[string]bool{
	"codex":  true,
	"cursor": true,
}

// AutoPopulateOverrides inspects a server definition and adds per-agent
// overrides when the command uses a package manager that some agents
// don't support. Called automatically when a server is added.
//
// Three outcomes for uvx servers on codex/cursor:
//  1. Known npm equivalent exists → override with npx
//  2. Python-only (no npm pkg) → exclude from that agent
//  3. Unknown package → pass through unchanged (best effort)
func AutoPopulateOverrides(def *McpServerDef) {
	if def.Command != "uvx" || len(def.Args) == 0 {
		return
	}

	if def.Overrides == nil {
		def.Overrides = make(map[string]*McpServerOverride)
	}

	pkg := def.Args[0]
	extraArgs := def.Args[1:]

	// Check if there's an npx equivalent.
	if alt := findAlternativeByUvxPkg(pkg); alt != nil && alt.NpxPkg != "" {
		npxArgs := make([]string, 0, 2+len(alt.NpxArgs)+len(extraArgs))
		npxArgs = append(npxArgs, "-y", alt.NpxPkg)
		npxArgs = append(npxArgs, alt.NpxArgs...)
		npxArgs = append(npxArgs, extraArgs...)

		for agent := range agentsRequiringNpx {
			if _, exists := def.Overrides[agent]; !exists {
				def.Overrides[agent] = &McpServerOverride{
					Command: "npx",
					Args:    npxArgs,
				}
			}
		}
		return
	}

	// Python-only package — mark as excluded for npx-only agents.
	if uvxOnlyPackages[pkg] {
		for agent := range agentsRequiringNpx {
			if _, exists := def.Overrides[agent]; !exists {
				def.Overrides[agent] = &McpServerOverride{
					Command:  "",
					Excluded: true,
				}
			}
		}
	}
}

func findAlternativeByUvxPkg(pkg string) *packageAlt {
	for i := range knownPackageAlternatives {
		if knownPackageAlternatives[i].UvxPkg == pkg {
			return &knownPackageAlternatives[i]
		}
	}
	return nil
}

func findAlternativeByNpxPkg(pkg string) *packageAlt {
	for i := range knownPackageAlternatives {
		if knownPackageAlternatives[i].NpxPkg == pkg {
			return &knownPackageAlternatives[i]
		}
	}
	return nil
}
