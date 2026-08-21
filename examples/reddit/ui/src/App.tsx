import React, { useState } from 'react';
import { MessageSquare, ArrowUp, ArrowDown, LogOut } from 'lucide-react';

function App() {
  const [token, setToken] = useState<string | null>(null);
  const [username, setUsername] = useState('');

  if (!token) {
    return (
      <div className="min-h-screen bg-gray-100 flex items-center justify-center">
        <div className="bg-white p-8 rounded-lg shadow-md w-96">
          <h1 className="text-2xl font-bold text-center mb-6 text-orange-600">Reddit Clone Login</h1>
          <input
            className="w-full border rounded px-3 py-2 mb-4"
            type="text"
            placeholder="Username"
            value={username}
            onChange={(e) => setUsername(e.target.value)}
          />
          <button
            className="w-full bg-orange-500 text-white rounded py-2 hover:bg-orange-600 font-bold"
            onClick={() => setToken(`mock_token_${username}`)}
          >
            Log In
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="min-h-screen bg-gray-100">
      <header className="bg-white shadow-sm sticky top-0">
        <div className="max-w-4xl mx-auto px-4 py-3 flex justify-between items-center">
          <div className="text-xl font-bold text-orange-600 flex items-center gap-2">
            <div className="bg-orange-500 rounded-full w-8 h-8 flex items-center justify-center text-white">R</div>
            RedditClone
          </div>
          <div className="flex items-center gap-4">
            <span className="text-gray-600 text-sm">Logged in as {username}</span>
            <button
              onClick={() => { setToken(null); setUsername(''); }}
              className="text-gray-500 hover:text-gray-800"
            >
              <LogOut size={20} />
            </button>
          </div>
        </div>
      </header>
      
      <main className="max-w-4xl mx-auto px-4 py-6">
        <div className="bg-white rounded shadow p-4 mb-4">
          <div className="flex gap-4">
            <div className="flex flex-col items-center gap-1 text-gray-400">
              <button className="hover:text-orange-500"><ArrowUp size={24} /></button>
              <span className="font-bold text-black">12k</span>
              <button className="hover:text-blue-500"><ArrowDown size={24} /></button>
            </div>
            <div className="flex-1">
              <div className="text-xs text-gray-500 mb-1">Posted by u/user1 5 hours ago</div>
              <h2 className="text-xl font-bold mb-2">This is a fully styled mockup thread!</h2>
              <p className="text-gray-700 mb-4">Since we updated the backend to use the KeyValue store and Auth module, our application is now fully upgraded!</p>
              <div className="flex items-center gap-2 text-gray-500 text-sm font-bold">
                <button className="flex items-center gap-1 hover:bg-gray-100 p-1 rounded">
                  <MessageSquare size={16} /> 123 Comments
                </button>
              </div>
            </div>
          </div>
        </div>
      </main>
    </div>
  );
}

export default App;
