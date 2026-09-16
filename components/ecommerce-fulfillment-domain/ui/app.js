let currentToken = '';
let currentRole = '';

const API_BASE = '';

function showToast(message, isError = false) {
    const container = document.getElementById('toast-container');
    const toast = document.createElement('div');
    toast.className = 'toast';
    if (isError) toast.style.borderLeft = '4px solid var(--error)';
    else toast.style.borderLeft = '4px solid var(--success)';
    
    toast.innerText = message;
    container.appendChild(toast);
    
    setTimeout(() => {
        toast.style.opacity = '0';
        setTimeout(() => toast.remove(), 300);
    }, 3000);
}

document.getElementById('login-btn').addEventListener('click', async () => {
    const role = document.getElementById('role-select').value;
    const email = `${role}@nexus.test`;
    const password = 'password123';
    
    try {
        // Try to register first, ignore error if already exists
        await fetch(`${API_BASE}/register`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({
                email,
                password,
                role: role,
                tenant: "nexus"
            })
        });

        // Now login
        const res = await fetch(`${API_BASE}/login`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({
                email,
                password,
                tenant: "nexus"
            })
        });
        
        if (!res.ok) throw new Error('Authentication failed');
        const data = await res.json();
        
        currentToken = data.access_token;
        currentRole = role;
        
        document.getElementById('auth-view').classList.add('hidden');
        document.getElementById('auth-view').classList.remove('active');
        document.getElementById('dashboard-view').classList.remove('hidden');
        document.getElementById('user-role-badge').innerText = role.toUpperCase();
        
        if (role === 'customer') {
            document.getElementById('customer-section').classList.remove('hidden');
        } else {
            document.getElementById('fulfillment-section').classList.remove('hidden');
            loadOrders();
        }
        
        showToast(`Authenticated as ${role}`);
    } catch (err) {
        showToast(err.message, true);
    }
});

document.getElementById('logout-btn').addEventListener('click', () => {
    currentToken = '';
    currentRole = '';
    document.getElementById('dashboard-view').classList.add('hidden');
    document.getElementById('customer-section').classList.add('hidden');
    document.getElementById('fulfillment-section').classList.add('hidden');
    document.getElementById('auth-view').classList.remove('hidden');
    document.getElementById('auth-view').classList.add('active');
});

document.getElementById('buy-btn').addEventListener('click', async () => {
    const qty = parseInt(document.getElementById('qty-input').value);
    
    try {
        const res = await fetch(`${API_BASE}/api/orders`, {
            method: 'POST',
            headers: { 
                'Content-Type': 'application/json',
                'Authorization': `Bearer ${currentToken}`
            },
            body: JSON.stringify({
                product_id: 'quantum-keyboard',
                qty: qty,
                stripe_source: 'tok_visa'
            })
        });
        
        const data = await res.json();
        if (!res.ok) throw new Error(data.error || 'Purchase failed');
        
        showToast(`Order ${data.id.split('-')[0]} placed successfully!`);
    } catch (err) {
        showToast(err.message, true);
    }
});

document.getElementById('refresh-orders-btn').addEventListener('click', loadOrders);

async function loadOrders() {
    try {
        const res = await fetch(`${API_BASE}/api/orders`, {
            headers: { 'Authorization': `Bearer ${currentToken}` }
        });
        
        const data = await res.json();
        if (!res.ok) throw new Error(data.error || 'Failed to load orders');
        
        const container = document.getElementById('orders-list');
        container.innerHTML = '';
        
        if (data.items.length === 0) {
            container.innerHTML = '<p>No orders in queue.</p>';
            return;
        }
        
        data.items.forEach(order => {
            const card = document.createElement('div');
            card.className = 'order-card';
            
            card.innerHTML = `
                <span class="status ${order.status}">${order.status}</span>
                <h3 style="margin-bottom: 0.5rem">Order: ${order.id.split('-')[0]}</h3>
                <p>Product: ${order.product_id} x${order.qty}</p>
                <p>Customer: ${order.customer}</p>
                ${order.status === 'paid' ? `<button class="primary-btn fulfill-btn" data-id="${order.id}">Ship Order</button>` : ''}
            `;
            container.appendChild(card);
        });
        
        document.querySelectorAll('.fulfill-btn').forEach(btn => {
            btn.addEventListener('click', async (e) => {
                const id = e.target.getAttribute('data-id');
                await fulfillOrder(id);
            });
        });
    } catch (err) {
        showToast(err.message, true);
    }
}

async function fulfillOrder(id) {
    try {
        const res = await fetch(`${API_BASE}/api/orders/${id}/fulfill`, {
            method: 'POST',
            headers: { 'Authorization': `Bearer ${currentToken}` }
        });
        
        const data = await res.json();
        if (!res.ok) throw new Error(data.error || 'Fulfillment failed');
        
        showToast(`Order ${id.split('-')[0]} shipped!`);
        loadOrders();
    } catch (err) {
        showToast(err.message, true);
    }
}
