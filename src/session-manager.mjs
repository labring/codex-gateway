import { randomUUID } from "node:crypto";
import { EventEmitter } from "node:events";
import { CodexAppServerBridge } from "./codex-app-server.mjs";

function positiveInteger(value, fallback) {
  const parsed = Number(value);
  if (!Number.isFinite(parsed) || parsed <= 0) {
    return fallback;
  }
  return Math.floor(parsed);
}

export class SessionManagerError extends Error {
  constructor(statusCode, message) {
    super(message);
    this.name = "SessionManagerError";
    this.statusCode = statusCode;
  }
}

export class SessionManager extends EventEmitter {
  constructor({
    createBridge = () => new CodexAppServerBridge(),
    sessionTtlMs = positiveInteger(process.env.SESSION_TTL_MS, 30 * 60 * 1000),
    maxSessions = positiveInteger(process.env.MAX_SESSIONS, 12),
    sweepIntervalMs = positiveInteger(process.env.SESSION_SWEEP_INTERVAL_MS, 60 * 1000),
  } = {}) {
    super();
    this.createBridge = createBridge;
    this.sessionTtlMs = sessionTtlMs;
    this.maxSessions = maxSessions;
    this.sweepIntervalMs = sweepIntervalMs;
    this.sessions = new Map();

    this.sweepTimer = setInterval(() => {
      this.sweepExpiredSessions();
    }, this.sweepIntervalMs);
    this.sweepTimer.unref?.();
  }

  async createSession({ model } = {}) {
    if (this.sessions.size >= this.maxSessions) {
      this.sweepExpiredSessions();
    }

    if (this.sessions.size >= this.maxSessions) {
      throw new SessionManagerError(
        503,
        `Maximum concurrent sessions reached (${this.maxSessions})`,
      );
    }

    const id = randomUUID();
    const bridge = this.createBridge();
    const now = Date.now();
    const session = {
      id,
      bridge,
      createdAt: now,
      lastAccessAt: now,
      expiresAt: now + this.sessionTtlMs,
      listeners: null,
    };

    this.attachBridgeListeners(session);

    try {
      await bridge.start();

      if (model && model !== bridge.getState().selectedModel) {
        await bridge.startNewThread({ model });
      }
    } catch (error) {
      this.detachBridgeListeners(session);
      await bridge.stop().catch(() => {});
      throw error;
    }

    this.sessions.set(id, session);
    this.emit("sessionCreated", this.describeSession(session));

    return {
      sessionId: id,
      session: this.describeSession(session),
      state: bridge.getState(),
    };
  }

  getState(sessionId) {
    return this.requireSession(sessionId).bridge.getState();
  }

  getSessionInfo(sessionId) {
    return this.describeSession(this.requireSession(sessionId));
  }

  async sendPrompt(sessionId, prompt) {
    const session = this.requireSession(sessionId);
    await session.bridge.sendPrompt(prompt);
    return session.bridge.getState();
  }

  async startNewThread(sessionId, { model } = {}) {
    const session = this.requireSession(sessionId);
    await session.bridge.startNewThread({ model });
    return session.bridge.getState();
  }

  async closeSession(sessionId, { reason = "closed" } = {}) {
    const session = this.sessions.get(sessionId);
    if (!session) {
      return false;
    }

    this.sessions.delete(sessionId);
    this.detachBridgeListeners(session);

    const summary = this.describeSession(session);

    try {
      await session.bridge.stop();
    } finally {
      this.emit("sessionClosed", {
        sessionId,
        reason,
        session: summary,
      });
    }

    return true;
  }

  count() {
    return this.sessions.size;
  }

  async shutdown() {
    clearInterval(this.sweepTimer);

    await Promise.allSettled(
      [...this.sessions.keys()].map((sessionId) =>
        this.closeSession(sessionId, { reason: "shutdown" }),
      ),
    );
  }

  requireSession(sessionId) {
    const session = this.sessions.get(sessionId);

    if (!session) {
      throw new SessionManagerError(404, `Unknown session: ${sessionId}`);
    }

    this.touchSession(session);
    return session;
  }

  touchSession(session) {
    const now = Date.now();
    session.lastAccessAt = now;
    session.expiresAt = now + this.sessionTtlMs;
  }

  sweepExpiredSessions() {
    const now = Date.now();

    for (const session of this.sessions.values()) {
      if (session.expiresAt > now) {
        continue;
      }

      void this.closeSession(session.id, { reason: "expired" });
    }
  }

  describeSession(session) {
    return {
      id: session.id,
      createdAt: new Date(session.createdAt).toISOString(),
      lastAccessAt: new Date(session.lastAccessAt).toISOString(),
      expiresAt: new Date(session.expiresAt).toISOString(),
    };
  }

  attachBridgeListeners(session) {
    const makeForwarder = (event) => (data) => {
      this.touchSession(session);
      this.emit("sessionEvent", {
        sessionId: session.id,
        event,
        data,
      });
    };

    session.listeners = {
      state: makeForwarder("state"),
      notification: makeForwarder("notification"),
      serverRequest: makeForwarder("server-request"),
      warning: makeForwarder("warning"),
      raw: makeForwarder("raw"),
    };

    session.bridge.on("state", session.listeners.state);
    session.bridge.on("notification", session.listeners.notification);
    session.bridge.on("serverRequest", session.listeners.serverRequest);
    session.bridge.on("warning", session.listeners.warning);
    session.bridge.on("raw", session.listeners.raw);
  }

  detachBridgeListeners(session) {
    if (!session.listeners) {
      return;
    }

    session.bridge.off("state", session.listeners.state);
    session.bridge.off("notification", session.listeners.notification);
    session.bridge.off("serverRequest", session.listeners.serverRequest);
    session.bridge.off("warning", session.listeners.warning);
    session.bridge.off("raw", session.listeners.raw);
    session.listeners = null;
  }
}
