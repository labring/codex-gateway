import { spawn } from "node:child_process";
import { EventEmitter } from "node:events";
import readline from "node:readline";
import process from "node:process";
import { getCodexAppServerConfigArgs } from "./codex-runtime.mjs";
import { buildCodexChildEnv, ENV_NAMES, readBooleanFlag, readEnv } from "./env-config.mjs";

const MAX_EVENTS = 120;
const MAX_TRANSCRIPT = 100;

function preview(value, limit = 120) {
  if (typeof value !== "string") {
    return null;
  }

  const compact = value.replace(/\s+/g, " ").trim();
  if (!compact) {
    return null;
  }

  return compact.length > limit ? `${compact.slice(0, limit - 1)}…` : compact;
}

function autoApprovalDecision(params) {
  return Array.isArray(params?.availableDecisions) &&
    params.availableDecisions.includes("acceptForSession")
    ? "acceptForSession"
    : "accept";
}

function clone(value) {
  return JSON.parse(JSON.stringify(value));
}

export class CodexAppServerBridge extends EventEmitter {
  constructor({
    cwd = process.cwd(),
    codexBin = readEnv(ENV_NAMES.codexBin) || "codex",
    debug = readBooleanFlag(ENV_NAMES.debug),
    clientInfo = {
      name: "codex_gateway_web",
      title: "Codex Gateway Web",
      version: "0.2.0",
    },
  } = {}) {
    super();
    this.cwd = cwd;
    this.codexBin = codexBin;
    this.debug = debug;
    this.clientInfo = clientInfo;

    this.child = null;
    this.rl = null;
    this.nextId = 1;
    this.pending = new Map();

    this.ready = false;
    this.closed = false;
    this.startTimestamp = null;

    this.runtime = { platformFamily: null, platformOs: null, userAgent: null };
    this.account = null;
    this.requiresOpenaiAuth = null;
    this.models = [];
    this.selectedModel = null;

    this.threadId = null;
    this.threadStatus = null;
    this.currentTurnId = null;
    this.activeTurn = false;
    this.lastTurnStatus = null;

    this.transcript = [];
    this.transcriptIndex = new Map();
    this.recentEvents = [];
    this.localCounter = 0;
  }

  async start() {
    if (this.ready) {
      return this.getState();
    }

    this.startTimestamp = new Date().toISOString();

    this.child = spawn(this.codexBin, ["app-server", ...getCodexAppServerConfigArgs()], {
      cwd: this.cwd,
      env: buildCodexChildEnv(),
      stdio: ["pipe", "pipe", "inherit"],
    });

    this.rl = readline.createInterface({ input: this.child.stdout });
    this.installProcessHandlers();

    const initializeResult = await this.request("initialize", {
      clientInfo: this.clientInfo,
    });

    this.send({ method: "initialized", params: {} });

    this.runtime = {
      platformFamily: initializeResult.platformFamily ?? null,
      platformOs: initializeResult.platformOs ?? null,
      userAgent: initializeResult.userAgent ?? null,
    };

    await this.refreshAccount();
    await this.refreshModels();
    await this.startNewThread();

    this.ready = true;
    this.emitState();
    return this.getState();
  }

  installProcessHandlers() {
    this.child.on("error", (error) => {
      this.emitWarning({
        type: "process-error",
        message: `Failed to start ${this.codexBin} app-server`,
        detail: error.message,
      });
    });

    this.child.on("exit", (code, signal) => {
      if (this.closed) {
        return;
      }

      this.closed = true;
      for (const { reject, method } of this.pending.values()) {
        reject(
          new Error(
            `${this.codexBin} app-server exited before replying to ${method} (code=${code}, signal=${signal})`,
          ),
        );
      }
      this.pending.clear();

      this.emitWarning({
        type: "process-exit",
        message: `${this.codexBin} app-server exited`,
        detail: `code=${code} signal=${signal}`,
      });
    });

    this.rl.on("line", (line) => {
      this.handleLine(line);
    });
  }

  handleLine(line) {
    let message;

    try {
      message = JSON.parse(line);
    } catch (error) {
      this.emitWarning({
        type: "json-parse-error",
        message: "Failed to parse app-server message",
        detail: error.message,
      });
      return;
    }

    if (this.debug) {
      this.emit("raw", line);
    }

    if (this.isServerRequest(message)) {
      this.handleServerRequest(message);
      return;
    }

    if (this.isResponse(message)) {
      this.handleResponse(message);
      return;
    }

    if (typeof message.method === "string") {
      this.handleNotification(message);
      return;
    }

    this.emitWarning({
      type: "unknown-message",
      message: "Received unknown app-server message shape",
      detail: line,
    });
  }

