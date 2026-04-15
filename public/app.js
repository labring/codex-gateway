const readyStateEl = document.querySelector("#ready-state");
const accountStateEl = document.querySelector("#account-state");
const sessionStateEl = document.querySelector("#session-state");
const threadStateEl = document.querySelector("#thread-state");
const turnStateEl = document.querySelector("#turn-state");
const modelSelectEl = document.querySelector("#model-select");
const connectionStateEl = document.querySelector("#connection-state");
const transcriptEl = document.querySelector("#transcript");
const eventsEl = document.querySelector("#events");
const eventCountEl = document.querySelector("#event-count");
const formEl = document.querySelector("#composer");
const promptEl = document.querySelector("#prompt");
const sendEl = document.querySelector("#send");
const stopEl = document.querySelector("#stop-turn");
const newThreadEl = document.querySelector("#new-thread");
const errorEl = document.querySelector("#error");
const authTokenEl = document.querySelector("#auth-token");

let state = null;
let eventSource = null;
let sessionId = null;
let authToken = "";

function escapeHtml(value) {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

function showError(message) {
  errorEl.hidden = false;
  errorEl.textContent = message;
}

function clearError() {
  errorEl.hidden = true;
  errorEl.textContent = "";
}

function setConnectionState(label) {
  connectionStateEl.textContent = label;
}

function renderState(nextState) {
  state = nextState;

  readyStateEl.textContent = state.ready ? "ready" : "starting";
  accountStateEl.textContent = state.account?.summary ?? "unknown";
  sessionStateEl.textContent = sessionId ?? "not started";
  threadStateEl.textContent = state.threadId ?? "not started";
  turnStateEl.textContent = state.activeTurn
    ? `running${state.currentTurnId ? ` (${state.currentTurnId.slice(0, 8)})` : ""}`
    : state.lastTurnStatus || "idle";

  renderModelOptions();
  renderTranscript();
  renderEvents();
  renderControls();
}

function renderModelOptions() {
  const models = state?.models ?? [];
  const currentValue = state?.selectedModel ?? "";

  modelSelectEl.innerHTML = models
    .map(
      (model) =>
        `<option value="${escapeHtml(model.model)}" ${
          model.model === currentValue ? "selected" : ""
        }>${escapeHtml(model.displayName || model.model)}</option>`,
    )
    .join("");
}

function renderTranscript() {
  const transcript = state?.transcript ?? [];

  if (transcript.length === 0) {
    transcriptEl.innerHTML = `
      <div class="empty-state">
        <p>No messages yet.</p>
        <p>Start with a small prompt to confirm the bridge is healthy.</p>
      </div>
    `;
    return;
  }

  transcriptEl.innerHTML = transcript
    .map((entry) => {
      const text = escapeHtml(entry.text || "").replaceAll("\n", "<br />");
      return `
        <article class="message message-${escapeHtml(entry.role)}">
          <header>
            <span class="role">${escapeHtml(entry.role)}</span>
            <span class="status">${escapeHtml(entry.status)}</span>
          </header>
          <div class="body">${text || "<span class=\"muted\">(empty)</span>"}</div>
        </article>
      `;
    })
    .join("");

  transcriptEl.scrollTop = transcriptEl.scrollHeight;
}

function renderEvents() {
  const events = (state?.recentEvents ?? []).slice(-30).reverse();
  eventCountEl.textContent = String(state?.recentEvents?.length ?? 0);

  if (events.length === 0) {
    eventsEl.innerHTML = `<p class="muted">No events yet.</p>`;
    return;
  }

  eventsEl.innerHTML = events
    .map(
      (event) => `
        <div class="event-row">
          <div class="event-top">
            <span class="event-method">${escapeHtml(event.method || event.type || "event")}</span>
            <span class="event-status">${escapeHtml(event.status || "-")}</span>
          </div>
          <div class="event-preview">${escapeHtml(event.textPreview || event.itemType || "")}</div>
        </div>
      `,
    )
    .join("");
}

function renderControls() {
  const busy = Boolean(state?.activeTurn);
  const unavailable = !sessionId;
  sendEl.disabled = busy || unavailable;
  stopEl.disabled = !busy || unavailable || !state?.currentTurnId;
  newThreadEl.disabled = busy || unavailable;
  modelSelectEl.disabled = busy || unavailable;
  promptEl.disabled = !state?.ready || unavailable;
}

function sessionPath(suffix = "") {
  if (!sessionId) {
    throw new Error("Session is not ready.");
  }

  return `/api/sessions/${encodeURIComponent(sessionId)}${suffix}`;
}

function authHeaders() {
  return authToken ? { Authorization: `Bearer ${authToken}` } : {};
}

async function postJson(url, body) {
  const response = await fetch(url, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      ...authHeaders(),
    },
    body: JSON.stringify(body),
  });

  const payload = await response.json();
  if (!response.ok) {
    throw new Error(payload.error || "Request failed");
  }

  return payload;
}

async function createSession() {
  const payload = await postJson("/api/sessions", {});
  sessionId = payload.sessionId;
  renderState(payload.state);
}

function connectEvents() {
  if (!sessionId) {
    throw new Error("Session is not ready.");
  }

  eventSource?.close();
  const url = new URL(sessionPath("/events"), window.location.origin);
  if (authToken) {
    url.searchParams.set("access_token", authToken);
  }
  eventSource = new EventSource(url);

  eventSource.addEventListener("open", () => {
    setConnectionState("streaming");
  });

  eventSource.addEventListener("session", (event) => {
    const session = JSON.parse(event.data);
    sessionId = session.id;
    sessionStateEl.textContent = session.id;
  });

  eventSource.addEventListener("state", (event) => {
    renderState(JSON.parse(event.data));
  });

  eventSource.addEventListener("warning", (event) => {
    const warning = JSON.parse(event.data);
    showError(warning.message || "Warning from server");
  });

  eventSource.addEventListener("server-request", (event) => {
    const request = JSON.parse(event.data);
    if (request.handled && request.result === "decline") {
      showError(`Declined ${request.method}`);
    }
  });

  eventSource.addEventListener("session-closed", (event) => {
    const payload = JSON.parse(event.data);
    showError(`Session closed: ${payload.reason}`);
    setConnectionState("closed");
  });

  eventSource.onerror = () => {
    setConnectionState("reconnecting");
  };
}

formEl.addEventListener("submit", async (event) => {
  event.preventDefault();
  clearError();

  const prompt = promptEl.value.trim();
  if (!prompt) {
    showError("Prompt must not be empty.");
    return;
  }

  try {
    await postJson(sessionPath("/turn"), { prompt });
    promptEl.value = "";
  } catch (error) {
    showError(error.message);
  }
});

newThreadEl.addEventListener("click", async () => {
  clearError();

  try {
    await postJson(sessionPath("/thread/new"), {
      model: modelSelectEl.value || undefined,
    });
  } catch (error) {
    showError(error.message);
  }
});

stopEl.addEventListener("click", async () => {
  clearError();

  try {
    await postJson(sessionPath("/turn/interrupt"), {});
  } catch (error) {
    showError(error.message);
  }
});

authTokenEl.addEventListener("change", () => {
  authToken = authTokenEl.value.trim();
  if (sessionId) {
    connectEvents();
  }
});

window.addEventListener("pagehide", () => {
  if (!sessionId) {
    return;
  }

  eventSource?.close();
  void fetch(sessionPath(""), {
    method: "DELETE",
    headers: authHeaders(),
    keepalive: true,
  });
});

try {
  await createSession();
  connectEvents();
} catch (error) {
  showError(error.message);
  setConnectionState("offline");
}
