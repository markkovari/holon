let es = null;
let reconnectTimer = null;
let pendingCount = 0;
let inflightCount = 0;
let deadCount = 0;

async function api(path, method = 'GET', body = null) {
  const headers = { 'content-type': 'application/json' };
  const res = await fetch(path, {
    method,
    headers,
    body: body ? JSON.stringify(body) : null
  });
  return res.json().catch(() => ({}));
}

function connectSSE() {
  if (es) es.close();
  // Get latest events
  es = new EventSource('/api/stream');
  
  es.onmessage = (event) => {
    const data = JSON.parse(event.data);
    appendLog(data);
    updateStats(data);
  };
  
  es.onerror = () => {
    es.close();
    clearTimeout(reconnectTimer);
    reconnectTimer = setTimeout(connectSSE, 2000);
  };
}

function appendLog(data) {
  const logWindow = document.getElementById('eventLog');
  if (logWindow.children.length === 1 && logWindow.children[0].innerText.includes('Waiting for events')) {
    logWindow.innerHTML = '';
  }

  const el = document.createElement('div');
  el.className = 'log-entry';
  const time = new Date(data.at * 1000).toLocaleTimeString();
  
  el.innerHTML = `
    <div style="display: flex; justify-content: space-between; margin-bottom: 0.25rem;">
      <strong style="color: #fff;">${data.topic || '(no topic)'}</strong>
      <span style="color: var(--text-muted); font-size: 0.7rem;">${time}</span>
    </div>
    <div style="display: flex; justify-content: space-between; align-items: center;">
      <span style="color: #64748b;">${data.id.substring(0, 8)}...</span>
      <span class="state-badge state-${data.state.toLowerCase()}">${data.state}</span>
    </div>
    ${data.attempts > 0 ? `<div style="font-size: 0.7rem; color: #fb7185; margin-top: 0.25rem;">Attempts: ${data.attempts}</div>` : ''}
  `;
  
  logWindow.prepend(el);
  if (logWindow.children.length > 50) {
    logWindow.lastChild.remove();
  }
}

function updateStats(data) {
  if (data.state === 'enqueued') pendingCount++;
  if (data.state === 'in-flight') inflightCount++;
  if (data.state === 'acked') {
    if (inflightCount > 0) inflightCount--;
    if (pendingCount > 0) pendingCount--;
  }
  if (data.state === 'dead') {
    if (inflightCount > 0) inflightCount--;
    if (pendingCount > 0) pendingCount--;
    deadCount++;
    loadDeadLetters(); // refresh list
  }
  
  document.getElementById('statPending').innerText = pendingCount;
  document.getElementById('statInFlight').innerText = inflightCount;
  document.getElementById('statDead').innerText = deadCount;
}

async function loadDeadLetters() {
  const res = await api('/api/dead-letters');
  const dlq = res.dead || [];
  deadCount = dlq.length;
  document.getElementById('statDead').innerText = deadCount;
  
  const list = document.getElementById('deadLettersList');
  if (dlq.length === 0) {
    list.innerHTML = '<div style="padding: 1rem; color: #94a3b8; font-style: italic; text-align: center; font-size: 0.85rem;">No dead letters.</div>';
    return;
  }
  
  list.innerHTML = dlq.map(d => `
    <div class="dlq-item">
      <div style="display: flex; justify-content: space-between; margin-bottom: 0.25rem;">
        <strong style="color: #fff; font-size: 0.85rem;">${d.topic}</strong>
        <span style="font-size: 0.7rem; color: #fb7185;">${d.attempts} attempts</span>
      </div>
      <div style="font-size: 0.75rem; color: #64748b; margin-bottom: 0.5rem; word-break: break-all;">${d.id}</div>
      <button class="btn-secondary" style="font-size: 0.7rem; padding: 0.25rem 0.5rem;" onclick="replayDead('${d.id}')">↻ Replay Event</button>
    </div>
  `).join('');
}

async function toggleSink(isUp) {
  const text = document.getElementById('sinkStatusText');
  text.innerText = isUp ? 'UP' : 'DOWN';
  text.style.color = isUp ? 'var(--success)' : 'var(--danger)';
  await api('/api/sink', 'POST', { up: isUp });
}

async function submitEvent() {
  const topic = document.getElementById('eventTopic').value.trim();
  const payloadStr = document.getElementById('eventPayload').value.trim();
  let payload;
  try {
    payload = JSON.parse(payloadStr);
  } catch (e) {
    return alert('Invalid JSON payload');
  }
  
  if (!topic) return alert('Topic required');
  
  await api('/api/events', 'POST', { topic, payload });
  closeModal('enqueueModal');
}

async function replayDead(id) {
  await api(`/api/dead-letters/${id}/replay`, 'POST');
  setTimeout(loadDeadLetters, 1000);
}

function openEnqueueModal() { document.getElementById('enqueueModal').style.display = 'flex'; }
function closeModal(id) { document.getElementById(id).style.display = 'none'; }

// Init
connectSSE();
loadDeadLetters();
