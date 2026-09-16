const API_BASE = '/api';
let currentToken = null;
let currentRole = null;
let currentSubject = null;

// DOM Elements
const authOverlay = document.getElementById('auth-overlay');
const userDisplay = document.getElementById('user-display');
const logoutBtn = document.getElementById('logout-btn');
const coordActions = document.getElementById('coordinator-actions');
const volInfo = document.getElementById('volunteer-info');
const shiftsList = document.getElementById('shifts-list');
const createShiftForm = document.getElementById('create-shift-form');
const refreshBtn = document.getElementById('refresh-btn');

function showToast(msg) {
    const t = document.createElement('div');
    t.className = 'toast';
    t.innerText = msg;
    document.getElementById('toast-container').appendChild(t);
    setTimeout(() => t.remove(), 3000);
}

// Authentication
async function login(subject, roles, scopes) {
    try {
        const res = await fetch('/test/token', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ subject, roles, scopes })
        });
        if (!res.ok) throw new Error('Login failed');
        const data = await res.json();
        currentToken = data.token;
        currentRole = roles[0];
        currentSubject = subject;
        
        authOverlay.classList.add('hidden');
        userDisplay.innerText = `Logged in as ${subject} (${currentRole})`;
        showToast(`Authenticated as ${currentRole}`);
        
        if (currentRole === 'coordinator') {
            coordActions.classList.remove('hidden');
            volInfo.classList.add('hidden');
        } else {
            coordActions.classList.add('hidden');
            volInfo.classList.remove('hidden');
        }
        
        loadShifts();
    } catch (e) {
        showToast(e.message);
    }
}

document.getElementById('login-coord').addEventListener('click', () => 
    login('coord@example.test', ['coordinator'], ['shifts:write', 'shifts:read', 'signups:write', 'signups:read'])
);
document.getElementById('login-vol-a').addEventListener('click', () => 
    login('vol-a@example.test', ['volunteer'], ['shifts:read', 'signups:write', 'signups:read'])
);
document.getElementById('login-vol-b').addEventListener('click', () => 
    login('vol-b@example.test', ['volunteer'], ['shifts:read', 'signups:write', 'signups:read'])
);

logoutBtn.addEventListener('click', () => {
    currentToken = null;
    currentRole = null;
    currentSubject = null;
    authOverlay.classList.remove('hidden');
    coordActions.classList.add('hidden');
    volInfo.classList.add('hidden');
    shiftsList.innerHTML = '';
});

// API Calls
async function loadShifts() {
    if (!currentToken) return;
    try {
        const res = await fetch(`${API_BASE}/shifts`, {
            headers: { 'Authorization': `Bearer ${currentToken}` }
        });
        if (!res.ok) throw new Error('Failed to load shifts');
        const data = await res.json();
        
        shiftsList.innerHTML = '';
        if (!data.shifts || data.shifts.length === 0) {
            shiftsList.innerHTML = '<p>No shifts available.</p>';
            return;
        }

        data.shifts.forEach(shift => {
            const card = document.createElement('div');
            card.className = 'shift-card';
            card.innerHTML = `
                <div class="shift-title">${shift.title}</div>
                <div class="shift-slots">Slots: ${shift.filled} / ${shift.slots}</div>
                <div class="shift-actions">
                    <!-- We assume signup ID tracking is done via a mock local storage or by fetching signups, but since we don't have a list signups API, we'll store my signup id in the button dataset -->
                    <button class="primary-btn signup-btn" data-id="${shift.id}" ${shift.filled >= shift.slots ? 'disabled' : ''}>Sign Up</button>
                    <button class="danger-btn cancel-btn hidden" data-id="${shift.id}">Cancel Signup</button>
                </div>
            `;
            
            // local state to mock tracking signup ID
            const mySignupId = localStorage.getItem(`signup_${shift.id}_${currentSubject}`);
            
            const signupBtn = card.querySelector('.signup-btn');
            const cancelBtn = card.querySelector('.cancel-btn');
            
            if (mySignupId) {
                signupBtn.classList.add('hidden');
                cancelBtn.classList.remove('hidden');
                cancelBtn.dataset.signup = mySignupId;
            }

            signupBtn.addEventListener('click', () => handleSignup(shift.id));
            cancelBtn.addEventListener('click', () => handleCancel(shift.id, cancelBtn.dataset.signup));
            
            shiftsList.appendChild(card);
        });
    } catch (e) {
        showToast(e.message);
    }
}

async function handleSignup(shiftId) {
    try {
        const res = await fetch(`${API_BASE}/shifts/${shiftId}/signup`, {
            method: 'POST',
            headers: { 'Authorization': `Bearer ${currentToken}` }
        });
        if (res.status === 429) {
            showToast("Quota exceeded! Limit of 3 per week.");
            return;
        }
        if (!res.ok) throw new Error('Signup failed');
        const data = await res.json();
        
        localStorage.setItem(`signup_${shiftId}_${currentSubject}`, data.signup_id);
        showToast('Signed up successfully!');
        loadShifts();
    } catch (e) {
        showToast(e.message);
    }
}

async function handleCancel(shiftId, signupId) {
    try {
        const res = await fetch(`${API_BASE}/shifts/${shiftId}/signups/${signupId}/cancel`, {
            method: 'POST',
            headers: { 'Authorization': `Bearer ${currentToken}` }
        });
        if (res.status === 403) {
            showToast("You are not allowed to cancel this signup.");
            return;
        }
        if (!res.ok) throw new Error('Cancel failed');
        
        localStorage.removeItem(`signup_${shiftId}_${currentSubject}`);
        showToast('Signup cancelled!');
        loadShifts();
    } catch (e) {
        showToast(e.message);
    }
}

createShiftForm.addEventListener('submit', async (e) => {
    e.preventDefault();
    const title = document.getElementById('shift-title').value;
    const slots = parseInt(document.getElementById('shift-slots').value);
    
    try {
        const res = await fetch(`${API_BASE}/shifts`, {
            method: 'POST',
            headers: { 
                'Authorization': `Bearer ${currentToken}`,
                'Content-Type': 'application/json'
            },
            body: JSON.stringify({ title, slots })
        });
        if (!res.ok) throw new Error('Failed to create shift');
        showToast('Shift created successfully!');
        createShiftForm.reset();
        loadShifts();
    } catch (err) {
        showToast(err.message);
    }
});

refreshBtn.addEventListener('click', loadShifts);
