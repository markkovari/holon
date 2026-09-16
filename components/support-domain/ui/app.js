let currentTickets = [];
let currentTicketId = null;

async function api(path, method = 'GET', body = null) {
    try {
        const options = { method };
        if (body) {
            options.body = JSON.stringify(body);
            options.headers = { 'Content-Type': 'application/json' };
        }
        const res = await fetch(path, options);
        if (!res.ok) {
            const err = await res.json().catch(() => ({}));
            return { error: err.error || res.statusText };
        }
        return await res.json().catch(() => ({}));
    } catch (e) {
        return { error: e.message };
    }
}

async function loadTickets() {
    const res = await api('/api/tickets');
    if (res && res.error) {
        console.error('Failed to load tickets', res.error);
        return;
    }
    currentTickets = res || [];
    renderTicketList();
}

function renderTicketList() {
    const list = document.getElementById('ticketList');
    list.innerHTML = '';
    currentTickets.forEach(ticket => {
        const d = new Date(ticket.created_at * 1000).toLocaleString();
        const div = document.createElement('div');
        div.className = `ticket-item ${ticket.id === currentTicketId ? 'active' : ''}`;
        div.onclick = () => selectTicket(ticket);
        div.innerHTML = `
            <div class="ticket-title">${ticket.title}</div>
            <div class="ticket-meta">
                <span>#${ticket.id.substring(0, 8)}</span>
                <span class="status-badge status-${ticket.status}">${ticket.status}</span>
            </div>
            <div class="ticket-meta" style="margin-top:0.5rem; font-size:0.75rem">${d}</div>
        `;
        list.appendChild(div);
    });
}

async function selectTicket(ticket) {
    currentTicketId = ticket.id;
    renderTicketList();
    renderTicketDetails(ticket);
}

function renderTicketDetails(ticket) {
    const details = document.getElementById('ticketDetails');
    const d = new Date(ticket.created_at * 1000).toLocaleString();
    
    // In a real app we'd fetch the ticket again to get replies, but for now we assume it's in the list or we can reload.
    // Actually, our list API returns replies? Let's assume we need to reload the list to get replies.
    
    let repliesHtml = '';
    const replies = ticket.replies || [];
    replies.forEach(r => {
        const isAgent = r.author === 'agent';
        const rd = new Date(r.timestamp * 1000).toLocaleString();
        repliesHtml += `
            <div class="reply ${isAgent ? 'agent' : ''}">
                <div class="reply-header">
                    <span>${isAgent ? 'Support Agent' : 'Customer'}</span>
                    <span>${rd}</span>
                </div>
                <div class="reply-body">${r.text}</div>
            </div>
        `;
    });

    details.innerHTML = `
        <div class="ticket-header">
            <div class="ticket-info">
                <h2>${ticket.title}</h2>
                <div class="ticket-meta" style="margin-bottom: 1rem;">
                    <span>Created ${d}</span>
                    <span class="status-badge status-${ticket.status}">${ticket.status}</span>
                </div>
                <p>${ticket.description}</p>
            </div>
            ${ticket.status === 'open' ? `<button class="danger-btn" onclick="closeTicket('${ticket.id}')">Close Ticket</button>` : ''}
        </div>
        <div class="replies">
            ${repliesHtml}
        </div>
        ${ticket.status === 'open' ? `
        <div class="reply-form">
            <textarea id="replyText" rows="3" placeholder="Type your reply..."></textarea>
            <div class="reply-actions">
                <button class="ai-suggest" onclick="suggestReply('${ticket.id}')">✨ AI Suggest Reply</button>
                <button class="primary-btn" onclick="addReply('${ticket.id}')">Send Reply</button>
            </div>
        </div>
        ` : ''}
    `;
}

function showCreateModal() {
    document.getElementById('createModal').classList.add('active');
}

function hideCreateModal() {
    document.getElementById('createModal').classList.remove('active');
    document.getElementById('newTitle').value = '';
    document.getElementById('newDescription').value = '';
}

async function createTicket() {
    const title = document.getElementById('newTitle').value.trim();
    const description = document.getElementById('newDescription').value.trim();
    if (!title || !description) return;

    const res = await api('/api/tickets', 'POST', { title, description });
    if (res && res.error) {
        alert('Failed: ' + res.error);
    } else {
        hideCreateModal();
        await loadTickets();
    }
}

async function addReply(id) {
    const text = document.getElementById('replyText').value.trim();
    if (!text) return;

    const res = await api(`/api/tickets/${id}/reply`, 'POST', { text });
    if (res && res.error) {
        alert('Failed: ' + res.error);
    } else {
        // Find ticket and reload to get new replies. Actually we should just reload all.
        await loadTickets();
        const t = currentTickets.find(x => x.id === id);
        if (t) renderTicketDetails(t);
    }
}

async function closeTicket(id) {
    const res = await api(`/api/tickets/${id}/close`, 'POST');
    if (res && res.error) {
        alert('Failed: ' + res.error);
    } else {
        await loadTickets();
        const t = currentTickets.find(x => x.id === id);
        if (t) renderTicketDetails(t);
    }
}

async function suggestReply(id) {
    const btn = document.querySelector('.ai-suggest');
    const og = btn.innerHTML;
    btn.innerHTML = '✨ Thinking...';
    btn.disabled = true;

    const res = await api(`/api/tickets/${id}/suggest`, 'POST');
    
    btn.innerHTML = og;
    btn.disabled = false;

    if (res && res.error) {
        alert('AI Failed: ' + res.error);
    } else if (res && res.suggestion) {
        document.getElementById('replyText').value = res.suggestion.text;
    }
}

window.onload = loadTickets;
