const readyStateEl = document.querySelector("#ready-state");
const accountStateEl = document.querySelector("#account-state");
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
const newThreadEl = document.querySelector("#new-thread");
const errorEl = document.querySelector("#error");

let state = null;
let eventSource = null;

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
        <p>Start with a small read-only prompt to confirm the bridge is healthy.</p>
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
  sendEl.disabled = busy;
  newThreadEl.disabled = busy;
  modelSelectEl.disabled = busy;
  promptEl.disabled = !state?.ready;
}

async function loadState() {
  const response = await fetch("/api/state");
  const payload = await response.json();
  renderState(payload);
}

async function postJson(url, body) {
  const response = await fetch(url, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
    },
    body: JSON.stringify(body),
  });

  const payload = await response.json();
  if (!response.ok) {
    throw new Error(payload.error || "Request failed");
  }

  return payload;
}

function connectEvents() {
  eventSource = new EventSource("/api/events");

  eventSource.addEventListener("open", () => {
    setConnectionState("streaming");
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
      showError(`Auto-declined ${request.method}`);
    }
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
    await postJson("/api/turn", { prompt });
    promptEl.value = "";
  } catch (error) {
    showError(error.message);
  }
});

newThreadEl.addEventListener("click", async () => {
  clearError();

  try {
    await postJson("/api/thread/new", { model: modelSelectEl.value || undefined });
  } catch (error) {
    showError(error.message);
  }
});

await loadState();
connectEvents();
