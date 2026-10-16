# The trading platform's private search backend: stock SearXNG plus one
# settings file that turns the JSON API on. Stock SearXNG serves HTML only,
# which is why the agent's search_web tool answered 403 until this image
# replaced the plain one.
FROM docker.io/searxng/searxng:latest

COPY settings.yml /etc/searxng/settings.yml
