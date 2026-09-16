const API_BASE = '/api';
let currentToken = null;
let currentRole = null;

const authOverlay = document.getElementById('auth-overlay');
const userDisplay = document.getElementById('user-display');
const logoutBtn = document.getElementById('logout-btn');
const libActions = document.getElementById('librarian-actions');
const createBookForm = document.getElementById('create-book-form');
const booksList = document.getElementById('books-list');
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
        
        authOverlay.classList.add('hidden');
        userDisplay.innerText = `Logged in as ${subject}`;
        showToast(`Authenticated as ${subject}`);
        
        if (currentRole === 'librarian') {
            libActions.classList.remove('hidden');
        } else {
            libActions.classList.add('hidden');
        }
        
        loadBooks();
    } catch (e) {
        showToast(e.message);
    }
}

document.getElementById('login-lib').addEventListener('click', () => 
    login('lib@example.test', ['librarian'], ['books:write', 'books:read', 'loans:write', 'loans:return'])
);
document.getElementById('login-alice').addEventListener('click', () => 
    login('alice@example.test', ['patron'], ['books:read', 'loans:write', 'loans:return'])
);
document.getElementById('login-bob').addEventListener('click', () => 
    login('bob@example.test', ['patron'], ['books:read', 'loans:write', 'loans:return'])
);

logoutBtn.addEventListener('click', () => {
    currentToken = null;
    currentRole = null;
    authOverlay.classList.remove('hidden');
    libActions.classList.add('hidden');
    booksList.innerHTML = '';
});

// Helper for local state
function getBorrowed() {
    return JSON.parse(localStorage.getItem('borrowed_books') || '{}');
}
function setBorrowed(map) {
    localStorage.setItem('borrowed_books', JSON.stringify(map));
}

// Load Books
async function loadBooks() {
    if (!currentToken) return;
    try {
        const res = await fetch(`${API_BASE}/books?t=${Date.now()}`, {
            headers: { 'Authorization': `Bearer ${currentToken}` }
        });
        
        let books = [];
        if (res.ok) {
            const data = await res.json();
            books = data.books || [];
        } else {
            books = JSON.parse(localStorage.getItem('books') || '[]');
        }
        
        displayBooks(books);
    } catch (e) {
        console.error(e);
        const books = JSON.parse(localStorage.getItem('books') || '[]');
        displayBooks(books);
    }
}

function displayBooks(books) {
    const borrowed = getBorrowed();
    booksList.innerHTML = '';
    if (books.length === 0) {
        booksList.innerHTML = '<p>No books in catalog.</p>';
        return;
    }

    books.forEach(book => {
        const loanId = borrowed[book.id];
        const isBorrowed = !!loanId;
        
        const card = document.createElement('div');
        card.className = 'book-card';
        card.innerHTML = `
            <div class="book-title">${book.title}</div>
            <div class="book-callnum">${book.call_number}</div>
            <div class="book-status ${isBorrowed ? 'status-borrowed' : 'status-available'}">
                ${isBorrowed ? 'Borrowed' : 'Available'}
            </div>
            <div class="book-actions">
                ${!isBorrowed
                    ? `<button class="primary-btn borrow-btn" data-id="${book.id}">Borrow</button>`
                    : `<button class="secondary-btn return-btn" data-loan="${loanId}" data-book="${book.id}">Return</button>`
                }
            </div>
        `;
        
        const borrowBtn = card.querySelector('.borrow-btn');
        const returnBtn = card.querySelector('.return-btn');
        
        if (borrowBtn) {
            borrowBtn.addEventListener('click', () => borrowBook(book.id));
        }
        if (returnBtn) {
            returnBtn.addEventListener('click', () => returnBook(loanId, book.id));
        }
        
        booksList.appendChild(card);
    });
}

// Create Book
createBookForm.addEventListener('submit', async (e) => {
    e.preventDefault();
    const title = document.getElementById('book-title').value;
    
    try {
        const res = await fetch(`${API_BASE}/books`, {
            method: 'POST',
            headers: { 
                'Authorization': `Bearer ${currentToken}`,
                'Content-Type': 'application/json'
            },
            body: JSON.stringify({ title })
        });
        
        if (res.status === 403) {
            showToast("Access Denied: Only librarians can add books.");
            return;
        }
        if (!res.ok) throw new Error('Failed to create book');
        
        const data = await res.json();
        showToast('Book added successfully!');
        createBookForm.reset();
        
        // Add to local state for mock display
        const localBooks = JSON.parse(localStorage.getItem('books') || '[]');
        localBooks.push({ id: data.id, title, call_number: data.call_number || 'BK-123', status: 'available', loan_id: null });
        localStorage.setItem('books', JSON.stringify(localBooks));
        
        loadBooks();
    } catch (err) {
        showToast(err.message);
    }
});

// Borrow Book
async function borrowBook(bookId) {
    try {
        const res = await fetch(`${API_BASE}/books/${bookId}/borrow`, {
            method: 'POST',
            headers: { 'Authorization': `Bearer ${currentToken}` }
        });
        
        if (res.status === 409) {
            showToast("Conflict: Book is already borrowed.");
            return;
        }
        if (!res.ok) throw new Error('Failed to borrow book');
        const data = await res.json();
        
        showToast('Book borrowed!');
        
        const borrowed = getBorrowed();
        borrowed[bookId] = data.id;
        setBorrowed(borrowed);
        
        loadBooks();
    } catch (err) {
        showToast(err.message);
    }
}

// Return Book
async function returnBook(loanId, bookId) {
    try {
        const res = await fetch(`${API_BASE}/loans/${loanId}/return`, {
            method: 'POST',
            headers: { 'Authorization': `Bearer ${currentToken}` }
        });
        
        if (res.status === 403) {
            showToast("Access Denied: You cannot return someone else's loan.");
            return;
        }
        if (!res.ok) throw new Error('Failed to return book');
        
        showToast('Book returned successfully!');
        
        const borrowed = getBorrowed();
        delete borrowed[bookId];
        setBorrowed(borrowed);
        
        loadBooks();
    } catch (err) {
        showToast(err.message);
    }
}

refreshBtn.addEventListener('click', loadBooks);

// Clear local storage on load so tests are fresh only if no query param
if (window.location.search.includes('clear=1')) {
    localStorage.removeItem('books');
    localStorage.removeItem('borrowed_books');
}
