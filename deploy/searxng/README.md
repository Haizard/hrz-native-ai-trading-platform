# SearXNG for the agent's `search_web` tool

The agent searches the web through a private SearXNG instance. The platform
side is fully wired (`SEARXNG_BASE_URL` in `.env`); this folder is the
instance itself.

## Why this folder exists

The stock SearXNG image serves **HTML only** — `GET /search?format=json`
answers `403 Forbidden`, because the default `settings.yml` allows only the
`html` format. The gateway's client uses the JSON API, so the instance needs
one setting changed. Since Koyeb cannot mount config files into a Docker Hub
image, the fix is this tiny image: stock SearXNG plus the settings file.

## Redeploy on Koyeb

1. Push this repo (or just this folder) to GitHub.
2. In the Koyeb dashboard, edit the SearXNG service:
   - Change the source from **Docker Hub** (`searxng/searxng`) to **GitHub**,
     pick the repository, and set the build context / Dockerfile path to
     `deploy/searxng/`.
   - Keep the same public port (SearXNG listens on **8080** inside the
     container) and the same domain.
3. Redeploy. The URL stays
   `https://thick-sephira-haizard-de9fd6cd.koyeb.app`, so nothing in the
   platform's `.env` changes.

## Verify

```sh
curl -s "https://thick-sephira-haizard-de9fd6cd.koyeb.app/search?q=btc&format=json" | head -c 200
```

A JSON body (`{"query": "btc", "results": [...]}`) means the tool is live. A
`403` page means the old image is still serving.

## What changed from stock

- `search.formats`: added `json` (kept `html` so the browser UI still works).
- `server.limiter: false`: all searches come from one client — the api
  gateway — and per-IP limiting would throttle the agent, not protect anyone.
  The instance is private; do not re-enable the limiter without also
  allowlisting the gateway's egress IP.
- `server.secret_key`: generated; signs UI cookies only.
