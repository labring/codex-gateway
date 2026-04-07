import { createServer } from "node:http";
import { promises as fs } from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";
import { CodexAppServerBridge } from "./codex-app-server.mjs";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const rootDir = path.resolve(__dirname, "..");
const publicDir = path.join(rootDir, "public");
const port = Number(process.env.PORT || 3000);

const bridge = new CodexAppServerBridge();
const clients = new Set();

function logLifecycle(message, extra) {
  const timestamp = new Date().toISOString();
  if (extra === undefined) {
    console.error(`[server ${timestamp}] ${message}`);
    return;
  }
  console.error(`[server ${timestamp}] ${message}`, extra);
}

function sendSse(response, event, data) {
  response.write(`event: ${event}\n`);
  response.write(`data: ${JSON.stringify(data)}\n\n`);
}

function broadcast(event, data) {
  for (const client of clients) {
    sendSse(client, event, data);
  }
}

function json(response, statusCode, payload) {
  response.writeHead(statusCode, {
    "Content-Type": "application/json; charset=utf-8",
    "Cache-Control": "no-store",
  });
  response.end(JSON.stringify(payload));
}

async function readBody(request) {
  const chunks = [];
  for await (const chunk of request) {
    chunks.push(chunk);
  }

  if (chunks.length === 0) {
    return {};
  }

  return JSON.parse(Buffer.concat(chunks).toString("utf8"));
}

async function serveStatic(response, filePath) {
  const extension = path.extname(filePath);
  const contentType =
    extension === ".html"
      ? "text/html; charset=utf-8"
      : extension === ".js"
        ? "text/javascript; charset=utf-8"
        : extension === ".css"
          ? "text/css; charset=utf-8"
          : "text/plain; charset=utf-8";

  const content = await fs.readFile(filePath);
  response.writeHead(200, {
    "Content-Type": contentType,
    "Cache-Control": "no-store",
  });
  response.end(content);
}

bridge.on("state", (state) => broadcast("state", state));
bridge.on("notification", (message) => broadcast("notification", message));
bridge.on("serverRequest", (message) => broadcast("server-request", message));
bridge.on("warning", (message) => {
  broadcast("warning", message);
  logLifecycle(`bridge warning: ${message.message}`, message.detail);
});
bridge.on("raw", (line) => broadcast("raw", { line }));

await bridge.start();

const keepAlive = setInterval(() => {
  for (const client of clients) {
    client.write(`: keepalive ${Date.now()}\n\n`);
  }
}, 15000);

const server = createServer(async (request, response) => {
  const url = new URL(request.url, `http://${request.headers.host}`);

  try {
    if (request.method === "GET" && url.pathname === "/") {
      await serveStatic(response, path.join(publicDir, "index.html"));
      return;
    }

    if (request.method === "GET" && url.pathname === "/app.js") {
      await serveStatic(response, path.join(publicDir, "app.js"));
      return;
    }

    if (request.method === "GET" && url.pathname === "/styles.css") {
      await serveStatic(response, path.join(publicDir, "styles.css"));
      return;
    }

    if (request.method === "GET" && url.pathname === "/api/state") {
      json(response, 200, bridge.getState());
      return;
    }

    if (request.method === "GET" && url.pathname === "/api/events") {
      response.writeHead(200, {
        "Content-Type": "text/event-stream; charset=utf-8",
        "Cache-Control": "no-store",
        Connection: "keep-alive",
      });
      response.write("retry: 1000\n\n");
      sendSse(response, "state", bridge.getState());
      clients.add(response);
      request.on("close", () => {
        clients.delete(response);
      });
      return;
    }

    if (request.method === "POST" && url.pathname === "/api/turn") {
      const body = await readBody(request);
      await bridge.sendPrompt(body.prompt);
      json(response, 202, { ok: true, state: bridge.getState() });
      return;
    }

    if (request.method === "POST" && url.pathname === "/api/thread/new") {
      const body = await readBody(request);
      await bridge.startNewThread({ model: body.model || undefined });
      json(response, 200, { ok: true, state: bridge.getState() });
      return;
    }

    json(response, 404, { error: "Not found" });
  } catch (error) {
    json(response, 500, {
      error: error.message,
      state: bridge.getState(),
    });
  }
});

server.listen(port, "127.0.0.1", () => {
  console.log(`Minimal Codex web UI listening at http://127.0.0.1:${port}`);
});

async function shutdown(exitCode) {
  clearInterval(keepAlive);
  for (const client of clients) {
    client.end();
  }
  server.close();
  await bridge.stop();
  process.exit(exitCode);
}

process.on("SIGINT", () => {
  shutdown(0);
});

process.on("SIGTERM", () => {
  shutdown(0);
});

process.on("exit", (code) => {
  logLifecycle(`node process exiting with code ${code}`);
});

process.on("uncaughtException", (error) => {
  logLifecycle("uncaughtException", error);
});

process.on("unhandledRejection", (reason) => {
  logLifecycle("unhandledRejection", reason);
});
