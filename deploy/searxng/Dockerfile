# syntax=docker/dockerfile:1

# The trading platform's private search backend: stock SearXNG with one
# settings change -- the JSON API on. Stock SearXNG serves HTML only, which
# is why the agent's search_web tool answered 403 until this image replaced
# the plain one.
#
# Self-contained on purpose: the settings are written into the image at
# build time, so the build does not depend on which directory Koyeb picks
# as the build context. The Dockerfile location is the only setting the
# service needs.
FROM docker.io/searxng/searxng:latest

# The write needs root: the base image's default user cannot write /etc.
USER root

# use_default_settings keeps every engine and stock default; the file only
# overrides three things:
#
# 1. `search.formats` gains `json` -- the whole reason this file exists. The
#    stock default is `[html]`, and the api gateway's search_web tool calls
#    `GET /search?format=json`, which a stock instance answers 403.
# 2. `server.limiter: false` -- every search arrives from ONE client (the api
#    gateway's IP), so per-IP rate limiting would throttle the agent exactly
#    when it is working hardest. The instance is private; nothing else talks
#    to it.
# 3. `secret_key` is generated (`openssl rand -hex 32`), not the shipped
#    placeholder. It signs UI cookies, nothing else.
RUN <<EOF cat > /etc/searxng/settings.yml
use_default_settings: true

general:
  debug: false
  instance_name: "Haizard Search"

server:
  secret_key: "6e08c657828ec95f17b18c4f6978bd1df7d98e515a71b2f66615eb0ab8f2ac28"
  limiter: false
  image_proxy: true

search:
  formats:
    - html
    - json

ui:
  static_use_hash: true
EOF
