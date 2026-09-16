const API_BASE = '/api';
let currentToken = null;

const authOverlay = document.getElementById('auth-overlay');
const userDisplay = document.getElementById('user-display');
const logoutBtn = document.getElementById('logout-btn');

const createTicketForm = document.getElementById('create-ticket-form');
const searchTicketForm = document.getElementById('search-ticket-form');
const searchResultsSection = document.getElementById('search-results-section');
const ticketsList = document.getElementById('tickets-list');

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
        
        authOverlay.classList.add('hidden');
        userDisplay.innerText = `Logged in as ${subject}`;
        showToast(`Authenticated as ${subject}`);
    } catch (e) {
        showToast(e.message);
    }
}

document.getElementById('login-lead').addEventListener('click', () => 
    login('lead@example.test', ['lead'], ['tickets:write', 'tickets:read', 'tickets:resolve'])
);
document.getElementById('login-agent-billing').addEventListener('click', () => 
    login('agent-b@example.test', ['agent:billing'], ['tickets:write', 'tickets:read', 'tickets:resolve'])
);
document.getElementById('login-agent-search').addEventListener('click', () => 
    login('agent-s@example.test', ['agent:search'], ['tickets:write', 'tickets:read', 'tickets:resolve'])
);

logoutBtn.addEventListener('click', () => {
    currentToken = null;
    authOverlay.classList.remove('hidden');
    searchResultsSection.classList.add('hidden');
});

// Create Ticket
createTicketForm.addEventListener('submit', async (e) => {
    e.preventDefault();
    const subject = document.getElementById('ticket-subject').value;
    const body = document.getElementById('ticket-body').value;
    const queue = document.getElementById('ticket-queue').value;
    
    try {
        const res = await fetch(`${API_BASE}/tickets`, {
            method: 'POST',
            headers: { 
                'Authorization': `Bearer ${currentToken}`,
                'Content-Type': 'application/json'
            },
            body: JSON.stringify({ subject, body, queue })
        });
        if (!res.ok) throw new Error('Failed to create ticket');
        showToast('Ticket created successfully!');
        createTicketForm.reset();
    } catch (err) {
        showToast(err.message);
    }
});

// Search Tickets
searchTicketForm.addEventListener('submit', async (e) => {
    e.preventDefault();
    const query = document.getElementById('search-query').value;
    const queue = document.getElementById('search-queue').value;
    
    try {
        const res = await fetch(`${API_BASE}/tickets/search?q=${encodeURIComponent(query)}&queue=${encodeURIComponent(queue)}`, {
            headers: { 'Authorization': `Bearer ${currentToken}` }
        });
        if (!res.ok) throw new Error('Failed to search tickets');
        const data = await res.json();
        
        displayResults(data.results);
    } catch (err) {
        showToast(err.message);
    }
});

function displayResults(tickets) {
    searchResultsSection.classList.remove('hidden');
    ticketsList.innerHTML = '';
    
    if (!tickets || tickets.length === 0) {
        ticketsList.innerHTML = '<p>No tickets found.</p>';
        return;
    }

    tickets.forEach(ticket => {
        const card = document.createElement('div');
        card.className = 'ticket-card';
        card.innerHTML = `
            <div class="ticket-queue">${ticket.queue}</div>
            <div class="ticket-subject">${ticket.subject}</div>
            <div class="ticket-body">${ticket.body}</div>
            <div class="ticket-actions">
                <button class="primary-btn resolve-btn" data-id="${ticket.id}">Resolve</button>
            </div>
        `;
        
        card.querySelector('.resolve-btn').addEventListener('click', () => resolveTicket(ticket.id));
        ticketsList.appendChild(card);
    });
}

// Resolve Ticket
async function resolveTicket(id) {
    try {
        const res = await fetch(`${API_BASE}/tickets/${id}/resolve`, {
            method: 'POST',
            headers: { 'Authorization': `Bearer ${currentToken}` }
        });
        
        if (res.status === 403) {
            showToast("Access Denied: You do not have permission to resolve tickets in this queue.");
            return;
        }
        if (!res.ok) throw new Error('Failed to resolve ticket');
        
        showToast('Ticket resolved!');
        
        // Refresh search results
        document.getElementById('search-ticket-form').dispatchEvent(new Event('submit'));
    } catch (err) {
        showToast(err.message);
    }
}
