const API_BASE = '/api';
let currentToken = null;
let currentUser = null;
let currentRole = null;

// DOM Elements
const authSection = document.getElementById('auth-section');
const dashboardSection = document.getElementById('dashboard-section');
const authControls = document.getElementById('auth-controls');
const userInfo = document.getElementById('user-info');
const currentUserSpan = document.getElementById('current-user');
const adminControls = document.getElementById('admin-controls');
const devicesGrid = document.getElementById('devices-grid');
const createDeviceForm = document.getElementById('create-device-form');

// Toast
function showToast(msg) {
    const toast = document.createElement('div');
    toast.className = 'toast';
    toast.textContent = msg;
    document.getElementById('toast-container').appendChild(toast);
    setTimeout(() => {
        toast.remove();
    }, 3000);
}

// Authentication
async function register(email, password) {
    const res = await fetch(`${API_BASE}/register`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ email, password })
    });
    return res.ok;
}

async function login(email, password) {
    const res = await fetch(`${API_BASE}/login`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ email, password })
    });
    
    if (res.ok) {
        const data = await res.json();
        currentToken = data.access_token;
        
        // Get user info
        const meRes = await fetch(`${API_BASE}/me`, {
            headers: { 'Authorization': `Bearer ${currentToken}` }
        });
        const meData = await meRes.json();
        currentUser = meData.subject;
        currentRole = meData.roles.includes('admin') ? 'admin' : 'user';
        
        showToast(`Authenticated as ${currentUser}`);
        updateUI();
        loadDevices();
    } else {
        showToast('Login failed');
        throw new Error('Login failed');
    }
}

async function mockLogin(email) {
    const password = "password123";
    try {
        await login(email, password);
    } catch (e) {
        // Attempt register then login
        await register(email, password);
        await login(email, password);
    }
}

document.getElementById('login-admin').addEventListener('click', () => mockLogin('admin@example.test'));
document.getElementById('login-user').addEventListener('click', () => mockLogin('user@example.test'));
document.getElementById('logout-btn').addEventListener('click', async () => {
    if (currentToken) {
        await fetch(`${API_BASE}/logout`, {
            method: 'POST',
            headers: { 'Authorization': `Bearer ${currentToken}` }
        });
    }
    currentToken = null;
    currentUser = null;
    currentRole = null;
    updateUI();
});

function updateUI() {
    if (currentToken) {
        authSection.classList.add('hidden');
        authControls.classList.add('hidden');
        dashboardSection.classList.remove('hidden');
        userInfo.classList.remove('hidden');
        currentUserSpan.textContent = currentUser;
        adminControls.classList.remove('hidden');
    } else {
        authSection.classList.remove('hidden');
        authControls.classList.remove('hidden');
        dashboardSection.classList.add('hidden');
        userInfo.classList.add('hidden');
    }
}

// Devices
createDeviceForm.addEventListener('submit', async (e) => {
    e.preventDefault();
    const name = document.getElementById('new-device-name').value;
    
    const res = await fetch(`${API_BASE}/items`, {
        method: 'POST',
        headers: { 
            'Content-Type': 'application/json',
            'Authorization': `Bearer ${currentToken}`
        },
        body: JSON.stringify({ name })
    });
    
    if (res.ok) {
        showToast('Device added successfully!');
        document.getElementById('new-device-name').value = '';
        loadDevices();
    } else {
        showToast('Failed to add device');
    }
});

async function loadDevices() {
    if (!currentToken) return;
    try {
        const res = await fetch(`${API_BASE}/items`, {
            headers: { 'Authorization': `Bearer ${currentToken}` }
        });
        if (res.ok) {
            const data = await res.json();
            displayDevices(data.items || []);
        }
    } catch (e) {
        console.error(e);
    }
}

function displayDevices(devices) {
    devicesGrid.innerHTML = '';
    if (devices.length === 0) {
        devicesGrid.innerHTML = '<p class="text-muted" style="grid-column: 1 / -1; text-align: center;">No devices found.</p>';
        return;
    }

    devices.forEach(device => {
        const isOn = device.state === 'on';
        const card = document.createElement('div');
        card.className = `device-card ${isOn ? 'is-on' : ''}`;
        card.dataset.id = device.id;
        
        card.innerHTML = `
            <div class="device-header">
                <div class="device-name">${device.name}</div>
                <div class="device-status ${isOn ? 'status-on' : 'status-off'}">${isOn ? 'ON' : 'OFF'}</div>
            </div>
            <div class="toggle-switch"></div>
        `;
        
        card.addEventListener('click', () => toggleDevice(device.id, card));
        devicesGrid.appendChild(card);
    });
}

async function toggleDevice(id, cardElement) {
    if (!currentToken) return;
    
    // Optimistic update
    const isOn = cardElement.classList.contains('is-on');
    cardElement.classList.toggle('is-on');
    const statusEl = cardElement.querySelector('.device-status');
    
    if (isOn) {
        statusEl.textContent = 'OFF';
        statusEl.className = 'device-status status-off';
    } else {
        statusEl.textContent = 'ON';
        statusEl.className = 'device-status status-on';
    }
    
    try {
        const res = await fetch(`${API_BASE}/items/${id}/toggle`, {
            method: 'POST',
            headers: { 'Authorization': `Bearer ${currentToken}` }
        });
        
        if (!res.ok) {
            throw new Error('Failed to toggle');
        }
        showToast(`Device turned ${!isOn ? 'ON' : 'OFF'}`);
    } catch (e) {
        // Revert on failure
        showToast('Toggle failed, reverting...');
        loadDevices();
    }
}