  isServerRequest(message) {
    return (
      message &&
      typeof message === "object" &&
      Object.prototype.hasOwnProperty.call(message, "id") &&
      typeof message.method === "string"
    );
  }

  isResponse(message) {
    return (
      message &&
      typeof message === "object" &&
      Object.prototype.hasOwnProperty.call(message, "id") &&
      !Object.prototype.hasOwnProperty.call(message, "method")
    );
  }

  handleResponse(message) {
    const pendingRequest = this.pending.get(message.id);
    if (!pendingRequest) {
      this.emitWarning({
        type: "unexpected-response",
        message: `Received response for unknown id=${message.id}`,
      });
      return;
    }

    this.pending.delete(message.id);

    if (message.error) {
      pendingRequest.reject(
        new Error(
          `${pendingRequest.method} failed: ${message.error.message} (code=${message.error.code})`,
        ),
      );
      return;
    }

    pendingRequest.resolve(message.result ?? {});
  }

  handleServerRequest(message) {
    const { id, method, params = {} } = message;

    if (
      method === "item/commandExecution/requestApproval" ||
      method === "item/fileChange/requestApproval"
    ) {
      const decision = autoApprovalDecision(params);
      this.send({ id, result: decision });
      this.recordSummaryEvent({
        type: "serverRequest",
        method,
        status: "auto-accepted",
        itemType: method.includes("commandExecution")
          ? "commandExecution"
          : "fileChange",
        textPreview: preview(params.reason) ?? preview(params.command) ?? preview(params.cwd),
      });
      this.pushSystemNote(`Auto-accepted ${method} in the gateway web UI.`);
      this.emit("serverRequest", {
        method,
        params,
        handled: true,
        result: decision,
      });
      this.emitState();
      return;
    }

    const error = {
      code: -32601,
      message: `Unsupported server request in gateway demo: ${method}`,
    };

    this.send({ id, error });
    this.recordSummaryEvent({
      type: "serverRequest",
      method,
      status: "rejected",
      textPreview: error.message,
    });
    this.pushSystemNote(error.message);
    this.emit("serverRequest", {
      method,
      params,
      handled: false,
      error,
    });
    this.emitState();
  }

  handleNotification(message) {
    const { method, params = {} } = message;
    const item = params.item ?? null;

    switch (method) {
      case "thread/started":
        this.threadId = params.thread?.id ?? this.threadId;
        break;
      case "thread/status/changed":
        this.threadStatus = params.status ?? this.threadStatus;
        break;
      case "turn/started":
        this.currentTurnId = params.turn?.id ?? this.currentTurnId;
        this.activeTurn = true;
        this.lastTurnStatus = "inProgress";
        break;
      case "turn/completed":
        this.currentTurnId = null;
        this.activeTurn = false;
        this.lastTurnStatus = params.turn?.status ?? null;
        break;
      case "item/started":
        this.handleStartedItem(item);
        break;
      case "item/agentMessage/delta":
        this.handleAgentMessageDelta(params);
        break;
      case "item/completed":
        this.handleCompletedItem(item);
        break;
      case "error":
        this.pushSystemNote(params.error?.message ?? "Unknown app-server error");
        break;
      default:
        break;
    }

    this.recordSummaryEvent(this.summarizeNotification(message));
    this.emit("notification", message);
    this.emitState();
  }

  handleStartedItem(item) {
    if (!item || !item.type) {
      return;
    }

    if (item.type === "agentMessage") {
      this.upsertTranscript({
        id: item.id,
        role: "assistant",
        text: item.text ?? "",
        status: "inProgress",
        source: "app-server",
      });
      return;
    }

    if (item.type === "userMessage") {
      const text = this.extractUserText(item);
      if (text && !this.hasRecentUserText(text)) {
        this.pushTranscript({
          id: item.id,
          role: "user",
          text,
          status: "completed",
          source: "app-server",
        });
      }
    }
  }

  handleCompletedItem(item) {
    if (!item || !item.type) {
      return;
    }

    if (item.type === "agentMessage") {
      this.upsertTranscript({
        id: item.id,
        role: "assistant",
        text: item.text ?? "",
        status: "completed",
        source: "app-server",
      });
      return;
    }

    if (item.type === "commandExecution" && item.status === "declined") {
      this.pushSystemNote("A command execution request was declined by the gateway UI.");
      return;
    }

    if (item.type === "fileChange" && item.status === "declined") {
      this.pushSystemNote("A file change request was declined by the gateway UI.");
    }
  }

