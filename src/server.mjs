import { createServer } from "node:http";
import { promises as fs } from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";
import { CodexAppServerBridge } from "./codex-app-server.mjs";
import { SessionManager, SessionManagerError } from "./session-manager.mjs";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const rootDir = path.resolve(__dirname, "..");
const publicDir = path.join(rootDir, "public");
const host = process.env.HOST || "0.0.0.0";
const port = Number(process.env.PORT || 3000);
const bridgeCwd = process.env.CODEX_CWD || rootDir;

const sessionManager = new SessionManager({
  createBridge: () =>
    new CodexAppServerBridge({
      cwd: bridgeCwd,
    }),
});
const sessionClients = new Map();

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

function getClients(sessionId) {
  let clients = sessionClients.get(sessionId);
  if (!clients) {
    clients = new Set();
    sessionClients.set(sessionId, clients);
  }
  return clients;
}

function broadcast(sessionId, event, data) {
  const clients = sessionClients.get(sessionId);
  if (!clients) {
    return;
  }

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

  try {
    return JSON.parse(Buffer.concat(chunks).toString("utf8"));
  } catch {
    throw new SessionManagerError(400, "Request body must be valid JSON");
  }
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

function matchRoute(pathname, pattern) {
  const match = pathname.match(pattern);
  if (!match) {
    return null;
  }

  return decodeURIComponent(match[1]);
}

function statusCodeForError(error) {
  if (error instanceof SessionManagerError) {
    return error.statusCode;
  }

  if (error.message === "Prompt must not be empty") {
    return 400;
  }

  if (error.message === "A turn is already in progress") {
    return 409;
  }

  return 500;
}

sessionManager.on("sessionCreated", (session) => {
  logLifecycle(`session created ${session.id}`);
});

sessionManager.on("sessionEvent", ({ sessionId, event, data }) => {
  broadcast(sessionId, event, data);

  if (event === "warning") {
    logLifecycle(`session ${sessionId} warning: ${data.message}`, data.detail);
  }
});

sessionManager.on("sessionClosed", ({ sessionId, reason }) => {
  const clients = sessionClients.get(sessionId);
  if (clients) {
    for (const client of clients) {
      sendSse(client, "session-closed", { sessionId, reason });
      client.end();
    }
    sessionClients.delete(sessionId);
  }

  logLifecycle(`session closed ${sessionId} (${reason})`);
});

const keepAlive = setInterval(() => {
  for (const clients of sessionClients.values()) {
    for (const client of clients) {
      client.write(`: keepalive ${Date.now()}\n\n`);
    }
  }
}, 15000);
keepAlive.unref?.();

const server = createServer(async (request, response) => {
  const url = new URL(request.url, `http://${request.headers.host || `${host}:${port}`}`);

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

    if (request.method === "GET" && url.pathname === "/healthz") {
      json(response, 200, {
        ok: true,
        uptimeSeconds: Math.round(process.uptime()),
      });
      return;
    }

    if (request.method === "GET" && url.pathname === "/readyz") {
      json(response, 200, {
        ok: true,
        activeSessions: sessionManager.count(),
      });
      return;
    }

    if (
      url.pathname === "/api/state" ||
      url.pathname === "/api/events" ||
      url.pathname === "/api/turn" ||
      url.pathname === "/api/thread/new"
    ) {
      json(response, 410, {
        error: "Legacy single-session endpoints were removed. Create a session first via POST /api/sessions.",
      });
      return;
    }

    if (request.method === "POST" && url.pathname === "/api/sessions") {
      const body = await readBody(request);
      const result = await sessionManager.createSession({
        model: typeof body.model === "string" && body.model.trim() ? body.model.trim() : undefined,
      });
      json(response, 201, { ok: true, ...result });
      return;
    }

    const sessionIdForState = matchRoute(url.pathname, /^\/api\/sessions\/([^/]+)\/state$/);
    if (request.method === "GET" && sessionIdForState) {
      json(response, 200, {
        ok: true,
        sessionId: sessionIdForState,
        session: sessionManager.getSessionInfo(sessionIdForState),
        state: sessionManager.getState(sessionIdForState),
      });
      return;
    }

    const sessionIdForEvents = matchRoute(url.pathname, /^\/api\/sessions\/([^/]+)\/events$/);
    if (request.method === "GET" && sessionIdForEvents) {
      const session = sessionManager.getSessionInfo(sessionIdForEvents);
      const state = sessionManager.getState(sessionIdForEvents);

      response.writeHead(200, {
        "Content-Type": "text/event-stream; charset=utf-8",
        "Cache-Control": "no-store",
        Connection: "keep-alive",
      });
      response.write("retry: 1000\n\n");
      sendSse(response, "session", session);
      sendSse(response, "state", state);
      getClients(sessionIdForEvents).add(response);
      request.on("close", () => {
        const clients = sessionClients.get(sessionIdForEvents);
        if (!clients) {
          return;
        }
        clients.delete(response);
        if (clients.size === 0) {
          sessionClients.delete(sessionIdForEvents);
        }
      });
      return;
    }

    const sessionIdForTurn = matchRoute(url.pathname, /^\/api\/sessions\/([^/]+)\/turn$/);
    if (request.method === "POST" && sessionIdForTurn) {
      const body = await readBody(request);

      if (typeof body.prompt !== "string" || !body.prompt.trim()) {
        throw new SessionManagerError(400, "Prompt must not be empty");
      }

      const state = await sessionManager.sendPrompt(sessionIdForTurn, body.prompt);
      json(response, 202, {
        ok: true,
        sessionId: sessionIdForTurn,
        session: sessionManager.getSessionInfo(sessionIdForTurn),
        state,
      });
      return;
    }

    const sessionIdForThread = matchRoute(url.pathname, /^\/api\/sessions\/([^/]+)\/thread\/new$/);
    if (request.method === "POST" && sessionIdForThread) {
      const body = await readBody(request);
      const state = await sessionManager.startNewThread(sessionIdForThread, {
        model: typeof body.model === "string" && body.model.trim() ? body.model.trim() : undefined,
      });
      json(response, 200, {
        ok: true,
        sessionId: sessionIdForThread,
        session: sessionManager.getSessionInfo(sessionIdForThread),
        state,
      });
      return;
    }

    const sessionIdForDelete = matchRoute(url.pathname, /^\/api\/sessions\/([^/]+)$/);
    if (request.method === "DELETE" && sessionIdForDelete) {
      const removed = await sessionManager.closeSession(sessionIdForDelete, {
        reason: "deleted",
      });

      if (!removed) {
        throw new SessionManagerError(404, `Unknown session: ${sessionIdForDelete}`);
      }

      json(response, 200, { ok: true, sessionId: sessionIdForDelete });
      return;
    }

    json(response, 404, { error: "Not found" });
  } catch (error) {
    json(response, statusCodeForError(error), {
      error: error.message,
    });
  }
});

server.listen(port, host, () => {
  console.log(`Codex gateway listening at http://${host}:${port}`);
});

async function shutdown(exitCode) {
  clearInterval(keepAlive);
  for (const clients of sessionClients.values()) {
    for (const client of clients) {
      client.end();
    }
  }
  sessionClients.clear();
  server.close();
  await sessionManager.shutdown();
  process.exit(exitCode);
}

process.on("SIGINT", () => {
  void shutdown(0);
});

process.on("SIGTERM", () => {
  void shutdown(0);
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
