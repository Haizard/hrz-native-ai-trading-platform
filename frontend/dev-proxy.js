// Local dev proxy: serves frontend/app statically and forwards everything
// else -- API calls and WebSocket upgrades -- to the gateway on :8080, so the
// app's relative URLs work on one origin. No dependencies; WS is proxied by
// piping the upgraded socket both ways.
//
// Usage: node frontend/dev-proxy.js [port]   (default 3000)

const http = require("http");
const net = require("net");
const fs = require("fs");
const path = require("path");

const ROOT = path.join(__dirname, "app");
const BACKEND = { host: "127.0.0.1", port: 8080 };
const PORT = Number(process.argv[2]) || 3000;

const MIME = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".wasm": "application/wasm",
  ".json": "application/json",
  ".png": "image/png",
  ".svg": "image/svg+xml",
};

const server = http.createServer((req, res) => {
  const url = new URL(req.url, "http://x");
  let file = path.normalize(path.join(ROOT, url.pathname === "/" ? "index.html" : url.pathname));
  // Static files only from the app directory; anything else proxies.
  if (file.startsWith(ROOT) && fs.existsSync(file) && fs.statSync(file).isFile()) {
    res.writeHead(200, {
      "content-type": MIME[path.extname(file)] || "application/octet-stream",
      // Dev: heuristic caching of app.js once served a stale shell for a whole
      // debugging session. Never cache the code, ever.
      "cache-control": "no-store",
    });
    fs.createReadStream(file).pipe(res);
    return;
  }
  const proxy = http.request(
    { ...BACKEND, path: req.url, method: req.method, headers: { ...req.headers, host: `${BACKEND.host}:${BACKEND.port}` } },
    (upstream) => {
      res.writeHead(upstream.statusCode, upstream.headers);
      upstream.pipe(res);
    },
  );
  proxy.on("error", (err) => {
    res.writeHead(502);
    res.end(`gateway unreachable: ${err.message}`);
  });
  req.pipe(proxy);
});

server.on("upgrade", (req, socket, head) => {
  // Both sides die eventually -- idle timeouts, restarts, tab closes. An
  // unhandled socket error is a process exit, so every side gets a handler.
  socket.on("error", () => {});
  const upstream = net.connect(BACKEND.port, BACKEND.host, () => {
    const headers = Object.entries(req.headers)
      .map(([k, v]) => `${k}: ${v}`)
      .join("\r\n");
    upstream.write(`${req.method} ${req.url} HTTP/1.1\r\n${headers}\r\n\r\n`);
    if (head?.length) upstream.write(head);
    upstream.pipe(socket);
    socket.pipe(upstream);
  });
  upstream.on("error", () => socket.destroy());
});

// A piped request can also die mid-stream (client close, backend restart).
server.on("clientError", (_err, socket) => socket.destroy());

server.listen(PORT, () => {
  console.log(`dev proxy: http://127.0.0.1:${PORT} -> gateway at :${BACKEND.port}`);
});