  handleAgentMessageDelta(params) {
    const itemId = params.itemId ?? null;
    const delta = this.extractDeltaText(params);

    if (!itemId || !delta) {
      return;
    }

    const existing = this.getTranscriptEntry(itemId);
    if (!existing) {
      this.pushTranscript({
        id: itemId,
        role: "assistant",
        text: delta,
        status: "inProgress",
        source: "app-server",
      });
      return;
    }

    existing.text += delta;
    existing.status = "inProgress";
  }

  extractUserText(item) {
    const parts = item.content ?? [];
    return parts
      .filter((part) => part.type === "text" && typeof part.text === "string")
      .map((part) => part.text)
      .join("\n")
      .trim();
  }

  extractDeltaText(params) {
    const candidates = [
      params.delta,
      params.text,
      params.textDelta,
      params.chunk,
      params.content,
    ];

    for (const candidate of candidates) {
      if (typeof candidate === "string" && candidate.length > 0) {
        return candidate;
      }
    }

    return "";
  }

  hasRecentUserText(text) {
    const last = [...this.transcript].reverse().find((entry) => entry.role === "user");
    return last ? last.text === text : false;
  }

  upsertTranscript(entry) {
    const existing = this.getTranscriptEntry(entry.id);
    if (!existing) {
      this.pushTranscript(entry);
      return;
    }

    existing.role = entry.role ?? existing.role;
    existing.text = entry.text ?? existing.text;
    existing.status = entry.status ?? existing.status;
    existing.source = entry.source ?? existing.source;
  }

  pushTranscript(entry) {
    const normalized = {
      id: entry.id ?? this.makeLocalId(entry.role ?? "message"),
      role: entry.role ?? "system",
      text: entry.text ?? "",
      status: entry.status ?? "completed",
      source: entry.source ?? "local",
      createdAt: entry.createdAt ?? Date.now(),
    };

    this.transcript.push(normalized);
    this.transcriptIndex.set(normalized.id, this.transcript.length - 1);

    if (this.transcript.length > MAX_TRANSCRIPT) {
      this.transcript = this.transcript.slice(-MAX_TRANSCRIPT);
      this.rebuildTranscriptIndex();
    }
  }

  pushSystemNote(text) {
    this.pushTranscript({
      id: this.makeLocalId("system"),
      role: "system",
      text,
      status: "completed",
      source: "bridge",
    });
  }

  rebuildTranscriptIndex() {
    this.transcriptIndex = new Map();
    this.transcript.forEach((entry, index) => {
      this.transcriptIndex.set(entry.id, index);
    });
  }

  getTranscriptEntry(id) {
    const index = this.transcriptIndex.get(id);
    return index === undefined ? null : this.transcript[index];
  }

  recordSummaryEvent(entry) {
    if (!entry) {
      return;
    }

    this.recentEvents.push({
      at: new Date().toISOString(),
      ...entry,
    });

    if (this.recentEvents.length > MAX_EVENTS) {
      this.recentEvents = this.recentEvents.slice(-MAX_EVENTS);
    }
  }

  summarizeNotification(message) {
    const { method, params = {} } = message;
    const item = params.item ?? {};

    return {
      type: "notification",
      method,
      itemType: item.type ?? null,
      itemId: item.id ?? params.itemId ?? null,
      status: params.turn?.status ?? item.status ?? null,
      textPreview:
        preview(item.text) ??
        preview(params.delta) ??
        preview(params.error?.message) ??
        preview(item.command) ??
        preview(item.query),
    };
  }

  emitWarning(entry) {
    this.recordSummaryEvent({
      type: "warning",
      method: entry.type,
      status: "warning",
      textPreview: preview(entry.message) ?? preview(entry.detail),
    });
    this.emit("warning", entry);
    this.emitState();
  }

  emitState() {
    this.emit("state", this.getState());
  }

  send(message) {
    if (!this.child || this.closed) {
      throw new Error("app-server process is not available");
    }

    this.child.stdin.write(`${JSON.stringify(message)}\n`);
  }

  request(method, params = {}) {
    const id = this.nextId++;

    return new Promise((resolve, reject) => {
      this.pending.set(id, { method, resolve, reject });
      this.send({ method, id, params });
    });
  }

  async refreshAccount() {
    const result = await this.request("account/read", { refreshToken: false });
    this.account = result.account ?? null;
    this.requiresOpenaiAuth = result.requiresOpenaiAuth ?? null;
  }

