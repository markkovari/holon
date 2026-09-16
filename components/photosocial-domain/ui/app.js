let token = localStorage.getItem('ps_token') || '';
    let currentUser = JSON.parse(localStorage.getItem('ps_user') || 'null');
    let activeAttributes = [];
    let currentPhotoId = null;
    let uploadedFileDataUrl = '';

    async function api(path, method = 'GET', body = null) {
      const headers = { 'content-type': 'application/json' };
      if (token) headers['authorization'] = `Bearer ${token}`;
      const res = await fetch(path, {
        method,
        headers,
        body: body ? JSON.stringify(body) : null
      });
      return res.json().catch(() => ({}));
    }

    async function init() {
      setupDropzone();
      await refreshMe();
      await loadAttributes();
      await loadPhotos();
    }

    function setupDropzone() {
      const dz = document.getElementById('dropzone');
      if (!dz) return;
      ['dragenter', 'dragover'].forEach(name => {
        dz.addEventListener(name, (e) => {
          e.preventDefault();
          e.stopPropagation();
          dz.classList.add('dragover');
        });
      });
      ['dragleave', 'drop'].forEach(name => {
        dz.addEventListener(name, (e) => {
          e.preventDefault();
          e.stopPropagation();
          dz.classList.remove('dragover');
        });
      });
      dz.addEventListener('drop', (e) => {
        const files = e.dataTransfer.files;
        if (files && files.length > 0) {
          processImageFile(files[0]);
        }
      });
    }

    function handleFileSelected(event) {
      const file = event.target.files && event.target.files[0];
      if (file) {
        processImageFile(file);
      }
    }

    function processImageFile(file) {
      if (!file.type.startsWith('image/')) {
        return alert('Please select a valid image file (JPEG, PNG, WebP, etc.)');
      }
      
      const titleInput = document.getElementById('uploadTitle');
      if (!titleInput.value || titleInput.value === 'Midnight Reflections') {
        const cleanName = file.name.replace(/\.[^/.]+$/, '').replace(/[-_]/g, ' ');
        titleInput.value = cleanName.charAt(0).toUpperCase() + cleanName.slice(1);
      }

      const reader = new FileReader();
      reader.onload = (e) => {
        const img = new Image();
        img.onload = () => {
          let width = img.width;
          let height = img.height;
          const maxDim = 1920;
          
          if (width > maxDim || height > maxDim) {
            if (width > height) {
              height = Math.round((height * maxDim) / width);
              width = maxDim;
            } else {
              width = Math.round((width * maxDim) / height);
              height = maxDim;
            }
          }
          
          const canvas = document.createElement('canvas');
          canvas.width = width;
          canvas.height = height;
          const ctx = canvas.getContext('2d');
          ctx.drawImage(img, 0, 0, width, height);
          
          // Downscale to JPEG (quality 0.82)
          uploadedFileDataUrl = canvas.toDataURL('image/jpeg', 0.82);
          
          document.getElementById('previewImg').src = uploadedFileDataUrl;
          document.getElementById('filePreview').style.display = 'block';
          document.getElementById('dropzone').style.display = 'none';
        };
        img.src = e.target.result;
      };
      reader.readAsDataURL(file);
    }

    function clearSelectedFile(event) {
      if (event) {
        event.preventDefault();
        event.stopPropagation();
      }
      uploadedFileDataUrl = '';
      document.getElementById('uploadFileInput').value = '';
      document.getElementById('previewImg').src = '';
      document.getElementById('filePreview').style.display = 'none';
      document.getElementById('dropzone').style.display = 'flex';
    }

    function handleUrlInput() {
      if (uploadedFileDataUrl) {
        clearSelectedFile();
      }
    }

    async function refreshMe() {
      if (token) {
        const me = await api('/api/me');
        if (me.subject) {
          currentUser = me;
          document.getElementById('userName').innerText = me.subject.split('@')[0];
          document.getElementById('userRole').innerText = me.is_admin ? 'admin' : 'creator';
          document.getElementById('userRole').className = 'role-badge ' + (me.is_admin ? 'role-admin' : 'role-user');
          document.getElementById('authBtn').innerText = 'Switch User';
          if (me.is_admin) {
            document.getElementById('adminBtn').style.display = 'inline-flex';
          } else {
            document.getElementById('adminBtn').style.display = 'none';
          }
          return;
        }
      }
      handleAuth('login', true);
    }

    async function handleAuth(action, quiet = false) {
      const email = document.getElementById('authEmail').value;
      const password = document.getElementById('authPassword').value;
      const role = document.getElementById('authRole').value;

      let res = await api(`/api/${action}`, 'POST', { email, password, role });
      if (action === 'register' && res.subject) {
        res = await api('/api/login', 'POST', { email, password });
      } else if (action === 'login' && res.error && quiet) {
        await api('/api/register', 'POST', { email, password, role });
        res = await api('/api/login', 'POST', { email, password });
      }

      if (res.access_token) {
        token = res.access_token;
        localStorage.setItem('ps_token', token);
        closeModal('authModal');
        await refreshMe();
        await loadAttributes();
        await loadPhotos();
      } else if (!quiet && res.error) {
        alert('Auth error: ' + res.error);
      }
    }

    async function loadAttributes() {
      activeAttributes = await api('/api/attributes');
      if (!Array.isArray(activeAttributes)) activeAttributes = [];
      renderAdminAttributes();
    }

    function renderAdminAttributes() {
      const list = document.getElementById('adminAttrList');
      if (!list) return;
      list.innerHTML = activeAttributes.map(a => `
        <div class="admin-attr-item">
          <div>
            <strong>${a.name}</strong>
            <div style="font-size: 0.78rem; color: var(--text-muted);">${a.description || ''}</div>
          </div>
          <button class="btn-secondary" style="padding: 0.25rem 0.5rem; color: #f43f5e;" onclick="deleteAttribute('${a.id || a.record_id}')">Delete</button>
        </div>
      `).join('');
    }

    async function submitNewAttribute() {
      const name = document.getElementById('newAttrName').value.trim();
      const description = document.getElementById('newAttrDesc').value.trim();
      if (!name) return alert('Enter attribute name');
      const res = await api('/api/admin/attributes', 'POST', { name, description });
      if (res.error) return alert(res.error);
      document.getElementById('newAttrName').value = '';
      document.getElementById('newAttrDesc').value = '';
      await loadAttributes();
    }

    async function deleteAttribute(id) {
      if (!confirm('Remove this scoring attribute?')) return;
      await api(`/api/admin/attributes/${id}`, 'DELETE');
      await loadAttributes();
    }

    async function loadPhotos(sort = 'latest', tabEl = null) {
      if (tabEl) {
        document.querySelectorAll('.feed-tab').forEach(t => t.classList.remove('active'));
        tabEl.classList.add('active');
      }
      let photos = await api(`/api/photos?sort=${sort}`);
      if (!Array.isArray(photos)) photos = [];
      
      const grid = document.getElementById('galleryGrid');
      grid.innerHTML = photos.map(p => {
        const scores = p.attribute_scores || {};
        const scoreKeys = Object.keys(scores).slice(0, 3);
        const scoreBadges = scoreKeys.map(k => `
          <div>
            <div class="attr-stat-label">${k}</div>
            <div class="attr-stat-val">${scores[k].avg || '—'}</div>
          </div>
        `).join('');

        const imgSrc = p.image_data || p.image_url;

        return `
          <div class="photo-card" id="card_${p.id}">
            <div class="photo-img-wrapper" onclick="openPhotoModal('${p.id}')">
              <img src="${imgSrc}" alt="${p.title}">
              <div class="ai-badge">✨ AI Critiqued</div>
            </div>
            <div class="card-body">
              <div class="photo-title" onclick="openPhotoModal('${p.id}')">${p.title}</div>
              <div class="photo-author">by ${p.author_name || 'Artist'}</div>
              <div class="ai-narrative-preview">${p.ai_narrative || p.description || ''}</div>
              
              <div class="attributes-breakdown">
                ${scoreBadges || '<div style="grid-column: 1/-1; font-size: 0.75rem; color: var(--text-muted);">Rate first attributes in details</div>'}
              </div>

              <div class="card-footer">
                <div class="vote-widget">
                  <button class="vote-btn" onclick="votePhoto('${p.id}', 1, this)">▲</button>
                  <span class="score-count" id="score_${p.id}">${p.score || 0}</span>
                  <button class="vote-btn" onclick="votePhoto('${p.id}', -1, this)">▼</button>
                </div>
                <button class="btn-secondary" style="font-size: 0.78rem; padding: 0.35rem 0.65rem;" onclick="openPhotoModal('${p.id}')">Review & Rate →</button>
              </div>
            </div>
          </div>
        `;
      }).join('');
    }

    async function votePhoto(id, val, btn) {
      const res = await api(`/api/photos/${id}/vote`, 'POST', { value: val });
      if (res.score !== undefined) {
        document.getElementById(`score_${id}`).innerText = res.score;
      }
    }

    async function openPhotoModal(id) {
      currentPhotoId = id;
      const photo = await api(`/api/photos/${id}`);
      const myRatings = await api(`/api/photos/${id}/my-ratings`);
      
      document.getElementById('modalTitle').innerText = photo.title;
      document.getElementById('modalImg').src = photo.image_data || photo.image_url;
      document.getElementById('modalAiNarrative').innerText = photo.ai_narrative || '';
      document.getElementById('modalAiCritique').innerText = photo.ai_critique || 'AI critique in progress...';

      const sliders = document.getElementById('ratingSliders');
      sliders.innerHTML = activeAttributes.map(a => {
        const userScore = myRatings.ratings ? (myRatings.ratings[a.id] || 8) : 8;
        return `
          <div class="rating-row">
            <div class="rating-label">
              ${a.name}
              <div style="font-size: 0.72rem; color: var(--text-muted); font-weight: normal;">${a.description || ''}</div>
            </div>
            <input type="range" class="rating-slider" min="1" max="10" step="0.5" value="${userScore}" 
                   id="slider_${a.id}" oninput="document.getElementById('val_${a.id}').innerText = this.value">
            <div class="rating-val" id="val_${a.id}">${userScore}</div>
          </div>
        `;
      }).join('');

      document.getElementById('photoModal').style.display = 'flex';
    }

    async function submitRatings() {
      if (!currentPhotoId) return;
      const ratings = activeAttributes.map(a => ({
        attribute_id: a.id,
        score: parseFloat(document.getElementById(`slider_${a.id}`).value)
      }));
      await api(`/api/photos/${currentPhotoId}/rate`, 'POST', { ratings });
      closeModal('photoModal');
      await loadPhotos();
    }

    async function submitPhoto() {
      const title = document.getElementById('uploadTitle').value.trim();
      const image_url = uploadedFileDataUrl ? '' : document.getElementById('uploadImgUrl').value.trim();
      const image_data = uploadedFileDataUrl || '';
      const description = document.getElementById('uploadDesc').value.trim();

      if (!title) return alert('Please enter a photo title');
      if (!image_data && !image_url) return alert('Please select a photo file or enter an image URL');

      const submitBtn = document.getElementById('uploadSubmitBtn');
      const originalText = submitBtn.innerText;
      submitBtn.innerText = 'Analyzing & Uploading...';
      submitBtn.disabled = true;

      try {
        const res = await api('/api/photos', 'POST', { title, image_url, image_data, description });
        if (res.error) return alert(res.error);
        clearSelectedFile();
        closeModal('uploadModal');
        await loadPhotos();
      } finally {
        submitBtn.innerText = originalText;
        submitBtn.disabled = false;
      }
    }

    function toggleAuthModal() { document.getElementById('authModal').style.display = 'flex'; }
    function openUploadModal() { document.getElementById('uploadModal').style.display = 'flex'; }
    function openAdminModal() { document.getElementById('adminModal').style.display = 'flex'; }
    function closeModal(id) { document.getElementById(id).style.display = 'none'; }

    init();