import React, { useState, useEffect } from 'react';
import { Heart, MessageCircle, Send, Bookmark, MoreHorizontal } from 'lucide-react';

function App() {
  const [token, setToken] = useState(localStorage.getItem('insta_token') || '');
  const [isLoggedIn, setIsLoggedIn] = useState(!!token);
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  
  const [posts, setPosts] = useState([]);

  useEffect(() => {
    fetch('/api/posts')
      .then(res => res.json())
      .then(data => setPosts(data || []))
      .catch(err => console.error(err));
  }, []);

  const handleLogin = (e) => {
    e.preventDefault();
    fetch('/api/login', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username, password })
    })
    .then(res => {
      if (!res.ok) throw new Error('Invalid credentials');
      return res.json();
    })
    .then(data => {
      setToken(data.token);
      localStorage.setItem('insta_token', data.token);
      setIsLoggedIn(true);
    })
    .catch(err => alert('Login failed: ' + err.message));
  };

  const handleLogout = () => {
    setToken('');
    localStorage.removeItem('insta_token');
    setIsLoggedIn(false);
  };

  const handleLike = (id) => {
    fetch(`/api/posts/${id}/like`, { 
      method: 'POST',
      headers: {
        'Authorization': token
      }
    })
      .then(res => {
        if (!res.ok) throw new Error('Unauthorized');
        return res.json();
      })
      .then(updatedPost => {
        setPosts(posts.map(p => p.id === updatedPost.id ? updatedPost : p));
      })
      .catch(err => alert("Please log in to like posts"));
  };

  const createMockPost = () => {
    fetch('/api/posts', {
      method: 'POST',
      headers: { 
        'Content-Type': 'application/json',
        'Authorization': token
      },
      body: JSON.stringify({
        author_id: username || 'test_user',
        image_url: `https://picsum.photos/seed/${Math.random()}/600/600`,
        caption: 'Loving this new clone! 🚀 #insta',
      })
    })
      .then(res => {
        if (!res.ok) throw new Error('Unauthorized');
        return res.json();
      })
      .then(newPost => setPosts([...posts, newPost]))
      .catch(err => alert("Please log in to create posts"));
  };

  if (!isLoggedIn) {
    return (
      <div className="min-h-screen bg-gray-50 flex flex-col justify-center py-12 sm:px-6 lg:px-8">
        <div className="sm:mx-auto sm:w-full sm:max-w-md">
          <h2 className="mt-6 text-center text-3xl font-extrabold text-gray-900 font-serif italic">Instaclone</h2>
        </div>
        <div className="mt-8 sm:mx-auto sm:w-full sm:max-w-md">
          <div className="bg-white py-8 px-4 shadow sm:rounded-lg sm:px-10 border border-gray-300">
            <form className="space-y-6" onSubmit={handleLogin}>
              <div>
                <label htmlFor="username" className="block text-sm font-medium text-gray-700">
                  Username
                </label>
                <div className="mt-1">
                  <input
                    id="username"
                    name="username"
                    type="text"
                    required
                    value={username}
                    onChange={(e) => setUsername(e.target.value)}
                    className="appearance-none block w-full px-3 py-2 border border-gray-300 rounded-md shadow-sm placeholder-gray-400 focus:outline-none focus:ring-blue-500 focus:border-blue-500 sm:text-sm"
                  />
                </div>
              </div>
              <div>
                <label htmlFor="password" className="block text-sm font-medium text-gray-700">
                  Password
                </label>
                <div className="mt-1">
                  <input
                    id="password"
                    name="password"
                    type="password"
                    required
                    value={password}
                    onChange={(e) => setPassword(e.target.value)}
                    className="appearance-none block w-full px-3 py-2 border border-gray-300 rounded-md shadow-sm placeholder-gray-400 focus:outline-none focus:ring-blue-500 focus:border-blue-500 sm:text-sm"
                  />
                </div>
              </div>
              <div>
                <button
                  type="submit"
                  className="w-full flex justify-center py-2 px-4 border border-transparent rounded-md shadow-sm text-sm font-medium text-white bg-blue-500 hover:bg-blue-600 focus:outline-none focus:ring-2 focus:ring-offset-2 focus:ring-blue-500"
                >
                  Log in
                </button>
              </div>
            </form>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="min-h-screen bg-gray-50 pb-10">
      <nav className="bg-white border-b border-gray-300 sticky top-0 z-10">
        <div className="max-w-4xl mx-auto px-4 sm:px-6 lg:px-8">
          <div className="flex justify-between h-16 items-center">
            <div className="flex-shrink-0 flex items-center">
              <span className="font-serif italic text-2xl font-bold">Instaclone</span>
            </div>
            <div className="flex items-center space-x-4">
              <button onClick={createMockPost} data-testid="create-post-btn" className="bg-blue-500 text-white px-4 py-1.5 rounded text-sm font-medium hover:bg-blue-600">
                Create Post
              </button>
              <button onClick={handleLogout} className="text-gray-600 hover:text-gray-900 text-sm font-medium">
                Logout
              </button>
            </div>
          </div>
        </div>
      </nav>

      <main className="max-w-lg mx-auto mt-8 px-4">
        <div className="space-y-8">
          {posts.map(post => (
            <article key={post.id} className="bg-white border border-gray-300 rounded-lg post-container">
              {/* Post Header */}
              <div className="flex items-center justify-between p-3">
                <div className="flex items-center space-x-3">
                  <div className="w-8 h-8 bg-gradient-to-tr from-yellow-400 to-fuchsia-600 p-[2px] rounded-full">
                    <div className="bg-white p-[2px] rounded-full h-full w-full">
                      <img className="rounded-full w-full h-full object-cover" src={`https://ui-avatars.com/api/?name=${post.author_id}&background=random`} alt={post.author_id} />
                    </div>
                  </div>
                  <span className="font-semibold text-sm">{post.author_id}</span>
                </div>
                <MoreHorizontal className="text-gray-500 w-5 h-5 cursor-pointer" />
              </div>

              {/* Post Image */}
              <div className="relative pb-[100%] bg-gray-100">
                <img src={post.image_url} alt="Post content" className="absolute top-0 left-0 w-full h-full object-cover" />
              </div>

              {/* Post Actions */}
              <div className="p-3">
                <div className="flex justify-between mb-2">
                  <div className="flex space-x-4">
                    <button onClick={() => handleLike(post.id)} className="like-btn focus:outline-none transition-transform active:scale-95">
                      <Heart className={`w-6 h-6 ${post.likes?.includes(token.replace('bearer mock_token_for_', '')) ? 'fill-red-500 text-red-500' : 'text-gray-800'}`} />
                    </button>
                    <MessageCircle className="w-6 h-6 text-gray-800 cursor-pointer" />
                    <Send className="w-6 h-6 text-gray-800 cursor-pointer" />
                  </div>
                  <Bookmark className="w-6 h-6 text-gray-800 cursor-pointer" />
                </div>
                <div className="font-semibold text-sm mb-1">
                  <span className="like-count">{post.likes?.length || 0}</span> likes
                </div>
                <div className="text-sm">
                  <span className="font-semibold mr-2">{post.author_id}</span>
                  {post.caption}
                </div>
              </div>
            </article>
          ))}
          {posts.length === 0 && (
            <div className="text-center py-10 text-gray-500">
              No posts yet. Be the first to post!
            </div>
          )}
        </div>
      </main>
    </div>
  );
}

export default App;
