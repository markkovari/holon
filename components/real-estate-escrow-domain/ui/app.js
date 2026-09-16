let token = localStorage.getItem('ree_token');
let user = null;

async function api(path, method = 'GET', body = null) {
  const headers = { 'content-type': 'application/json' };
  if (token) headers['authorization'] = `Bearer ${token}`;
  
  const res = await fetch(path, {
    method,
    headers,
    body: body ? JSON.stringify(body) : null
  });
  
  if (res.status === 401) {
    token = null;
    localStorage.removeItem('ree_token');
    showAuth();
    return { error: 'unauthorized' };
  }
  
  const text = await res.text();
  try { return JSON.parse(text); } catch(e) { return {}; }
}

function showAuth() {
  document.getElementById('authOverlay').style.display = 'flex';
}

function hideAuth() {
  document.getElementById('authOverlay').style.display = 'none';
}

async function handleAuth(action) {
  const email = document.getElementById('authEmail').value;
  const password = document.getElementById('authPassword').value;
  const role = document.getElementById('authRole').value;
  
  let res = await api(`/api/${action}`, 'POST', { email, password, role });
  if (action === 'register' && res.subject) {
    res = await api('/api/login', 'POST', { email, password });
  }
  
  if (res.access_token) {
    token = res.access_token;
    localStorage.setItem('ree_token', token);
    hideAuth();
    await checkMe();
  } else {
    alert(res.error || 'Authentication failed');
  }
}

async function logout() {
  if (token) await api('/api/logout', 'POST');
  token = null;
  localStorage.removeItem('ree_token');
  user = null;
  document.getElementById('userName').innerText = 'Guest';
  document.getElementById('txGrid').innerHTML = '';
  showAuth();
}

async function checkMe() {
  if (!token) return showAuth();
  
  const res = await api('/api/me');
  if (res.subject) {
    user = res;
    document.getElementById('userName').innerText = res.subject.substring(0, 12) + '...';
    hideAuth();
    loadItems();
  } else {
    showAuth();
  }
}

async function loadItems() {
  const grid = document.getElementById('txGrid');
  const res = await api('/api/items');
  
  if (!res.items || res.items.length === 0) {
    grid.innerHTML = '<div style="grid-column: 1 / -1; padding: 3rem; text-align: center; color: var(--text-muted);">No escrow transactions found.</div>';
    return;
  }
  
  grid.innerHTML = res.items.map(item => `
    <div class="tx-card">
      <div class="tx-id">${item.id}</div>
      <div class="tx-title">${item.name}</div>
      <div class="tx-owner">${item.owner.substring(0, 12)}...</div>
    </div>
  `).join('');
}

function openCreateModal() {
  if (!user || !user.roles.includes('agent')) {
    return alert('Only agents can create transactions');
  }
  document.getElementById('createModal').style.display = 'flex';
}

function closeModal(id) {
  document.getElementById(id).style.display = 'none';
}

async function submitTransaction() {
  const name = document.getElementById('txName').value;
  if (!name) return alert('Name required');
  
  const res = await api('/api/items', 'POST', { name });
  if (res.error) return alert(res.error);
  
  closeModal('createModal');
  loadItems();
}

// Init
checkMe();