  async refreshModels() {
    const result = await this.request("model/list", {
      limit: 50,
      includeHidden: false,
    });

    this.models = (result.data ?? []).map((model) => ({
      model: model.model,
      displayName: model.displayName,
      isDefault: Boolean(model.isDefault),
      hidden: Boolean(model.hidden),
      supportsPersonality: Boolean(model.supportsPersonality),
      inputModalities: model.inputModalities ?? ["text", "image"],
    }));

    if (this.models.length === 0) {
      throw new Error("model/list returned no visible models");
    }

    const configuredModel = readEnv(ENV_NAMES.defaultModel);
    this.selectedModel =
      configuredModel ||
      this.models.find((model) => model.isDefault)?.model ||
      this.models[0].model;
  }

  async startNewThread({ model } = {}) {
    const selectedModel = model || this.selectedModel;
    if (!selectedModel) {
      throw new Error("No model available for thread/start");
    }

    const result = await this.request("thread/start", {
      cwd: this.cwd,
      model: selectedModel,
    });

    const threadId = result.thread?.id;
    if (!threadId) {
      throw new Error("thread/start did not return a thread id");
    }

    this.threadId = threadId;
    this.selectedModel = selectedModel;
    this.threadStatus = { type: "idle" };
    this.currentTurnId = null;
    this.activeTurn = false;
    this.lastTurnStatus = null;
    this.transcript = [];
    this.rebuildTranscriptIndex();

    this.recordSummaryEvent({
      type: "local",
      method: "thread/new",
      status: "completed",
      textPreview: `Started thread ${threadId}`,
    });
    this.emitState();
    return threadId;
  }

  async sendPrompt(promptText) {
    const prompt = `${promptText ?? ""}`.trim();
    if (!prompt) {
      throw new Error("Prompt must not be empty");
    }

    if (this.activeTurn) {
      throw new Error("A turn is already in progress");
    }

    if (!this.threadId) {
      await this.startNewThread();
    }

    this.pushTranscript({
      id: this.makeLocalId("user"),
      role: "user",
      text: prompt,
      status: "completed",
      source: "local",
    });

    this.activeTurn = true;
    this.lastTurnStatus = "inProgress";
    this.emitState();

    await this.request("turn/start", {
      threadId: this.threadId,
      input: [{ type: "text", text: prompt }],
    });

    return { threadId: this.threadId };
  }

  waitForTurnCompletion(timeoutMs = 120000) {
    if (!this.activeTurn && this.lastTurnStatus) {
      return Promise.resolve({ status: this.lastTurnStatus });
    }

    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        cleanup();
        reject(new Error(`Timed out waiting for turn completion after ${timeoutMs}ms`));
      }, timeoutMs);

      const onNotification = (message) => {
        if (message.method !== "turn/completed") {
          return;
        }

        cleanup();
        resolve(message.params?.turn ?? {});
      };

      const cleanup = () => {
        clearTimeout(timeout);
        this.off("notification", onNotification);
      };

      this.on("notification", onNotification);
    });
  }

  getLatestAssistantText() {
    const assistant = [...this.transcript]
      .reverse()
      .find((entry) => entry.role === "assistant" && entry.text);
    return assistant?.text ?? "";
  }

  describeAccount() {
    if (!this.account) {
      return "none";
    }

    if (this.account.type === "chatgpt") {
      return `chatgpt:${this.account.email ?? "unknown"}`;
    }

    return this.account.type;
  }

  getState() {
    return clone({
      ready: this.ready,
      cwd: this.cwd,
      startedAt: this.startTimestamp,
      runtime: this.runtime,
      account: {
        raw: this.account,
        summary: this.describeAccount(),
        requiresOpenaiAuth: this.requiresOpenaiAuth,
      },
      models: this.models,
      selectedModel: this.selectedModel,
      threadId: this.threadId,
      threadStatus: this.threadStatus,
      currentTurnId: this.currentTurnId,
      activeTurn: this.activeTurn,
      lastTurnStatus: this.lastTurnStatus,
      transcript: this.transcript,
      recentEvents: this.recentEvents,
    });
  }

  async stop() {
    if (this.closed) {
      return;
    }

    this.closed = true;
    this.rl?.close();
    this.child?.stdin.end();
    this.child?.kill();
  }

  makeLocalId(prefix) {
    this.localCounter += 1;
    return `local-${prefix}-${this.localCounter}`;
  }
}
