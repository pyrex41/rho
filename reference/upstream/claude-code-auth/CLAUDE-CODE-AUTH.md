# Claude Code Authentication — How It Works

This directory contains all the files needed to understand (and reimplement) how the coding agent authenticates using a Claude Pro/Max subscription via Claude Code's OAuth flow.

## The Big Picture

Users with a Claude Pro/Max subscription can use their subscription to call Claude models through this agent — no separate Anthropic API key needed. The agent impersonates Claude Code by:

1. Running an OAuth PKCE flow against `claude.ai`
2. Detecting the resulting token by its `sk-ant-oat` prefix
3. Sending requests with Claude Code identity headers so Anthropic's API accepts the subscription token

## Files In This Directory

| File | What It Does |
|------|-------------|
| `oauth-flow.ts` | The Anthropic OAuth PKCE flow — login + token refresh |
| `oauth-types.ts` | `OAuthCredentials`, `OAuthProviderInterface` types |
| `oauth-registry.ts` | Provider registry (Anthropic, GitHub Copilot, Google, OpenAI) |
| `pkce.ts` | PKCE challenge/verifier generation using Web Crypto |
| `auth-storage.ts` | `AuthStorage` class — credential persistence, refresh with file locking |
| `env-api-keys.ts` | Environment variable → provider mapping (`ANTHROPIC_OAUTH_TOKEN` precedence) |

The OAuth detection and Claude Code identity injection lives in `../anthropic-provider.ts` (already in the reference folder). The key function is `createClient()` at line 486.

## Authentication Flow

```
┌─────────────────────────────────────────────────────────────┐
│ 1. LOGIN (one-time, via /login command)                     │
│                                                             │
│    Generate PKCE verifier + challenge (pkce.ts)             │
│    → Open browser to claude.ai/oauth/authorize              │
│    → User authenticates with their Claude account           │
│    → Get authorization code                                 │
│    → Exchange code at console.anthropic.com/v1/oauth/token  │
│    → Receive: access_token + refresh_token + expires_in     │
│    → Save to ~/.pi/agent/auth.json as { type: "oauth" }    │
└─────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────┐
│ 2. TOKEN RESOLUTION (every API call)                        │
│                                                             │
│    AuthStorage.getApiKey("anthropic"):                      │
│      1. Runtime override (--api-key flag)                   │
│      2. auth.json API key  { type: "api_key" }             │
│      3. auth.json OAuth    { type: "oauth" }               │
│         → Check expiry, refresh if needed (with file lock)  │
│         → Return access_token                               │
│      4. $ANTHROPIC_OAUTH_TOKEN env var                      │
│      5. $ANTHROPIC_API_KEY env var                          │
│      6. Fallback resolver (models.json custom providers)    │
└─────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────┐
│ 3. API CALL (anthropic-provider.ts createClient())          │
│                                                             │
│    Detect token type: apiKey.includes("sk-ant-oat")         │
│                                                             │
│    IF OAuth token (sk-ant-oat*):                            │
│      → Authorization: Bearer <token>  (not x-api-key)      │
│      → anthropic-beta: claude-code-20250219,oauth-2025-04-20│
│      → user-agent: claude-cli/2.1.2 (external, cli)        │
│      → x-app: cli                                          │
│      → System prompt prepend:                               │
│        "You are Claude Code, Anthropic's official CLI."     │
│      → Tool names renamed to Claude Code casing             │
│        (bash→Bash, read→Read, grep→Grep, etc.)             │
│                                                             │
│    IF API key (sk-ant-api*):                                │
│      → x-api-key: <key>  (standard header)                 │
│      → No identity headers, no tool renaming                │
└─────────────────────────────────────────────────────────────┘
```

## Key Constants

```
CLIENT_ID:     base64("OWQxYzI1MGEtZTYxYi00NGQ5LTg4ZWQtNTk0NGQxOTYyZjVl")
               → "9d1c250a-e61b-44d9-88ed-5944d1962f5e"

AUTHORIZE_URL: https://claude.ai/oauth/authorize
TOKEN_URL:     https://console.anthropic.com/v1/oauth/token
REDIRECT_URI:  https://console.anthropic.com/oauth/code/callback
SCOPES:        org:create_api_key user:profile user:inference

OAUTH_PREFIX:  "sk-ant-oat"  (how to detect OAuth vs API key tokens)

CLAUDE_CODE_VERSION: "2.1.2"
USER_AGENT:    "claude-cli/2.1.2 (external, cli)"
BETA_FLAGS:    "claude-code-20250219,oauth-2025-04-20"
SYSTEM_PROMPT: "You are Claude Code, Anthropic's official CLI for Claude."
```

## Token Format

```
OAuth access token: sk-ant-oat-... (Bearer auth)
API key:            sk-ant-api-... (x-api-key header)
```

The `sk-ant-oat` prefix is the sole mechanism for distinguishing authentication modes. Any token with this prefix triggers Claude Code identity injection.

## Token Refresh

OAuth tokens expire. The refresh flow:

1. Check `expires` timestamp (set to `expires_in - 5 minutes` for buffer)
2. If expired, acquire file lock on `auth.json` (prevents race with other agent instances)
3. Re-read file (another instance may have already refreshed)
4. If still expired, POST to TOKEN_URL with `grant_type: refresh_token`
5. Save new `access_token` + `refresh_token` + `expires`
6. Release lock

File locking uses `proper-lockfile` with retry (10 retries, exponential backoff, 30s stale timeout).

## For the Rust Port

Minimal implementation needs:

1. **PKCE generation** — `rand` + `sha2` + base64url encoding (~20 lines)
2. **OAuth flow** — HTTP POST to token endpoint with PKCE verifier (~40 lines)
3. **Token storage** — JSON file read/write with `flock` (~50 lines)
4. **Token detection** — `token.contains("sk-ant-oat")` (1 line)
5. **Header injection** — Set Bearer auth + Claude Code headers when OAuth detected (~15 lines)
6. **Token refresh** — Check expiry, POST refresh_token, update file (~30 lines)

Total: ~160 lines of Rust for the complete auth flow.
