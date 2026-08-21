import React, { useState, useEffect } from 'react';

function App() {
  const [posts, setPosts] = useState([]);

  useEffect(() => {
    fetch('/api/posts')
      .then(res => res.json())
      .then(data => setPosts(data))
      .catch(err => console.error(err));
  }, []);

  const handleLike = (id) => {
    fetch(`/api/posts/${id}/like`, { method: 'POST' })
      .then(res => res.json())
      .then(updatedPost => {
        setPosts(posts.map(p => p.id === updatedPost.id ? updatedPost : p));
      });
  };

  const createMockPost = () => {
    fetch('/api/posts', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        author_id: 'test_user',
        image_url: 'https://via.placeholder.com/150',
        caption: 'A cool post!',
      })
    })
      .then(res => res.json())
      .then(newPost => setPosts([...posts, newPost]));
  };

  return (
    <div style={{ maxWidth: '600px', margin: '0 auto', fontFamily: 'sans-serif' }}>
      <h1>Insta Clone Feed</h1>
      <button onClick={createMockPost} data-testid="create-post-btn">Create Post</button>
      <div style={{ marginTop: '20px' }}>
        {posts.map(post => (
          <div key={post.id} style={{ border: '1px solid #ccc', marginBottom: '20px', padding: '10px' }} className="post-container">
            <p><strong>{post.author_id}</strong></p>
            <img src={post.image_url} alt="Post" style={{ width: '100%', height: 'auto' }} />
            <p>{post.caption}</p>
            <p>Likes: <span className="like-count">{post.likes.length}</span></p>
            <button onClick={() => handleLike(post.id)} className="like-btn">Like</button>
          </div>
        ))}
      </div>
    </div>
  );
}

export default App;
