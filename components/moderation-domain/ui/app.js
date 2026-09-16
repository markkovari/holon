let token = null;

async function getAuthToken() {
  if (token) return token;
  const res = await fetch('/test/token', {
    method: 'POST',
    body: JSON.stringify({
      subject: 'mod_agent',
      scopes: ['items:read', 'items:write', 'items:moderate']
    })
  });
  const data = await res.json();
  token = data.token;
  return token;
}

async function api(path, method = 'GET', body = null) {
  const t = await getAuthToken();
  const headers = {
    'content-type': 'application/json',
    'authorization': `Bearer ${t}`
  };
  const res = await fetch(path, {
    method,
    headers,
    body: body ? JSON.stringify(body) : null
  });
  if (res.status === 204) return null;
  return res.json();
}

async function seedData() {
  await api('/test/seed', 'POST');
  await loadQueue();
}

async function submitItem() {
  const text = document.getElementById('intakeText').value;
  if (!text) return alert('Enter text');
  
  await api('/api/items', 'POST', { text });
  document.getElementById('intakeText').value = '';
  await loadQueue();
}

async function loadQueue() {
  const list = document.getElementById('queueList');
  list.innerHTML = '<div style="color: var(--text-muted); text-align: center; padding: 2rem;">Loading...</div>';
  
  const res = await api('/api/queue');
  if (!res.items || res.items.length === 0) {
    list.innerHTML = '<div style="color: var(--text-muted); text-align: center; padding: 2rem;">Queue is empty.</div>';
    return;
  }
  
  list.innerHTML = res.items.map(item => `
    <div class="queue-item" onclick="selectItem('${item.id}', '${item.text.replace(/'/g, "\\'")}', '${item.submitted_at}')">
      <div class="queue-item-id">ID: ${item.id}</div>
      <div class="queue-item-text">${item.text}</div>
    </div>
  `).join('');
}

function selectItem(id, text, time) {
  const panel = document.getElementById('reviewPanel');
  panel.innerHTML = `
    <div class="review-meta">
      <div class="meta-row"><span class="meta-label">ID</span><span class="meta-value">${id}</span></div>
      <div class="meta-row"><span class="meta-label">Submitted</span><span class="meta-value">${time || 'Unknown'}</span></div>
    </div>
    <div style="background: var(--bg); padding: 1rem; border-radius: 8px; border: 1px solid var(--border); margin-bottom: 2rem; font-size: 0.95rem; line-height: 1.5;">
      ${text}
    </div>
    <div style="display: flex; gap: 1rem;">
      <button class="btn-primary" style="flex: 1; justify-content: center;" onclick="reviewItem('${id}')">Run Automated Review</button>
    </div>
  `;
}

async function reviewItem(id) {
  const res = await api(`/api/items/${id}/review`, 'POST');
  if (res && res.error) {
      alert('Review failed: ' + res.error);
  } else {
      document.getElementById('reviewPanel').innerHTML = `<div style="color: var(--text-muted); text-align: center; padding: 2rem;">Review Complete: ${res ? res.final : 'Success'}</div>`;
  }
  await loadQueue();
}

// Init
loadQueue();
