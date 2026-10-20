"""Fails before the MCP handshake, like a server missing its API key."""
import sys

print("Error: GITHUB_TOKEN environment variable is required", file=sys.stderr, flush=True)
sys.exit(1)
